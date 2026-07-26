use crate::{
    client::ClientManager,
    config::local_commit,
    observability::{self, Timestamp},
    peer::PeerCapabilities,
};
use lan_mouse_ipc::{ClientHandle, DEFAULT_PORT};
use lan_mouse_proto::{
    CAPABILITY_CLIPBOARD_TEXT, MAX_EVENT_SIZE, MAX_WIRE_SIZE, ProtoEvent, ProtocolError, WireEvent,
    decode_wire_event, encode_clipboard_text,
};
use local_channel::mpsc::{Receiver, Sender, channel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
#[cfg(feature = "metrics")]
use std::{collections::VecDeque, time::Instant};
use thiserror::Error;
use tokio::{
    net::UdpSocket,
    sync::Mutex,
    task::{JoinSet, spawn_local},
};
use webrtc_dtls::{
    config::{Config, ExtendedMasterSecretType},
    conn::DTLSConn,
    crypto::Certificate,
};
use webrtc_util::Conn;

#[derive(Debug, Error)]
pub(crate) enum ConnectionError {
    #[error(transparent)]
    Bind(#[from] io::Error),
    #[error(transparent)]
    Dtls(#[from] webrtc_dtls::Error),
    #[error(transparent)]
    Webrtc(#[from] webrtc_util::Error),
    #[error("not connected")]
    NotConnected,
    #[error("emulation is disabled on the target device")]
    TargetEmulationDisabled,
    #[error("clipboard synchronization is not supported by the target device")]
    ClipboardUnsupported,
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("Connection timed out")]
    Timeout,
}

const DEFAULT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// number of pings sent per liveness round; at least one must be answered
/// before the round ends or the connection is closed
const PINGS_PER_ROUND: usize = 4;

/// delay between two pings of the same round
const PING_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Default)]
struct PingState {
    responses: HashSet<SocketAddr>,
    #[cfg(feature = "metrics")]
    outstanding: HashMap<SocketAddr, VecDeque<Instant>>,
}

impl PingState {
    fn sent(&mut self, _addr: SocketAddr) {
        #[cfg(feature = "metrics")]
        self.outstanding
            .entry(_addr)
            .or_default()
            .push_back(Instant::now());
    }

    fn response(&mut self, addr: SocketAddr) {
        self.responses.insert(addr);
        #[cfg(feature = "metrics")]
        if let Some(sent_at) = self
            .outstanding
            .get_mut(&addr)
            .and_then(VecDeque::pop_front)
        {
            observability::record_rtt(sent_at.elapsed());
        }
    }

    fn take_response(&mut self, addr: SocketAddr) -> bool {
        #[cfg(feature = "metrics")]
        self.outstanding.remove(&addr);
        self.responses.remove(&addr)
    }
}

async fn connect(
    addr: SocketAddr,
    cert: Certificate,
) -> Result<(Arc<dyn Conn + Sync + Send>, SocketAddr), (SocketAddr, ConnectionError)> {
    log::info!("connecting to {addr} ...");
    let conn = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| (addr, e.into()))?,
    );
    conn.connect(addr).await.map_err(|e| (addr, e.into()))?;
    let config = Config {
        certificates: vec![cert],
        server_name: "ignored".to_owned(),
        insecure_skip_verify: true,
        extended_master_secret: ExtendedMasterSecretType::Require,
        ..Default::default()
    };
    let timeout = tokio::time::sleep(DEFAULT_CONNECTION_TIMEOUT);
    tokio::select! {
        _ = timeout => Err((addr, ConnectionError::Timeout)),
        result = DTLSConn::new(conn, config, true, None) => match result {
            Ok(dtls_conn) => Ok((Arc::new(dtls_conn), addr)),
            Err(e) => Err((addr, e.into())),
        }
    }
}

async fn connect_any(
    addrs: &[SocketAddr],
    cert: Certificate,
) -> Result<(Arc<dyn Conn + Send + Sync>, SocketAddr), ConnectionError> {
    let mut joinset = JoinSet::new();
    for &addr in addrs {
        joinset.spawn_local(connect(addr, cert.clone()));
    }
    loop {
        match joinset.join_next().await {
            None => return Err(ConnectionError::NotConnected),
            Some(r) => match r.expect("join error") {
                Ok(conn) => return Ok(conn),
                Err((a, e)) => {
                    log::warn!("failed to connect to {a}: `{e}`")
                }
            },
        };
    }
}

pub(crate) struct Connection {
    cert: Certificate,
    connecting: Rc<Mutex<HashSet<ClientHandle>>>,
    recv_rx: Receiver<(ClientHandle, WireEvent)>,
    ctx: ConnectionContext,
}

/// State shared between [`Connection`] and the per-connection tasks it spawns.
#[derive(Clone)]
struct ConnectionContext {
    client_manager: ClientManager,
    conns: Rc<Mutex<HashMap<SocketAddr, Arc<dyn Conn + Send + Sync>>>>,
    recv_tx: Sender<(ClientHandle, WireEvent)>,
    ping_state: Rc<RefCell<PingState>>,
    peer_capabilities: PeerCapabilities,
}

impl ConnectionContext {
    /// look up the open connection to `addr`, if any
    async fn conn(&self, addr: SocketAddr) -> Option<Arc<dyn Conn + Send + Sync>> {
        self.conns.lock().await.get(&addr).cloned()
    }

    /// forget the connection to `addr` and everything we learned about it
    async fn disconnect(&self, handle: ClientHandle, addr: SocketAddr) {
        log::warn!("client ({handle}) @ {addr} connection closed");
        let active: Vec<SocketAddr> = {
            let mut conns = self.conns.lock().await;
            conns.remove(&addr);
            conns.keys().copied().collect()
        };
        self.client_manager.set_active_addr(handle, None);
        self.client_manager.set_peer_commit(handle, None);
        self.peer_capabilities.remove(addr);
        log::info!("active connections: {active:?}");
    }
}

impl Connection {
    pub(crate) fn new(cert: Certificate, client_manager: ClientManager) -> Self {
        let (recv_tx, recv_rx) = channel();
        Self {
            cert,
            connecting: Default::default(),
            recv_rx,
            ctx: ConnectionContext {
                client_manager,
                conns: Default::default(),
                recv_tx,
                ping_state: Default::default(),
                peer_capabilities: Default::default(),
            },
        }
    }

    pub(crate) async fn recv(&mut self) -> (ClientHandle, WireEvent) {
        self.recv_rx.recv().await.expect("channel closed")
    }

    pub(crate) async fn send(
        &self,
        event: ProtoEvent,
        handle: ClientHandle,
    ) -> Result<(), ConnectionError> {
        let serialization_started = Timestamp::now();
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        observability::record_serialization(serialization_started);
        let buf = &buf[..len];
        if let Some(addr) = self.ctx.client_manager.active_addr(handle) {
            if let Some(conn) = self.ctx.conn(addr).await {
                if !self.ctx.client_manager.alive(handle) {
                    return Err(ConnectionError::TargetEmulationDisabled);
                }
                match conn.send(buf).await {
                    Ok(_) => {
                        observability::record_sent(&event);
                        log::trace!("{event} >->->->->- {addr}");
                        return Ok(());
                    }
                    Err(e) => {
                        // The caller releases the capture when a send
                        // fails; reporting success here would strand the
                        // pointer on a peer we can no longer reach.
                        log::warn!("client {handle} failed to send: {e}");
                        self.ctx.disconnect(handle, addr).await;
                        return Err(e.into());
                    }
                }
            }
        }

        // check if we are already trying to connect
        let mut connecting = self.connecting.lock().await;
        if !connecting.contains(&handle) {
            connecting.insert(handle);
            // connect in the background
            spawn_local(connect_to_handle(
                self.cert.clone(),
                handle,
                self.connecting.clone(),
                self.ctx.clone(),
            ));
        }
        Err(ConnectionError::NotConnected)
    }

    pub(crate) async fn send_clipboard(
        &self,
        text: &str,
        handle: ClientHandle,
    ) -> Result<(), ConnectionError> {
        let Some(addr) = self.ctx.client_manager.active_addr(handle) else {
            return Err(ConnectionError::NotConnected);
        };
        if !self
            .ctx
            .peer_capabilities
            .supports(addr, CAPABILITY_CLIPBOARD_TEXT)
        {
            return Err(ConnectionError::ClipboardUnsupported);
        }
        let conn = self
            .ctx
            .conn(addr)
            .await
            .ok_or(ConnectionError::NotConnected)?;
        let packet = encode_clipboard_text(text)?;
        if let Err(error) = conn.send(&packet).await {
            log::warn!("client {handle} failed to send clipboard text: {error}");
            self.ctx.disconnect(handle, addr).await;
            return Err(error.into());
        }
        log::debug!("sent clipboard text to {addr} ({} bytes)", text.len());
        Ok(())
    }

    pub(crate) async fn broadcast_clipboard(&self, text: &str) {
        let packet = match encode_clipboard_text(text) {
            Ok(packet) => packet,
            Err(error) => {
                log::warn!("clipboard text was not broadcast: {error}");
                return;
            }
        };
        let conns = {
            let conns = self.ctx.conns.lock().await;
            conns
                .iter()
                .map(|(addr, conn)| (*addr, conn.clone()))
                .collect::<Vec<_>>()
        };
        for (addr, conn) in conns {
            if !self
                .ctx
                .peer_capabilities
                .supports(addr, CAPABILITY_CLIPBOARD_TEXT)
            {
                continue;
            }
            if let Err(error) = conn.send(&packet).await {
                log::warn!("failed to send clipboard text to {addr}: {error}");
                if let Some(handle) = self.ctx.client_manager.get_client(addr) {
                    self.ctx.disconnect(handle, addr).await;
                }
            }
        }
    }
}

async fn connect_to_handle(
    cert: Certificate,
    handle: ClientHandle,
    connecting: Rc<Mutex<HashSet<ClientHandle>>>,
    context: ConnectionContext,
) -> Result<(), ConnectionError> {
    log::info!("client {handle} connecting ...");
    // sending did not work, figure out active conn.
    if let Some(addrs) = context.client_manager.get_ips(handle) {
        let port = context
            .client_manager
            .get_port(handle)
            .unwrap_or(DEFAULT_PORT);
        let addrs = addrs
            .into_iter()
            .map(|a| SocketAddr::new(a, port))
            .collect::<Vec<_>>();
        log::info!("client ({handle}) connecting ... (ips: {addrs:?})");
        let res = connect_any(&addrs, cert).await;
        let (conn, addr) = match res {
            Ok(c) => c,
            Err(e) => {
                connecting.lock().await.remove(&handle);
                return Err(e);
            }
        };
        log::info!("client ({handle}) connected @ {addr}");
        context.client_manager.set_active_addr(handle, Some(addr));
        context.conns.lock().await.insert(addr, conn.clone());
        connecting.lock().await.remove(&handle);

        // Best-effort version handshake. Send our commit hash once
        // immediately after the DTLS handshake; the listen side
        // mirrors a Hello back so the receive loop can populate
        // `peer_commit`. Old peers will silently skip this event
        // per the forward-compat handler in [`receive_loop`].
        let (buf, len) = ProtoEvent::Hello {
            commit: local_commit(),
            capabilities: CAPABILITY_CLIPBOARD_TEXT,
        }
        .into();
        if let Err(e) = conn.send(&buf[..len]).await {
            log::debug!("hello send to {addr} failed: {e}");
        }

        // poll connection for active
        spawn_local(ping_pong(addr, conn.clone(), context.ping_state.clone()));

        // receiver
        spawn_local(receive_loop(handle, addr, conn, context));
        return Ok(());
    }
    connecting.lock().await.remove(&handle);
    Err(ConnectionError::NotConnected)
}

async fn ping_pong(
    addr: SocketAddr,
    conn: Arc<dyn Conn + Send + Sync>,
    ping_state: Rc<RefCell<PingState>>,
) {
    loop {
        let (buf, len) = ProtoEvent::Ping.into();

        // at least one ping of the round must be answered
        for _ in 0..PINGS_PER_ROUND {
            if let Err(e) = conn.send(&buf[..len]).await {
                log::warn!("{addr}: send error `{e}`, closing connection");
                let _ = conn.close().await;
                break;
            }
            ping_state.borrow_mut().sent(addr);
            log::trace!("PING >->->->->- {addr}");

            tokio::time::sleep(PING_INTERVAL).await;
        }

        if !ping_state.borrow_mut().take_response(addr) {
            log::warn!("{addr} did not respond, closing connection");
            let _ = conn.close().await;
            return;
        }
    }
}

async fn receive_loop(
    handle: ClientHandle,
    addr: SocketAddr,
    conn: Arc<dyn Conn + Send + Sync>,
    context: ConnectionContext,
) {
    let mut buf = vec![0u8; MAX_WIRE_SIZE];
    while let Ok(len) = conn.recv(&mut buf).await {
        match decode_wire_event(&buf[..len]) {
            Ok(WireEvent::Protocol(event)) => {
                log::trace!("{addr} <==<==<== {event}");
                match event {
                    ProtoEvent::Pong(b) => {
                        context.client_manager.set_active_addr(handle, Some(addr));
                        context.client_manager.set_alive(handle, b);
                        context.ping_state.borrow_mut().response(addr);
                    }
                    ProtoEvent::Hello {
                        commit,
                        capabilities,
                    } => {
                        context.client_manager.set_peer_commit(handle, Some(commit));
                        context.peer_capabilities.set(addr, capabilities);
                    }
                    event => context
                        .recv_tx
                        .send((handle, WireEvent::Protocol(event)))
                        .expect("channel closed"),
                }
            }
            Ok(event @ WireEvent::ClipboardText(_)) => context
                .recv_tx
                .send((handle, event))
                .expect("channel closed"),
            // Skip undecodable datagrams without dropping the
            // connection. Each DTLS recv is one framed message, so
            // skipping is safe and keeps us forward-compatible with
            // peers that send event types we don't yet know about.
            Err(e) => log::debug!("ignoring undecodable event from {addr}: {e}"),
        }
    }
    log::warn!("recv error");
    context.disconnect(handle, addr).await;
}

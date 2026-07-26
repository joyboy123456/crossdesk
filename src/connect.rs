use crate::{
    client::ClientManager,
    config::local_commit,
    observability::{self, Timestamp},
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
pub(crate) enum LanMouseConnectionError {
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
) -> Result<(Arc<dyn Conn + Sync + Send>, SocketAddr), (SocketAddr, LanMouseConnectionError)> {
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
        _ = timeout => Err((addr, LanMouseConnectionError::Timeout)),
        result = DTLSConn::new(conn, config, true, None) => match result {
            Ok(dtls_conn) => Ok((Arc::new(dtls_conn), addr)),
            Err(e) => Err((addr, e.into())),
        }
    }
}

async fn connect_any(
    addrs: &[SocketAddr],
    cert: Certificate,
) -> Result<(Arc<dyn Conn + Send + Sync>, SocketAddr), LanMouseConnectionError> {
    let mut joinset = JoinSet::new();
    for &addr in addrs {
        joinset.spawn_local(connect(addr, cert.clone()));
    }
    loop {
        match joinset.join_next().await {
            None => return Err(LanMouseConnectionError::NotConnected),
            Some(r) => match r.expect("join error") {
                Ok(conn) => return Ok(conn),
                Err((a, e)) => {
                    log::warn!("failed to connect to {a}: `{e}`")
                }
            },
        };
    }
}

pub(crate) struct LanMouseConnection {
    cert: Certificate,
    client_manager: ClientManager,
    conns: Rc<Mutex<HashMap<SocketAddr, Arc<dyn Conn + Send + Sync>>>>,
    connecting: Rc<Mutex<HashSet<ClientHandle>>>,
    recv_rx: Receiver<(ClientHandle, WireEvent)>,
    recv_tx: Sender<(ClientHandle, WireEvent)>,
    ping_state: Rc<RefCell<PingState>>,
    peer_capabilities: Rc<RefCell<HashMap<SocketAddr, u32>>>,
}

#[derive(Clone)]
struct ConnectionContext {
    client_manager: ClientManager,
    conns: Rc<Mutex<HashMap<SocketAddr, Arc<dyn Conn + Send + Sync>>>>,
    recv_tx: Sender<(ClientHandle, WireEvent)>,
    ping_state: Rc<RefCell<PingState>>,
    peer_capabilities: Rc<RefCell<HashMap<SocketAddr, u32>>>,
}

impl LanMouseConnection {
    pub(crate) fn new(cert: Certificate, client_manager: ClientManager) -> Self {
        let (recv_tx, recv_rx) = channel();
        Self {
            cert,
            client_manager,
            conns: Default::default(),
            connecting: Default::default(),
            recv_rx,
            recv_tx,
            ping_state: Default::default(),
            peer_capabilities: Default::default(),
        }
    }

    pub(crate) async fn recv(&mut self) -> (ClientHandle, WireEvent) {
        self.recv_rx.recv().await.expect("channel closed")
    }

    pub(crate) async fn send(
        &self,
        event: ProtoEvent,
        handle: ClientHandle,
    ) -> Result<(), LanMouseConnectionError> {
        let serialization_started = Timestamp::now();
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        observability::record_serialization(serialization_started);
        let buf = &buf[..len];
        if let Some(addr) = self.client_manager.active_addr(handle) {
            let conn = {
                let conns = self.conns.lock().await;
                conns.get(&addr).cloned()
            };
            if let Some(conn) = conn {
                if !self.client_manager.alive(handle) {
                    return Err(LanMouseConnectionError::TargetEmulationDisabled);
                }
                match conn.send(buf).await {
                    Ok(_) => observability::record_sent(&event),
                    Err(e) => {
                        log::warn!("client {handle} failed to send: {e}");
                        disconnect(
                            &self.client_manager,
                            handle,
                            addr,
                            &self.conns,
                            &self.peer_capabilities,
                        )
                        .await;
                    }
                }
                log::trace!("{event} >->->->->- {addr}");
                return Ok(());
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
                ConnectionContext {
                    client_manager: self.client_manager.clone(),
                    conns: self.conns.clone(),
                    recv_tx: self.recv_tx.clone(),
                    ping_state: self.ping_state.clone(),
                    peer_capabilities: self.peer_capabilities.clone(),
                },
            ));
        }
        Err(LanMouseConnectionError::NotConnected)
    }

    pub(crate) async fn send_clipboard(
        &self,
        text: &str,
        handle: ClientHandle,
    ) -> Result<(), LanMouseConnectionError> {
        let Some(addr) = self.client_manager.active_addr(handle) else {
            return Err(LanMouseConnectionError::NotConnected);
        };
        if self
            .peer_capabilities
            .borrow()
            .get(&addr)
            .is_none_or(|capabilities| capabilities & CAPABILITY_CLIPBOARD_TEXT == 0)
        {
            return Err(LanMouseConnectionError::ClipboardUnsupported);
        }
        let conn = {
            let conns = self.conns.lock().await;
            conns.get(&addr).cloned()
        }
        .ok_or(LanMouseConnectionError::NotConnected)?;
        let packet = encode_clipboard_text(text)?;
        if let Err(error) = conn.send(&packet).await {
            log::warn!("client {handle} failed to send clipboard text: {error}");
            disconnect(
                &self.client_manager,
                handle,
                addr,
                &self.conns,
                &self.peer_capabilities,
            )
            .await;
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
        let capable = self.peer_capabilities.borrow().clone();
        let conns = {
            let conns = self.conns.lock().await;
            conns
                .iter()
                .map(|(addr, conn)| (*addr, conn.clone()))
                .collect::<Vec<_>>()
        };
        for (addr, conn) in conns {
            if !capable
                .get(&addr)
                .is_some_and(|capabilities| capabilities & CAPABILITY_CLIPBOARD_TEXT != 0)
            {
                continue;
            }
            if let Err(error) = conn.send(&packet).await {
                log::warn!("failed to send clipboard text to {addr}: {error}");
                if let Some(handle) = self.client_manager.get_client(addr) {
                    disconnect(
                        &self.client_manager,
                        handle,
                        addr,
                        &self.conns,
                        &self.peer_capabilities,
                    )
                    .await;
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
) -> Result<(), LanMouseConnectionError> {
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
    Err(LanMouseConnectionError::NotConnected)
}

async fn ping_pong(
    addr: SocketAddr,
    conn: Arc<dyn Conn + Send + Sync>,
    ping_state: Rc<RefCell<PingState>>,
) {
    loop {
        let (buf, len) = ProtoEvent::Ping.into();

        // send 4 pings, at least one must be answered
        for _ in 0..4 {
            if let Err(e) = conn.send(&buf[..len]).await {
                log::warn!("{addr}: send error `{e}`, closing connection");
                let _ = conn.close().await;
                break;
            }
            ping_state.borrow_mut().sent(addr);
            log::trace!("PING >->->->->- {addr}");

            tokio::time::sleep(Duration::from_millis(500)).await;
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
                        context
                            .peer_capabilities
                            .borrow_mut()
                            .insert(addr, capabilities);
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
    disconnect(
        &context.client_manager,
        handle,
        addr,
        &context.conns,
        &context.peer_capabilities,
    )
    .await;
}

async fn disconnect(
    client_manager: &ClientManager,
    handle: ClientHandle,
    addr: SocketAddr,
    conns: &Mutex<HashMap<SocketAddr, Arc<dyn Conn + Send + Sync>>>,
    peer_capabilities: &RefCell<HashMap<SocketAddr, u32>>,
) {
    log::warn!("client ({handle}) @ {addr} connection closed");
    conns.lock().await.remove(&addr);
    client_manager.set_active_addr(handle, None);
    client_manager.set_peer_commit(handle, None);
    peer_capabilities.borrow_mut().remove(&addr);
    let active: Vec<SocketAddr> = conns.lock().await.keys().copied().collect();
    log::info!("active connections: {active:?}");
}

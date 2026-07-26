use futures::Stream;
use lan_mouse_proto::{
    CAPABILITY_CLIPBOARD_TEXT, MAX_EVENT_SIZE, MAX_WIRE_SIZE, ProtoEvent, WireEvent,
    decode_wire_event, encode_clipboard_text,
};
use rustls::pki_types::CertificateDer;
use std::{
    collections::{HashMap, VecDeque},
    net::{Ipv4Addr, SocketAddr},
    rc::Rc,
    sync::{Arc, Mutex, PoisonError, RwLock},
    time::Duration,
};
use thiserror::Error;
use tokio::{sync::Mutex as AsyncMutex, task::spawn_local};
use tokio_util::sync::CancellationToken;
use webrtc_dtls::{
    config::{ClientAuthType::RequireAnyClientCert, Config, ExtendedMasterSecretType},
    conn::DTLSConn,
    crypto::Certificate,
    listener::listen,
};
use webrtc_util::{Conn, Error, conn::Listener};

use crate::{
    crypto,
    observability::{self, Timestamp},
    peer::PeerCapabilities,
    task::{Receiver, Sender, TaskHandle, channel, send},
};

/// How long `accept()` may be awaited before the select loop goes around
/// again. Works around <https://github.com/webrtc-rs/webrtc/issues/614>,
/// where a pending `accept()` future starves the other select branches.
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// address the DTLS listener binds to
const LISTEN_IP: Ipv4Addr = Ipv4Addr::UNSPECIFIED;

#[derive(Error, Debug)]
pub enum ListenerCreationError {
    #[error(transparent)]
    WebrtcUtil(#[from] webrtc_util::Error),
    #[error(transparent)]
    WebrtcDtls(#[from] webrtc_dtls::Error),
}

type ArcConn = Arc<dyn Conn + Send + Sync>;

pub(crate) enum ListenEvent {
    Msg {
        event: WireEvent,
        addr: SocketAddr,
        received_at: Timestamp,
    },
    Accept {
        addr: SocketAddr,
        fingerprint: String,
    },
    Rejected {
        fingerprint: String,
    },
}

pub(crate) struct DtlsListener {
    listen_rx: Receiver<ListenEvent>,
    listen_task: TaskHandle,
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    request_port_change: Sender<u16>,
    port_changed: Receiver<Result<u16, ListenerCreationError>>,
    peer_capabilities: PeerCapabilities,
}

type VerifyPeerCertificateFn = Arc<
    dyn (Fn(&[Vec<u8>], &[CertificateDer<'static>]) -> Result<(), webrtc_dtls::Error>)
        + Send
        + Sync,
>;

impl DtlsListener {
    pub(crate) async fn new(
        port: u16,
        cert: Certificate,
        authorized_keys: Arc<RwLock<HashMap<String, String>>>,
    ) -> Result<Self, ListenerCreationError> {
        let (listen_tx, listen_rx) = channel();
        let (request_port_change, mut request_port_change_rx) = channel();
        let (port_changed_tx, port_changed) = channel();
        let connection_attempts: Arc<Mutex<VecDeque<String>>> = Default::default();

        let authorized = authorized_keys.clone();
        let verify_peer_certificate: Option<VerifyPeerCertificateFn> = {
            let connection_attempts = connection_attempts.clone();
            Some(Arc::new(
                move |certs: &[Vec<u8>], _chains: &[CertificateDer<'static>]| {
                    // Everything here comes from an unauthenticated peer, so
                    // it must only ever reject the connection - never abort
                    // the process.
                    let [cert] = certs else {
                        log::warn!(
                            "rejecting connection: expected exactly one peer certificate, got {}",
                            certs.len()
                        );
                        return Err(webrtc_dtls::Error::ErrVerifyDataMismatch);
                    };
                    let fingerprint = crypto::generate_fingerprint(cert);
                    if authorized
                        .read()
                        .unwrap_or_else(PoisonError::into_inner)
                        .contains_key(&fingerprint)
                    {
                        Ok(())
                    } else {
                        connection_attempts
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push_back(fingerprint);
                        Err(webrtc_dtls::Error::ErrVerifyDataMismatch)
                    }
                },
            ))
        };
        let cfg = Config {
            certificates: vec![cert.clone()],
            extended_master_secret: ExtendedMasterSecretType::Require,
            client_auth: RequireAnyClientCert,
            verify_peer_certificate,
            ..Default::default()
        };

        let listen_addr = SocketAddr::from((LISTEN_IP, port));
        let mut listener = listen(listen_addr, cfg.clone()).await?;

        let conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>> =
            Rc::new(AsyncMutex::new(Vec::new()));
        let peer_capabilities = PeerCapabilities::default();

        let conns_clone = conns.clone();
        let peer_capabilities_clone = peer_capabilities.clone();
        let cancellation_token = CancellationToken::new();
        let listen_task = {
            let listen_tx = listen_tx.clone();
            let connection_attempts = connection_attempts.clone();
            let cancellation_token = cancellation_token.clone();
            spawn_local(async move {
                loop {
                    let sleep = tokio::time::sleep(ACCEPT_POLL_INTERVAL);
                    tokio::select! {
                        _ = cancellation_token.cancelled() => return,
                        _ = sleep => continue,
                        c = listener.accept() => match c {
                            Ok((conn, addr)) => {
                                log::info!("dtls client connected, ip: {addr}");
                                let mut conns = conns_clone.lock().await;
                                conns.push((addr, conn.clone()));
                                // This runs after the peer authenticated, so a
                                // certificate is expected - but dropping one
                                // connection beats aborting the service.
                                let Some(fingerprint) = peer_fingerprint(&conn).await else {
                                    log::warn!("could not read the certificate of {addr}");
                                    continue;
                                };
                                send(&listen_tx, "accepted connection", ListenEvent::Accept { addr, fingerprint });
                                spawn_local(read_loop(
                                    conns_clone.clone(),
                                    addr,
                                    conn,
                                    listen_tx.clone(),
                                    peer_capabilities_clone.clone(),
                                ));
                            },
                            Err(e) => {
                                if let Error::Std(ref e) = e {
                                    if let Some(e) = e.0.downcast_ref::<webrtc_dtls::Error>() {
                                        match e {
                                            webrtc_dtls::Error::ErrVerifyDataMismatch => {
                                                if let Some(fingerprint) = connection_attempts.lock().unwrap_or_else(PoisonError::into_inner).pop_front() {
                                                    send(&listen_tx, "rejected connection", ListenEvent::Rejected { fingerprint });
                                                }
                                            }
                                            _ => log::warn!("accept: {e}"),
                                        }
                                    } else {
                                        log::warn!("accept: {e:?}");
                                    }
                                } else {
                                    log::warn!("accept: {e:?}");
                                }
                            }
                        },
                        port = request_port_change_rx.recv() => {
                            let Some(port) = port else {
                                return;
                            };
                            let listen_addr = SocketAddr::from((LISTEN_IP, port));
                            match listen(listen_addr, cfg.clone()).await {
                                Ok(new_listener) => {
                                    let _ = listener.close().await;
                                    listener = new_listener;
                                    send(&port_changed_tx, "port change result", Ok(port));
                                }
                                Err(e) => {
                                    log::warn!("unable to change port: {e}");
                                    send(&port_changed_tx, "port change error", Err(e.into()));
                                }
                            };
                        },
                    };
                }
            })
        };

        Ok(Self {
            conns,
            listen_rx,
            listen_task: TaskHandle::new(cancellation_token, listen_task),
            port_changed,
            request_port_change,
            peer_capabilities,
        })
    }

    pub(crate) fn request_port_change(&mut self, port: u16) {
        send(&self.request_port_change, "port change request", port);
    }

    /// The outcome of a requested port change, or `None` if the listener
    /// stopped before it could report one.
    pub(crate) async fn port_changed(&mut self) -> Option<Result<u16, ListenerCreationError>> {
        self.port_changed.recv().await
    }

    pub(crate) async fn terminate(&mut self) {
        self.listen_task.terminate("dtls listener").await;
        let conns = self.conns.lock().await;
        for (_, conn) in conns.iter() {
            let _ = conn.close().await;
        }
        // Ends the event stream: the read loops can no longer queue events and
        // anything still buffered is dropped.
        self.listen_rx.close();
    }

    pub(crate) async fn reply(&self, addr: SocketAddr, event: ProtoEvent) {
        log::trace!("reply {event} >=>=>=>=>=> {addr}");
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let conns = self.conns.lock().await;
        for (a, conn) in conns.iter() {
            if *a == addr {
                let _ = conn.send(&buf[..len]).await;
            }
        }
    }

    pub(crate) fn set_peer_capabilities(&self, addr: SocketAddr, capabilities: u32) {
        self.peer_capabilities.set(addr, capabilities);
    }

    pub(crate) async fn send_clipboard(&self, addr: SocketAddr, text: &str) {
        if !self
            .peer_capabilities
            .supports(addr, CAPABILITY_CLIPBOARD_TEXT)
        {
            return;
        }
        let packet = match encode_clipboard_text(text) {
            Ok(packet) => packet,
            Err(error) => {
                log::warn!("clipboard text was not sent to {addr}: {error}");
                return;
            }
        };
        let conns = self.conns.lock().await;
        if let Some((_, conn)) = conns.iter().find(|(peer, _)| *peer == addr) {
            if let Err(error) = conn.send(&packet).await {
                log::warn!("failed to send clipboard text to {addr}: {error}");
            }
        }
    }

    pub(crate) async fn broadcast_clipboard(&self, text: &str) {
        let packet = match encode_clipboard_text(text) {
            Ok(packet) => packet,
            Err(error) => {
                log::warn!("clipboard text was not broadcast: {error}");
                return;
            }
        };
        let conns = self.conns.lock().await;
        for (addr, conn) in conns.iter() {
            if self
                .peer_capabilities
                .supports(*addr, CAPABILITY_CLIPBOARD_TEXT)
            {
                if let Err(error) = conn.send(&packet).await {
                    log::warn!("failed to send clipboard text to {addr}: {error}");
                }
            }
        }
    }

    pub(crate) async fn get_certificate_fingerprint(&self, addr: SocketAddr) -> Option<String> {
        let conn = self
            .conns
            .lock()
            .await
            .iter()
            .find(|(a, _)| *a == addr)
            .map(|(_, c)| c.clone())?;
        peer_fingerprint(&conn).await
    }
}

/// SHA-256 fingerprint of the certificate the peer authenticated with.
///
/// `None` if the connection is not DTLS or presented no certificate; both mean
/// we cannot identify the peer, not that the process should stop.
async fn peer_fingerprint(conn: &ArcConn) -> Option<String> {
    let dtls_conn: &DTLSConn = conn.as_any().downcast_ref()?;
    let certs = dtls_conn.connection_state().await.peer_certificates;
    Some(crypto::generate_fingerprint(certs.first()?))
}

impl Stream for DtlsListener {
    type Item = ListenEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.listen_rx.poll_recv(cx)
    }
}

async fn read_loop(
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    addr: SocketAddr,
    conn: ArcConn,
    dtls_tx: Sender<ListenEvent>,
    peer_capabilities: PeerCapabilities,
) -> Result<(), Error> {
    let mut b = vec![0u8; MAX_WIRE_SIZE];

    while let Ok(len) = conn.recv(&mut b).await {
        let received_at = Timestamp::now();
        match decode_wire_event(&b[..len]) {
            Ok(event) => {
                if let WireEvent::Protocol(event) = &event {
                    observability::record_received(event);
                }
                send(
                    &dtls_tx,
                    "received event",
                    ListenEvent::Msg {
                        event,
                        addr,
                        received_at,
                    },
                )
            }
            Err(e) => {
                // Skip the malformed/unknown datagram and keep
                // listening. Each DTLS recv returns one full
                // datagram, so a parse error here can't desync a
                // stream; the next call gets a fresh, framed
                // message. This makes the protocol forward-
                // compatible: a peer running a newer Lan Mouse
                // version can introduce additional event types
                // and old peers will simply ignore them rather
                // than dropping the connection.
                log::debug!("ignoring undecodable event from {addr}: {e}");
            }
        }
    }
    log::info!("dtls client disconnected {addr:?}");
    peer_capabilities.remove(addr);
    let mut conns = conns.lock().await;
    // The entry may already be gone if the listener was torn down while this
    // read loop was still running.
    if let Some(index) = conns.iter().position(|(a, _)| *a == addr) {
        conns.remove(index);
    }
    Ok(())
}

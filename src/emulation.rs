use crate::{
    config::local_commit,
    listen::{DtlsListener, ListenEvent, ListenerCreationError},
    observability::{self, Timestamp},
    position::proto_to_ipc,
    task::{DropGuard, Receiver, Sender, TaskHandle, channel, send},
};
use futures::StreamExt;
use input_emulation::{EmulationHandle, InputEmulation, InputEmulationError};
use input_event::Event;
use lan_mouse_proto::{CAPABILITY_CLIPBOARD_TEXT, ProtoEvent, WireEvent};
use std::{
    cell::Cell,
    collections::HashMap,
    net::SocketAddr,
    rc::Rc,
    time::{Duration, Instant},
};
use tokio::{select, task::spawn_local};
use tokio_util::sync::CancellationToken;

/// how often connected peers are checked for liveness
const LIVENESS_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// a peer that has not sent anything for this long is considered gone; its
/// emulation handle is destroyed so held keys do not stick
const PEER_TIMEOUT: Duration = Duration::from_secs(1);

/// repeated connection attempts from the same unauthorized fingerprint are
/// reported to the frontend at most once per this interval
const REJECTED_REPORT_INTERVAL: Duration = Duration::from_secs(2);

/// emulation handling events received from a listener
pub(crate) struct Emulation {
    task: TaskHandle,
    request_tx: Sender<EmulationRequest>,
    event_rx: Receiver<EmulationEvent>,
}

pub(crate) enum EmulationEvent {
    Connected {
        addr: SocketAddr,
        fingerprint: String,
    },
    ConnectionAttempt {
        fingerprint: String,
    },
    /// new connection
    Entered {
        /// address of the connection
        addr: SocketAddr,
        /// position of the connection
        pos: lan_mouse_ipc::Position,
        /// certificate fingerprint of the connection
        fingerprint: String,
    },
    /// connection closed
    Disconnected {
        addr: SocketAddr,
    },
    /// the port of the listener has changed
    PortChanged(Result<u16, ListenerCreationError>),
    /// emulation was disabled
    EmulationDisabled,
    /// emulation was enabled
    EmulationEnabled,
    /// capture should be released
    ReleaseNotify,
    /// peer sent us a Hello with its build commit hash. Used to
    /// populate `client_manager.peer_commit` from the listen side
    /// too — without this, peer-version visibility silently fails
    /// whenever the outgoing connection in the *other* direction is
    /// broken (one-way setups, asymmetric NAT, peer's TCP listener
    /// down). The connect-side path stays as the primary source;
    /// this is the defensive fallback.
    PeerHello {
        addr: SocketAddr,
        commit: [u8; 8],
    },
    ClipboardText(String),
}

enum EmulationRequest {
    Reenable,
    Release(SocketAddr),
    ChangePort(u16),
    SetClipboard {
        text: Option<String>,
        broadcast: bool,
    },
}

impl Emulation {
    pub(crate) fn new(backend: Option<input_emulation::Backend>, listener: DtlsListener) -> Self {
        let cancellation_token = CancellationToken::new();
        let emulation_proxy = EmulationProxy::new(backend, cancellation_token.child_token());
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_task = ListenTask {
            listener,
            emulation_proxy,
            request_rx,
            event_tx,
            clipboard_text: None,
            cancellation_token: cancellation_token.clone(),
        };
        let task = TaskHandle::new(cancellation_token, spawn_local(emulation_task.run()));
        Self {
            task,
            request_tx,
            event_rx,
        }
    }

    pub(crate) fn send_leave_event(&self, addr: SocketAddr) {
        send(
            &self.request_tx,
            "leave notification",
            EmulationRequest::Release(addr),
        );
    }

    pub(crate) fn reenable(&self) {
        send(
            &self.request_tx,
            "emulation reenable",
            EmulationRequest::Reenable,
        );
    }

    pub(crate) fn request_port_change(&self, port: u16) {
        send(
            &self.request_tx,
            "port change",
            EmulationRequest::ChangePort(port),
        );
    }

    pub(crate) fn set_clipboard(&self, text: Option<String>, broadcast: bool) {
        send(
            &self.request_tx,
            "clipboard update",
            EmulationRequest::SetClipboard { text, broadcast },
        );
    }

    /// The next emulation event, or `None` once the listen task has stopped.
    pub(crate) async fn event(&mut self) -> Option<EmulationEvent> {
        self.event_rx.recv().await
    }

    /// wait for termination
    pub(crate) async fn terminate(&mut self) {
        self.task.terminate("input emulation").await;
    }
}

struct ListenTask {
    listener: DtlsListener,
    emulation_proxy: EmulationProxy,
    request_rx: Receiver<EmulationRequest>,
    event_tx: Sender<EmulationEvent>,
    clipboard_text: Option<String>,
    cancellation_token: CancellationToken,
}

impl ListenTask {
    async fn run(mut self) {
        let mut interval = tokio::time::interval(LIVENESS_CHECK_INTERVAL);
        let mut last_response = HashMap::new();
        let mut rejected_connections = HashMap::new();
        loop {
            select! {
                e = self.listener.next() => {match e {
                    Some(ListenEvent::Msg { event, addr, received_at }) => {
                        last_response.insert(addr, Instant::now());
                        match event {
                            WireEvent::ClipboardText(text) => {
                                send(&self.event_tx, "remote clipboard text", EmulationEvent::ClipboardText(text));
                            }
                            WireEvent::Protocol(event) => {
                                log::trace!("{event} <-<-<-<-<- {addr}");
                                match event {
                                    ProtoEvent::Enter(pos) => {
                                        if let Some(fingerprint) = self.listener.get_certificate_fingerprint(addr).await {
                                            log::info!("releasing capture: {addr} entered this device");
                                            send(&self.event_tx, "release notification", EmulationEvent::ReleaseNotify);
                                            self.listener.reply(addr, ProtoEvent::Ack(0)).await;
                                            send(&self.event_tx, "peer entered", EmulationEvent::Entered{addr, pos: proto_to_ipc(pos), fingerprint});
                                        }
                                    }
                                    ProtoEvent::Leave(_) => {
                                        self.emulation_proxy.remove(addr);
                                        self.listener.reply(addr, ProtoEvent::Ack(0)).await;
                                    }
                                    ProtoEvent::Input(event) => self.emulation_proxy.consume(event, addr, received_at),
                                    ProtoEvent::Ping => self.listener.reply(addr, ProtoEvent::Pong(self.emulation_proxy.emulation_active.get())).await,
                                    ProtoEvent::Hello { commit, capabilities } => {
                                        self.listener.set_peer_capabilities(addr, capabilities);
                                        self.listener.reply(addr, ProtoEvent::Hello {
                                            commit: local_commit(),
                                            capabilities: CAPABILITY_CLIPBOARD_TEXT,
                                        }).await;
                                        if capabilities & CAPABILITY_CLIPBOARD_TEXT != 0 {
                                            if let Some(text) = self.clipboard_text.as_deref() {
                                                self.listener.send_clipboard(addr, text).await;
                                            }
                                        }
                                        send(&self.event_tx, "peer hello", EmulationEvent::PeerHello { addr, commit });
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Some(ListenEvent::Accept { addr, fingerprint }) => {
                        send(&self.event_tx, "peer connected", EmulationEvent::Connected { addr, fingerprint });
                    }
                    Some(ListenEvent::Rejected { fingerprint }) => {
                        if rejected_connections.insert(fingerprint.clone(), Instant::now())
                            .is_none_or(|i| i.elapsed() >= REJECTED_REPORT_INTERVAL) {
                                send(&self.event_tx, "connection attempt", EmulationEvent::ConnectionAttempt { fingerprint });
                            }
                    }
                    None => break
                }}
                event = self.emulation_proxy.event() => match event {
                    Some(event) => send(&self.event_tx, "emulation event", event),
                    None => break,
                },
                request = self.request_rx.recv() => match request {
                    None => break,
                    // reenable emulation
                    Some(EmulationRequest::Reenable) => self.emulation_proxy.reenable(),
                    // notify the other end that we hit a barrier (should release capture)
                    Some(EmulationRequest::Release(addr)) => self.listener.reply(addr, ProtoEvent::Leave(0)).await,
                    Some(EmulationRequest::ChangePort(port)) => {
                        self.listener.request_port_change(port);
                        match self.listener.port_changed().await {
                            Some(result) => send(&self.event_tx, "port change result", EmulationEvent::PortChanged(result)),
                            None => break,
                        }
                    }
                    Some(EmulationRequest::SetClipboard { text, broadcast }) => {
                        self.clipboard_text = text;
                        if broadcast {
                            if let Some(text) = self.clipboard_text.as_deref() {
                                self.listener.broadcast_clipboard(text).await;
                            }
                        }
                    }
                },
                _ = interval.tick() => {
                    last_response.retain(|&addr,instant| {
                        if instant.elapsed() > PEER_TIMEOUT {
                            log::warn!("releasing keys: {addr} not responding!");
                            self.emulation_proxy.remove(addr);
                            send(&self.event_tx, "peer disconnected", EmulationEvent::Disconnected { addr });
                            false
                        } else {
                            true
                        }
                    });
                }
                _ = self.cancellation_token.cancelled() => break,
            }
        }
        self.listener.terminate().await;
        self.emulation_proxy.terminate().await;
    }
}

/// proxy handling the actual input emulation,
/// discarding events when it is disabled
pub(crate) struct EmulationProxy {
    emulation_active: Rc<Cell<bool>>,
    request_tx: Sender<ProxyRequest>,
    event_rx: Receiver<EmulationEvent>,
    task: TaskHandle,
}

enum ProxyRequest {
    Input(Event, SocketAddr, Timestamp),
    Remove(SocketAddr),
    Reenable,
}

impl ProxyRequest {
    fn record_dequeued(&self) {
        if matches!(self, Self::Input(..)) {
            observability::injection_queue_pop();
        }
    }
}

impl EmulationProxy {
    fn new(
        backend: Option<input_emulation::Backend>,
        cancellation_token: CancellationToken,
    ) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_active = Rc::new(Cell::new(false));
        let emulation_task = EmulationTask {
            backend,
            cancellation_token: cancellation_token.clone(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
        };
        let task = TaskHandle::new(cancellation_token, spawn_local(emulation_task.run()));
        Self {
            emulation_active,
            request_tx,
            task,
            event_rx,
        }
    }

    /// The next event from the emulation backend, or `None` once its task has
    /// stopped.
    async fn event(&mut self) -> Option<EmulationEvent> {
        let event = self.event_rx.recv().await?;
        if let EmulationEvent::EmulationEnabled = event {
            self.emulation_active.replace(true);
        }
        if let EmulationEvent::EmulationDisabled = event {
            self.emulation_active.replace(false);
        }
        Some(event)
    }

    fn consume(&self, event: Event, addr: SocketAddr, received_at: Timestamp) {
        // ignore events if emulation is currently disabled
        if self.emulation_active.get() {
            observability::injection_queue_push();
            send(
                &self.request_tx,
                "input event",
                ProxyRequest::Input(event, addr, received_at),
            );
        } else {
            observability::record_emulation_inactive_drop(&event);
        }
    }

    fn remove(&self, addr: SocketAddr) {
        send(&self.request_tx, "peer removal", ProxyRequest::Remove(addr));
    }

    fn reenable(&self) {
        send(
            &self.request_tx,
            "emulation reenable",
            ProxyRequest::Reenable,
        );
    }

    async fn terminate(&mut self) {
        self.task.terminate("emulation backend").await;
    }
}

struct EmulationTask {
    backend: Option<input_emulation::Backend>,
    cancellation_token: CancellationToken,
    request_rx: Receiver<ProxyRequest>,
    event_tx: Sender<EmulationEvent>,
    handles: HashMap<SocketAddr, EmulationHandle>,
    next_id: EmulationHandle,
}

impl EmulationTask {
    async fn run(mut self) {
        loop {
            if let Err(e) = self.do_emulation().await {
                log::warn!("input emulation exited: {e}");
            }
            if self.cancellation_token.is_cancelled() {
                break;
            }
            // wait for reenable request
            loop {
                let request = select! {
                    request = self.request_rx.recv() => match request {
                        Some(request) => request,
                        None => return,
                    },
                    _ = self.cancellation_token.cancelled() => return,
                };
                request.record_dequeued();
                match request {
                    ProxyRequest::Reenable => break,
                    ProxyRequest::Input(event, ..) => {
                        observability::record_emulation_inactive_drop(&event);
                    }
                    ProxyRequest::Remove(..) => { /* emulation inactive => ignore */ }
                }
            }
        }
    }

    async fn do_emulation(&mut self) -> Result<(), InputEmulationError> {
        log::info!("creating input emulation ...");
        let mut emulation = select! {
            r = InputEmulation::new(self.backend) => r?,
            // allow termination while requesting input emulation
            _ = self.cancellation_token.cancelled() => return Ok(()),
        };

        // used to send enabled and disabled events
        let _emulation_guard = DropGuard::new(
            self.event_tx.clone(),
            EmulationEvent::EmulationEnabled,
            EmulationEvent::EmulationDisabled,
        );

        // create active handles
        if let Err(e) = self.create_clients(&mut emulation).await {
            emulation.terminate().await;
            return Err(e);
        }

        let res = self.do_emulation_session(&mut emulation).await;
        // FIXME replace with async drop when stabilized
        emulation.terminate().await;
        res
    }

    async fn create_clients(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        for handle in self.handles.values() {
            select! {
                _ = emulation.create(*handle) => {},
                _ = self.cancellation_token.cancelled() => return Ok(()),
            }
        }
        Ok(())
    }

    async fn do_emulation_session(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        loop {
            select! {
                _ = self.cancellation_token.cancelled() => break Ok(()),
                e = self.request_rx.recv() => {
                    let Some(request) = e else {
                        break Ok(());
                    };
                    request.record_dequeued();
                    match request {
                    ProxyRequest::Input(event, addr, received_at) => {
                        let handle = match self.handles.get(&addr) {
                            Some(&handle) => handle,
                            None => {
                                let handle = self.next_id;
                                self.next_id += 1;
                                emulation.create(handle).await;
                                self.handles.insert(addr, handle);
                                handle
                            }
                        };
                        let result = emulation.consume(event, handle).await;
                        observability::record_receive_to_inject(received_at);
                        result?;
                    },
                    ProxyRequest::Remove(addr) => {
                        if let Some(handle) = self.handles.remove(&addr) {
                            emulation.destroy(handle).await;
                        }
                    }
                    ProxyRequest::Reenable => continue,
                }},
            }
        }
    }
}

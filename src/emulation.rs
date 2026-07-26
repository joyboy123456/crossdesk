use crate::{
    config::local_commit,
    listen::{DtlsListener, ListenEvent, ListenerCreationError},
    observability::{self, Timestamp},
    position::proto_to_ipc,
    task::DropGuard,
};
use futures::StreamExt;
use input_emulation::{EmulationHandle, InputEmulation, InputEmulationError};
use input_event::Event;
use lan_mouse_proto::{CAPABILITY_CLIPBOARD_TEXT, ProtoEvent, WireEvent};
use local_channel::mpsc::{Receiver, Sender, channel};
use std::{
    cell::Cell,
    collections::HashMap,
    net::SocketAddr,
    rc::Rc,
    time::{Duration, Instant},
};
use tokio::{
    select,
    task::{JoinHandle, spawn_local},
};

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
    task: JoinHandle<()>,
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
    Terminate,
}

impl Emulation {
    pub(crate) fn new(backend: Option<input_emulation::Backend>, listener: DtlsListener) -> Self {
        let emulation_proxy = EmulationProxy::new(backend);
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_task = ListenTask {
            listener,
            emulation_proxy,
            request_rx,
            event_tx,
            clipboard_text: None,
        };
        let task = spawn_local(emulation_task.run());
        Self {
            task,
            request_tx,
            event_rx,
        }
    }

    pub(crate) fn send_leave_event(&self, addr: SocketAddr) {
        self.request_tx
            .send(EmulationRequest::Release(addr))
            .expect("channel closed");
    }

    pub(crate) fn reenable(&self) {
        self.request_tx
            .send(EmulationRequest::Reenable)
            .expect("channel closed");
    }

    pub(crate) fn request_port_change(&self, port: u16) {
        self.request_tx
            .send(EmulationRequest::ChangePort(port))
            .expect("channel closed")
    }

    pub(crate) fn set_clipboard(&self, text: Option<String>, broadcast: bool) {
        self.request_tx
            .send(EmulationRequest::SetClipboard { text, broadcast })
            .expect("channel closed");
    }

    pub(crate) async fn event(&mut self) -> EmulationEvent {
        self.event_rx.recv().await.expect("channel closed")
    }

    /// wait for termination
    pub(crate) async fn terminate(&mut self) {
        log::debug!("terminating emulation");
        self.request_tx
            .send(EmulationRequest::Terminate)
            .expect("channel closed");
        if let Err(e) = (&mut self.task).await {
            log::warn!("{e}");
        }
    }
}

struct ListenTask {
    listener: DtlsListener,
    emulation_proxy: EmulationProxy,
    request_rx: Receiver<EmulationRequest>,
    event_tx: Sender<EmulationEvent>,
    clipboard_text: Option<String>,
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
                                self.event_tx
                                    .send(EmulationEvent::ClipboardText(text))
                                    .expect("channel closed");
                            }
                            WireEvent::Protocol(event) => {
                                log::trace!("{event} <-<-<-<-<- {addr}");
                                match event {
                                    ProtoEvent::Enter(pos) => {
                                        if let Some(fingerprint) = self.listener.get_certificate_fingerprint(addr).await {
                                            log::info!("releasing capture: {addr} entered this device");
                                            self.event_tx.send(EmulationEvent::ReleaseNotify).expect("channel closed");
                                            self.listener.reply(addr, ProtoEvent::Ack(0)).await;
                                            self.event_tx.send(EmulationEvent::Entered{addr, pos: proto_to_ipc(pos), fingerprint}).expect("channel closed");
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
                                        self.event_tx.send(EmulationEvent::PeerHello { addr, commit }).expect("channel closed");
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Some(ListenEvent::Accept { addr, fingerprint }) => {
                        self.event_tx.send(EmulationEvent::Connected { addr, fingerprint }).expect("channel closed");
                    }
                    Some(ListenEvent::Rejected { fingerprint }) => {
                        if rejected_connections.insert(fingerprint.clone(), Instant::now())
                            .is_none_or(|i| i.elapsed() >= REJECTED_REPORT_INTERVAL) {
                                self.event_tx.send(EmulationEvent::ConnectionAttempt { fingerprint }).expect("channel closed");
                            }
                    }
                    None => break
                }}
                event = self.emulation_proxy.event() => {
                    self.event_tx.send(event).expect("channel closed");
                }
                request = self.request_rx.recv() => match request.expect("channel closed") {
                    // reenable emulation
                    EmulationRequest::Reenable => self.emulation_proxy.reenable(),
                    // notify the other end that we hit a barrier (should release capture)
                    EmulationRequest::Release(addr) => self.listener.reply(addr, ProtoEvent::Leave(0)).await,
                    EmulationRequest::ChangePort(port) => {
                        self.listener.request_port_change(port);
                        let result = self.listener.port_changed().await;
                        self.event_tx.send(EmulationEvent::PortChanged(result)).expect("channel closed");
                    }
                    EmulationRequest::SetClipboard { text, broadcast } => {
                        self.clipboard_text = text;
                        if broadcast {
                            if let Some(text) = self.clipboard_text.as_deref() {
                                self.listener.broadcast_clipboard(text).await;
                            }
                        }
                    }
                    EmulationRequest::Terminate => break,
                },
                _ = interval.tick() => {
                    last_response.retain(|&addr,instant| {
                        if instant.elapsed() > PEER_TIMEOUT {
                            log::warn!("releasing keys: {addr} not responding!");
                            self.emulation_proxy.remove(addr);
                            self.event_tx.send(EmulationEvent::Disconnected { addr }).expect("channel closed");
                            false
                        } else {
                            true
                        }
                    });
                }
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
    exit_requested: Rc<Cell<bool>>,
    request_tx: Sender<ProxyRequest>,
    event_rx: Receiver<EmulationEvent>,
    task: JoinHandle<()>,
}

enum ProxyRequest {
    Input(Event, SocketAddr, Timestamp),
    Remove(SocketAddr),
    Terminate,
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
    fn new(backend: Option<input_emulation::Backend>) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_active = Rc::new(Cell::new(false));
        let exit_requested = Rc::new(Cell::new(false));
        let emulation_task = EmulationTask {
            backend,
            exit_requested: exit_requested.clone(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
        };
        let task = spawn_local(emulation_task.run());
        Self {
            emulation_active,
            exit_requested,
            request_tx,
            task,
            event_rx,
        }
    }

    async fn event(&mut self) -> EmulationEvent {
        let event = self.event_rx.recv().await.expect("channel closed");
        if let EmulationEvent::EmulationEnabled = event {
            self.emulation_active.replace(true);
        }
        if let EmulationEvent::EmulationDisabled = event {
            self.emulation_active.replace(false);
        }
        event
    }

    fn consume(&self, event: Event, addr: SocketAddr, received_at: Timestamp) {
        // ignore events if emulation is currently disabled
        if self.emulation_active.get() {
            observability::injection_queue_push();
            self.request_tx
                .send(ProxyRequest::Input(event, addr, received_at))
                .expect("channel closed");
        } else {
            observability::record_emulation_inactive_drop(&event);
        }
    }

    fn remove(&self, addr: SocketAddr) {
        self.request_tx
            .send(ProxyRequest::Remove(addr))
            .expect("channel closed");
    }

    fn reenable(&self) {
        self.request_tx
            .send(ProxyRequest::Reenable)
            .expect("channel closed");
    }

    async fn terminate(&mut self) {
        self.exit_requested.replace(true);
        self.request_tx
            .send(ProxyRequest::Terminate)
            .expect("channel closed");
        let _ = (&mut self.task).await;
    }
}

struct EmulationTask {
    backend: Option<input_emulation::Backend>,
    exit_requested: Rc<Cell<bool>>,
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
            if self.exit_requested.get() {
                break;
            }
            // wait for reenable request
            loop {
                let request = self.request_rx.recv().await.expect("channel closed");
                request.record_dequeued();
                match request {
                    ProxyRequest::Reenable => break,
                    ProxyRequest::Terminate => return,
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
        let mut emulation = tokio::select! {
            r = InputEmulation::new(self.backend) => r?,
            // allow termination event while requesting input emulation
            _ = wait_for_termination(&mut self.request_rx) => return Ok(()),
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
            tokio::select! {
                _ = emulation.create(*handle) => {},
                _ = wait_for_termination(&mut self.request_rx) => return Ok(()),
            }
        }
        Ok(())
    }

    async fn do_emulation_session(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        loop {
            tokio::select! {
                e = self.request_rx.recv() => {
                    let request = e.expect("channel closed");
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
                    ProxyRequest::Terminate => break Ok(()),
                    ProxyRequest::Reenable => continue,
                }},
            }
        }
    }
}

async fn wait_for_termination(rx: &mut Receiver<ProxyRequest>) {
    loop {
        let request = rx.recv().await.expect("channel closed");
        request.record_dequeued();
        match request {
            ProxyRequest::Terminate => return,
            ProxyRequest::Input(event, ..) => {
                observability::record_emulation_inactive_drop(&event);
            }
            ProxyRequest::Remove(_) => continue,
            ProxyRequest::Reenable => continue,
        }
    }
}

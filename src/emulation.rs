use crate::{
    config::local_commit,
    listen::{DtlsListener, ListenEvent, ListenerCreationError},
    observability::{self, Timestamp},
    position::{proto_to_emulation, proto_to_ipc},
    service::control::ControlArbiter,
    task::{DropGuard, Receiver, Sender, TaskHandle, channel, send},
};
use futures::StreamExt;
use input_emulation::{EmulationHandle, InputEmulation, InputEmulationError};
use input_event::Event;
use lan_mouse_proto::{
    CAPABILITY_CLIPBOARD_TEXT, CAPABILITY_CONTROL_SESSION, CAPABILITY_ENTER_POSITION,
    CONTROL_SESSION_CLOSE_BIT, ProtoEvent, WireEvent,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    net::SocketAddr,
    rc::Rc,
    time::{Duration, Instant},
};
use tokio::{select, task::spawn_local};
use tokio_util::sync::CancellationToken;

/// how often connected peers are checked for liveness
const LIVENESS_CHECK_INTERVAL: Duration = Duration::from_millis(250);

/// a peer that has not sent anything for this long is considered gone; its
/// emulation handle is destroyed so held keys do not stick
const PEER_TIMEOUT: Duration = Duration::from_secs(1);

/// repeated connection attempts from the same unauthorized fingerprint are
/// reported to the frontend at most once per this interval
const REJECTED_REPORT_INTERVAL: Duration = Duration::from_secs(2);
const LEAVE_ACK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug)]
struct PendingLeave {
    event: ProtoEvent,
    serial: u32,
    scoped: bool,
    started_at: Instant,
    last_sent: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionSerial {
    Legacy,
    Scoped(u32),
}

impl SessionSerial {
    fn wire(self) -> u32 {
        match self {
            Self::Legacy => 0,
            Self::Scoped(serial) => serial,
        }
    }

    fn close_wire(self) -> u32 {
        match self {
            Self::Legacy => 0,
            Self::Scoped(serial) => serial | CONTROL_SESSION_CLOSE_BIT,
        }
    }

    fn matches_close(self, serial: u32) -> bool {
        self.close_wire() == serial
    }

    fn valid(self) -> bool {
        match self {
            Self::Legacy => true,
            Self::Scoped(serial) => serial != 0 && serial & CONTROL_SESSION_CLOSE_BIT == 0,
        }
    }
}

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
    /// A peer that was controlling this device ended its entered session.
    Left {
        addr: SocketAddr,
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
    Release(SocketAddr, Option<f64>),
    Disconnect(SocketAddr),
    SetAcceptingControl(bool),
    ChangePort(u16),
    SetClipboard {
        text: Option<String>,
        broadcast: bool,
    },
}

impl Emulation {
    pub(crate) fn new(
        backend: Option<input_emulation::Backend>,
        listener: DtlsListener,
        accepting_control: bool,
        control_arbiter: ControlArbiter,
    ) -> Self {
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
            entered: HashMap::new(),
            accepting_control,
            control_arbiter,
            pending_leaves: HashMap::new(),
        };
        let task = TaskHandle::new(cancellation_token, spawn_local(emulation_task.run()));
        Self {
            task,
            request_tx,
            event_rx,
        }
    }

    pub(crate) fn send_leave_event(&self, addr: SocketAddr, edge_ratio: Option<f64>) {
        send(
            &self.request_tx,
            "leave notification",
            EmulationRequest::Release(addr, edge_ratio),
        );
    }

    pub(crate) fn disconnect(&self, addr: SocketAddr) {
        send(
            &self.request_tx,
            "controller disconnect",
            EmulationRequest::Disconnect(addr),
        );
    }

    pub(crate) fn set_accepting_control(&self, accepting: bool) {
        send(
            &self.request_tx,
            "control acceptance",
            EmulationRequest::SetAcceptingControl(accepting),
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
    /// peers currently controlling this device (Enter accepted, no Leave
    /// yet); they are kicked with a Leave when emulation becomes unavailable
    entered: HashMap<SocketAddr, SessionSerial>,
    /// Whether a new peer may begin controlling this device. Existing peers
    /// may still retransmit Enter to recover a lost Ack.
    accepting_control: bool,
    /// synchronous direction ownership shared with the capture task
    control_arbiter: ControlArbiter,
    /// Leave packets are retried until the controlling peer confirms by Ack
    /// or its reciprocal Leave. A single lost UDP datagram must not strand the
    /// peer's cursor in Sending forever.
    pending_leaves: HashMap<SocketAddr, PendingLeave>,
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
                                        self.handle_enter(addr, pos, None, SessionSerial::Legacy).await;
                                    }
                                    ProtoEvent::EnterAt { pos, ratio } => {
                                        self.handle_enter(
                                            addr,
                                            pos,
                                            Some(ratio),
                                            SessionSerial::Legacy,
                                        ).await;
                                    }
                                    ProtoEvent::EnterSession { pos, serial, ratio } => {
                                        self.handle_enter(
                                            addr,
                                            pos,
                                            Some(ratio),
                                            SessionSerial::Scoped(serial),
                                        ).await;
                                    }
                                    ProtoEvent::Leave(serial)
                                    | ProtoEvent::LeaveAt { serial, .. } => {
                                        self.handle_leave(addr, serial).await;
                                    }
                                    ProtoEvent::Ack(serial) => {
                                        if let Some(pending) = self.pending_leaves.get(&addr).copied() {
                                            if pending.serial == serial {
                                                self.pending_leaves.remove(&addr);
                                                if !pending.scoped {
                                                    self.listener.close_peer(addr).await;
                                                }
                                            }
                                        }
                                    }
                                    ProtoEvent::Input(event) => {
                                        if input_allowed(
                                            &self.entered,
                                            addr,
                                            SessionSerial::Legacy,
                                        ) {
                                            self.emulation_proxy.consume(event, addr, received_at);
                                        } else {
                                            log::debug!("ignoring input from {addr} outside an entered session");
                                        }
                                    }
                                    ProtoEvent::InputSession { serial, event } => {
                                        if input_allowed(
                                            &self.entered,
                                            addr,
                                            SessionSerial::Scoped(serial),
                                        ) {
                                            self.emulation_proxy.consume(event, addr, received_at);
                                        } else {
                                            log::debug!(
                                                "ignoring stale scoped input from {addr} session {serial}"
                                            );
                                        }
                                    }
                                    ProtoEvent::Ping => self.listener.reply(addr, ProtoEvent::Pong(self.emulation_proxy.emulation_active.get())).await,
                                    ProtoEvent::Hello { commit, capabilities } => {
                                        self.listener.set_peer_capabilities(addr, capabilities);
                                        self.listener.reply(addr, ProtoEvent::Hello {
                                            commit: local_commit(),
                                            capabilities: CAPABILITY_CLIPBOARD_TEXT
                                                | CAPABILITY_ENTER_POSITION
                                                | CAPABILITY_CONTROL_SESSION,
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
                    Some(event) => {
                        // emulation just became unavailable (backend gone or
                        // permissions lost): kick every peer currently
                        // controlling us, otherwise their cursor stays
                        // captured aiming at a black hole
                        if matches!(event, EmulationEvent::EmulationDisabled) {
                            for (addr, session) in std::mem::take(&mut self.entered) {
                                log::warn!("emulation became unavailable, kicking {addr}");
                                let serial = session.close_wire();
                                let leave = ProtoEvent::Leave(serial);
                                self.listener.reply(addr, leave).await;
                                self.pending_leaves.insert(
                                    addr,
                                    PendingLeave {
                                        event: leave,
                                        serial,
                                        scoped: matches!(session, SessionSerial::Scoped(_)),
                                        started_at: Instant::now(),
                                        last_sent: Instant::now(),
                                    },
                                );
                                self.emulation_proxy.remove(addr);
                                self.control_arbiter.release_incoming();
                                send(&self.event_tx, "peer left", EmulationEvent::Left { addr });
                            }
                        }
                        send(&self.event_tx, "emulation event", event)
                    }
                    None => break,
                },
                request = self.request_rx.recv() => match request {
                    None => break,
                    // reenable emulation
                    Some(EmulationRequest::Reenable) => self.emulation_proxy.reenable(),
                    // notify the other end that we hit a barrier (should release capture)
                    Some(EmulationRequest::Release(addr, edge_ratio)) => {
                        let session = self
                            .entered
                            .get(&addr)
                            .copied()
                            .unwrap_or(SessionSerial::Legacy);
                        let serial = session.close_wire();
                        let event = match edge_ratio {
                            Some(ratio) if self.listener.peer_supports(addr, CAPABILITY_ENTER_POSITION) => {
                                ProtoEvent::LeaveAt { serial, ratio }
                            }
                            _ => ProtoEvent::Leave(serial),
                        };
                        self.finish_incoming(addr);
                        self.listener.reply(addr, event).await;
                        self.pending_leaves.insert(
                            addr,
                            PendingLeave {
                                event,
                                serial,
                                scoped: matches!(session, SessionSerial::Scoped(_)),
                                started_at: Instant::now(),
                                last_sent: Instant::now(),
                            },
                        );
                    }
                    Some(EmulationRequest::Disconnect(addr)) => {
                        let session = self
                            .entered
                            .get(&addr)
                            .copied()
                            .unwrap_or(SessionSerial::Legacy);
                        let serial = session.close_wire();
                        if self.finish_incoming(addr) {
                            let event = ProtoEvent::Leave(serial);
                            self.listener.reply(addr, event).await;
                            self.pending_leaves.insert(
                                addr,
                                PendingLeave {
                                    event,
                                    serial,
                                    scoped: matches!(session, SessionSerial::Scoped(_)),
                                    started_at: Instant::now(),
                                    last_sent: Instant::now(),
                                },
                            );
                        }
                    }
                    Some(EmulationRequest::SetAcceptingControl(accepting)) => {
                        self.accepting_control = accepting;
                    }
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
                    let entered = &mut self.entered;
                    let pending_leaves = &mut self.pending_leaves;
                    last_response.retain(|&addr,instant| {
                        if instant.elapsed() > PEER_TIMEOUT {
                            log::warn!("releasing keys: {addr} not responding!");
                            if entered.remove(&addr).is_some() {
                                self.emulation_proxy.remove(addr);
                                self.control_arbiter.release_incoming();
                            }
                            pending_leaves.remove(&addr);
                            send(&self.event_tx, "peer disconnected", EmulationEvent::Disconnected { addr });
                            false
                        } else {
                            true
                        }
                    });

                    let expired = self
                        .pending_leaves
                        .iter()
                        .filter_map(|(&addr, pending)| {
                            (pending.started_at.elapsed() >= LEAVE_ACK_TIMEOUT).then_some(addr)
                        })
                        .collect::<Vec<_>>();
                    for addr in expired {
                        log::warn!(
                            "closing {addr}: Leave was not acknowledged within {LEAVE_ACK_TIMEOUT:?}"
                        );
                        self.pending_leaves.remove(&addr);
                        last_response.remove(&addr);
                        self.listener.close_peer(addr).await;
                    }

                    let retries = self
                        .pending_leaves
                        .iter_mut()
                        .filter_map(|(&addr, pending)| {
                            if pending.last_sent.elapsed() >= LIVENESS_CHECK_INTERVAL {
                                pending.last_sent = Instant::now();
                                Some((addr, pending.event))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    for (addr, event) in retries {
                        self.listener.reply(addr, event).await;
                    }
                }
                _ = self.cancellation_token.cancelled() => break,
            }
        }
        self.listener.terminate().await;
        self.emulation_proxy.terminate().await;
    }

    fn finish_incoming(&mut self, addr: SocketAddr) -> bool {
        if self.entered.remove(&addr).is_none() {
            return false;
        }
        // Invalidate the proxy generation synchronously before another
        // direction can acquire the arbiter. Queued input from this session
        // will then be discarded even if its FIFO Remove is still pending.
        self.emulation_proxy.remove(addr);
        self.control_arbiter.release_incoming();
        send(&self.event_tx, "peer left", EmulationEvent::Left { addr });
        true
    }

    async fn handle_leave(&mut self, addr: SocketAddr, serial: u32) {
        let legacy_epoch_rollover = self
            .pending_leaves
            .get(&addr)
            .is_some_and(|pending| !pending.scoped && pending.serial == serial);
        if self
            .pending_leaves
            .get(&addr)
            .is_some_and(|pending| pending.serial == serial)
        {
            self.pending_leaves.remove(&addr);
        }
        if self
            .entered
            .get(&addr)
            .is_some_and(|session| session.matches_close(serial))
        {
            self.finish_incoming(addr);
        }
        // Ack even a stale Leave so its sender can stop retrying, but never
        // let a mismatched generation tear down the current session.
        self.listener.reply(addr, ProtoEvent::Ack(serial)).await;
        if legacy_epoch_rollover {
            self.listener.close_peer(addr).await;
        }
    }

    async fn handle_enter(
        &mut self,
        addr: SocketAddr,
        pos: lan_mouse_proto::Position,
        ratio: Option<f64>,
        session: SessionSerial,
    ) {
        if !session.valid() {
            log::warn!("rejecting invalid control-session serial from {addr}");
            self.listener
                .reply(addr, ProtoEvent::Leave(session.close_wire()))
                .await;
            return;
        }
        if let Some(pending) = self.pending_leaves.get(&addr).copied() {
            // Without a protocol session id, an Enter received before the
            // previous Leave handshake completes may be a delayed packet from
            // the old session. Keep closing that session until Ack/Leave.
            self.listener.reply(addr, pending.event).await;
            return;
        }
        match enter_decision(&self.entered, self.accepting_control, addr, session) {
            EnterDecision::Duplicate { ack } => {
                // The sender retries Enter until its Ack arrives. A retry is
                // synchronization only: do not release capture, recreate the
                // emulation handle, or post another absolute edge movement.
                self.listener.reply(addr, ProtoEvent::Ack(ack)).await;
                return;
            }
            EnterDecision::Upgrade { ack } => {
                self.entered.insert(addr, session);
                self.listener.reply(addr, ProtoEvent::Ack(ack)).await;
                return;
            }
            EnterDecision::Reject { leave } => {
                log::warn!("rejecting enter from {addr}: this device is not accepting control");
                self.listener.reply(addr, ProtoEvent::Leave(leave)).await;
                return;
            }
            EnterDecision::Accept => {}
        }

        let Some(fingerprint) = self.listener.get_certificate_fingerprint(addr).await else {
            return;
        };
        if self
            .reject_enter_if_emulation_unavailable(addr, session.wire())
            .await
        {
            return;
        }
        if !self.control_arbiter.try_acquire_incoming() {
            log::warn!("rejecting enter from {addr}: outgoing control owns the session");
            self.listener
                .reply(addr, ProtoEvent::Leave(session.close_wire()))
                .await;
            return;
        }

        match ratio {
            Some(ratio) => {
                log::info!("releasing capture: {addr} entered this device (at {ratio:.3})");
            }
            None => log::info!("releasing capture: {addr} entered this device"),
        }
        send(
            &self.event_tx,
            "release notification",
            EmulationEvent::ReleaseNotify,
        );
        self.entered.insert(addr, session);
        self.emulation_proxy.activate(addr);
        self.listener
            .reply(addr, ProtoEvent::Ack(session.wire()))
            .await;
        send(
            &self.event_tx,
            "peer entered",
            EmulationEvent::Entered {
                addr,
                pos: proto_to_ipc(pos),
                fingerprint,
            },
        );
        if let Some(ratio) = ratio.filter(|ratio| ratio.is_finite()) {
            self.emulation_proxy
                .enter(proto_to_emulation(pos), ratio.clamp(0.0, 1.0), addr);
        }
    }

    /// If input emulation is currently unavailable (no backend or missing
    /// permissions), refuse the Enter by replying with a Leave so the
    /// controlling side releases its capture immediately. Without this the
    /// controller acks into a black hole: its mouse is captured and every
    /// event goes to an emulator that cannot move the local cursor, so the
    /// return barrier is never reached and the controller's mouse stays
    /// frozen until the connection dies.
    ///
    /// Returns true if the Enter was rejected.
    async fn reject_enter_if_emulation_unavailable(
        &mut self,
        addr: SocketAddr,
        serial: u32,
    ) -> bool {
        if self.emulation_proxy.emulation_active.get() {
            return false;
        }
        log::warn!("rejecting enter from {addr}: input emulation is unavailable");
        let session = if serial == 0 {
            SessionSerial::Legacy
        } else {
            SessionSerial::Scoped(serial)
        };
        self.listener
            .reply(addr, ProtoEvent::Leave(session.close_wire()))
            .await;
        true
    }
}

fn input_allowed(
    entered: &HashMap<SocketAddr, SessionSerial>,
    addr: SocketAddr,
    session: SessionSerial,
) -> bool {
    entered.get(&addr) == Some(&session)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnterDecision {
    Accept,
    Duplicate { ack: u32 },
    Upgrade { ack: u32 },
    Reject { leave: u32 },
}

fn enter_decision(
    entered: &HashMap<SocketAddr, SessionSerial>,
    accepting_control: bool,
    addr: SocketAddr,
    incoming: SessionSerial,
) -> EnterDecision {
    if let Some(current) = entered.get(&addr).copied() {
        if current == incoming {
            EnterDecision::Duplicate {
                ack: incoming.wire(),
            }
        } else if current == SessionSerial::Legacy && matches!(incoming, SessionSerial::Scoped(_)) {
            EnterDecision::Upgrade {
                ack: incoming.wire(),
            }
        } else {
            EnterDecision::Reject {
                leave: incoming.close_wire(),
            }
        }
    } else if accepting_control && entered.is_empty() {
        EnterDecision::Accept
    } else {
        EnterDecision::Reject {
            leave: incoming.close_wire(),
        }
    }
}

/// proxy handling the actual input emulation,
/// discarding events when it is disabled
pub(crate) struct EmulationProxy {
    emulation_active: Rc<Cell<bool>>,
    sessions: Rc<RefCell<HashMap<SocketAddr, u64>>>,
    next_generation: Cell<u64>,
    request_tx: Sender<ProxyRequest>,
    event_rx: Receiver<EmulationEvent>,
    task: TaskHandle,
}

enum ProxyRequest {
    Input(Event, SocketAddr, u64, Timestamp),
    /// place the cursor at `ratio` along the entered edge
    Enter(input_emulation::Position, f64, SocketAddr, u64),
    Remove(SocketAddr, u64),
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
        let sessions = Rc::new(RefCell::new(HashMap::new()));
        let emulation_task = EmulationTask {
            backend,
            cancellation_token: cancellation_token.clone(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
            sessions: sessions.clone(),
        };
        let task = TaskHandle::new(cancellation_token, spawn_local(emulation_task.run()));
        Self {
            emulation_active,
            sessions,
            next_generation: Cell::new(0),
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
        if !self.emulation_active.get() {
            observability::record_emulation_inactive_drop(&event);
            return;
        }
        let Some(generation) = self.sessions.borrow().get(&addr).copied() else {
            observability::record_emulation_inactive_drop(&event);
            return;
        };
        observability::injection_queue_push();
        send(
            &self.request_tx,
            "input event",
            ProxyRequest::Input(event, addr, generation, received_at),
        );
    }

    fn enter(&self, pos: input_emulation::Position, ratio: f64, addr: SocketAddr) {
        // ignore if emulation is currently disabled, same as input events
        if !self.emulation_active.get() {
            return;
        }
        let Some(generation) = self.sessions.borrow().get(&addr).copied() else {
            return;
        };
        send(
            &self.request_tx,
            "enter event",
            ProxyRequest::Enter(pos, ratio, addr, generation),
        );
    }

    fn activate(&self, addr: SocketAddr) {
        let generation = self.next_generation.get();
        self.next_generation.set(generation.wrapping_add(1));
        self.sessions.borrow_mut().insert(addr, generation);
    }

    fn remove(&self, addr: SocketAddr) {
        if let Some(generation) = self.sessions.borrow_mut().remove(&addr) {
            send(
                &self.request_tx,
                "peer removal",
                ProxyRequest::Remove(addr, generation),
            );
        }
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
    handles: HashMap<SocketAddr, (EmulationHandle, u64)>,
    next_id: EmulationHandle,
    sessions: Rc<RefCell<HashMap<SocketAddr, u64>>>,
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
                    ProxyRequest::Enter(..) => { /* emulation inactive => ignore */ }
                    ProxyRequest::Remove(addr, generation) => {
                        if self
                            .handles
                            .get(&addr)
                            .is_some_and(|(_, current)| *current == generation)
                        {
                            self.handles.remove(&addr);
                        }
                    }
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

        // Used to send enabled and disabled events. A dummy backend accepts
        // events and throws them away - reporting that as "enabled" would let
        // a peer enter this device and capture its mouse against a black
        // hole, with no way back because the local cursor never moves and so
        // never reaches the return barrier.
        let _emulation_guard = if emulation.can_emulate() {
            Some(DropGuard::new(
                self.event_tx.clone(),
                EmulationEvent::EmulationEnabled,
                EmulationEvent::EmulationDisabled,
            ))
        } else {
            log::warn!(
                "input emulation fell back to the {} backend and cannot move the cursor; \
                 peers will not be allowed to enter this device",
                emulation.backend()
            );
            None
        };

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

    async fn get_or_create_handle(
        &mut self,
        emulation: &mut InputEmulation,
        addr: SocketAddr,
        generation: u64,
    ) -> EmulationHandle {
        if let Some(&(handle, current)) = self.handles.get(&addr) {
            if current == generation {
                return handle;
            }
            self.handles.remove(&addr);
            emulation.destroy(handle).await;
        }

        let handle = self.next_id;
        self.next_id += 1;
        emulation.create(handle).await;
        self.handles.insert(addr, (handle, generation));
        handle
    }

    async fn create_clients(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        for (handle, _) in self.handles.values() {
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
                    ProxyRequest::Input(event, addr, generation, received_at) => {
                        if !self.session_is_current(addr, generation) {
                            observability::record_emulation_inactive_drop(&event);
                            continue;
                        }
                        let handle = self
                            .get_or_create_handle(&mut *emulation, addr, generation)
                            .await;
                        let result = emulation.consume(event, handle).await;
                        observability::record_receive_to_inject(received_at);
                        result?;
                    },
                    ProxyRequest::Enter(pos, ratio, addr, generation) => {
                        if !self.session_is_current(addr, generation) {
                            continue;
                        }
                        let handle = self
                            .get_or_create_handle(&mut *emulation, addr, generation)
                            .await;
                        emulation.enter(handle, pos, ratio).await;
                    }
                    ProxyRequest::Remove(addr, generation) => {
                        if let Some((handle, current)) = self.handles.get(&addr).copied() {
                            if current == generation {
                                self.handles.remove(&addr);
                                emulation.destroy(handle).await;
                            }
                        }
                    }
                    ProxyRequest::Reenable => continue,
                }},
            }
        }
    }

    fn session_is_current(&self, addr: SocketAddr, generation: u64) -> bool {
        self.sessions
            .borrow()
            .get(&addr)
            .is_some_and(|current| *current == generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_accepted_only_while_peer_is_entered() {
        let addr = "127.0.0.1:4242".parse().expect("valid socket address");
        let mut entered = HashMap::new();

        assert!(!input_allowed(&entered, addr, SessionSerial::Legacy));
        entered.insert(addr, SessionSerial::Legacy);
        assert!(input_allowed(&entered, addr, SessionSerial::Legacy));
        assert!(!input_allowed(&entered, addr, SessionSerial::Scoped(1)));
        entered.insert(addr, SessionSerial::Scoped(7));
        assert!(input_allowed(&entered, addr, SessionSerial::Scoped(7)));
        assert!(!input_allowed(&entered, addr, SessionSerial::Scoped(6)));
        assert!(!input_allowed(&entered, addr, SessionSerial::Legacy));
        entered.remove(&addr);
        assert!(!input_allowed(&entered, addr, SessionSerial::Legacy));
    }

    #[test]
    fn only_one_controller_can_enter_and_retries_are_idempotent() {
        let first = "127.0.0.1:4242".parse().expect("valid socket address");
        let second = "127.0.0.1:4243".parse().expect("valid socket address");
        let mut entered = HashMap::new();

        assert_eq!(
            enter_decision(&entered, true, first, SessionSerial::Legacy),
            EnterDecision::Accept
        );
        entered.insert(first, SessionSerial::Legacy);
        assert_eq!(
            enter_decision(&entered, true, first, SessionSerial::Legacy),
            EnterDecision::Duplicate { ack: 0 }
        );
        assert_eq!(
            enter_decision(&entered, true, first, SessionSerial::Scoped(7)),
            EnterDecision::Upgrade { ack: 7 }
        );
        entered.insert(first, SessionSerial::Scoped(7));
        assert_eq!(
            enter_decision(&entered, true, first, SessionSerial::Scoped(6)),
            EnterDecision::Reject {
                leave: CONTROL_SESSION_CLOSE_BIT | 6,
            }
        );
        assert_eq!(
            enter_decision(&entered, true, second, SessionSerial::Scoped(9)),
            EnterDecision::Reject {
                leave: CONTROL_SESSION_CLOSE_BIT | 9,
            }
        );
        assert_eq!(
            enter_decision(&HashMap::new(), false, first, SessionSerial::Scoped(11),),
            EnterDecision::Reject {
                leave: CONTROL_SESSION_CLOSE_BIT | 11,
            }
        );
    }

    /// A dummy backend accepts every event and throws it away. If the service
    /// announced that as enabled, a peer would be allowed to enter this
    /// device and its own cursor would be captured against a black hole: the
    /// local cursor never moves, so it never reaches the barrier that hands
    /// control back, and the peer's mouse and keyboard stay frozen.
    #[tokio::test]
    async fn a_dummy_backend_is_never_announced_as_enabled() {
        let (event_tx, mut event_rx) = channel();
        let (request_tx, request_rx) = channel();
        // closing the request channel ends the session loop immediately
        drop(request_tx);

        let mut task = EmulationTask {
            backend: Some(input_emulation::Backend::Dummy),
            cancellation_token: CancellationToken::new(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
            sessions: Default::default(),
        };
        task.do_emulation().await.expect("dummy emulation session");

        assert!(
            event_rx.try_recv().is_err(),
            "the dummy backend must not report emulation as enabled"
        );
    }
}

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use futures::StreamExt;
use input_capture::{
    CaptureError, CaptureEvent, CaptureHandle, InputCapture, InputCaptureError, Position,
};
use input_event::{Event, KeyboardEvent, scancode};
use lan_mouse_ipc::ClientHandle;
use lan_mouse_proto::{
    CAPABILITY_CONTROL_SESSION, CAPABILITY_ENTER_POSITION, CONTROL_SESSION_CLOSE_BIT, ProtoEvent,
    WireEvent,
};
use tokio::task::spawn_local;
use tokio_util::sync::CancellationToken;

use crate::{
    connect::Connection,
    observability::{self, Timestamp},
    position::{capture_to_proto, ipc_to_capture},
    service::control::ControlArbiter,
    task::{DropGuard, Receiver, Sender, TaskHandle, channel, send},
};

/// minimum time between two "releasing capture" warnings, so a peer that is
/// down does not flood the log with one line per captured input event
const RELEASE_LOG_DEBOUNCE: Duration = Duration::from_millis(500);

/// how long the capture may stay in [`State::WaitingForAck`] before it is
/// force-released. Without this, a peer that receives our Enter but never
/// acknowledges it would leave the local mouse captured (and frozen)
/// indefinitely - the input black-hole can only be escaped via the release
/// bind, which users may not know.
const ENTER_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// Enter is a synchronization packet, not a mouse-motion packet. Repeating it
/// for every captured input event turns a slow link into an Enter/Ack storm and
/// can keep macOS' local-input suppression window permanently active.
const ENTER_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const LEAVE_ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// how often to re-send the current modifier state to the active peer while
/// sending input. Key events travel over UDP/DTLS and can be dropped; without
/// periodic re-sync a lost modifier key-up (especially Control on macOS,
/// where Control+Click = right-click) leaves the peer with a stuck modifier
/// until the next FlagsChanged happens to arrive or the peer times out.
const MODIFIER_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug)]
struct PendingOutboundLeave {
    event: ProtoEvent,
    serial: u32,
    scoped: bool,
    started_at: Instant,
    last_sent: Instant,
}

pub(crate) struct Capture {
    task: TaskHandle,
    request_tx: Sender<CaptureRequest>,
    event_rx: Receiver<ICaptureEvent>,
}

/// What a capture barrier belongs to.
///
/// `input-capture` identifies barriers by a bare `u64`, and both outgoing
/// clients and incoming connections need one. They share that number space by
/// convention: client handles (slab indices) count up from zero, triggers for
/// incoming connections count up from the middle. This type makes the
/// convention explicit and keeps the raw encoding in one place - the encoding
/// itself is unchanged, so the capture backends see exactly what they did
/// before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureTarget {
    /// a configured client we send input to
    Client(ClientHandle),
    /// an enter-only barrier for a peer that connected to us
    IncomingTrigger(u64),
}

/// first handle reserved for incoming connections
const INCOMING_TRIGGER_BEGIN: u64 = u64::MAX / 2 + 1;

impl CaptureTarget {
    pub(crate) fn to_raw(self) -> CaptureHandle {
        match self {
            Self::Client(handle) => handle,
            Self::IncomingTrigger(n) => INCOMING_TRIGGER_BEGIN.wrapping_add(n),
        }
    }

    pub(crate) fn from_raw(handle: CaptureHandle) -> Self {
        if handle >= INCOMING_TRIGGER_BEGIN {
            Self::IncomingTrigger(handle - INCOMING_TRIGGER_BEGIN)
        } else {
            Self::Client(handle)
        }
    }
}

pub(crate) enum ICaptureEvent {
    /// a capture barrier was entered; `ratio` is the crossing point along
    /// the barrier edge (normalized, top/left = 0.0), if known
    CaptureBegin {
        target: CaptureTarget,
        ratio: Option<f64>,
    },
    /// capture disabled
    CaptureDisabled,
    /// capture disabled
    CaptureEnabled,
    /// A (new) client was entered.
    /// In contrast to [`ICaptureEvent::CaptureBegin`] this
    /// event is only triggered when the capture was
    /// explicitly released in the meantime by
    /// either the remote client leaving its device region,
    /// a new device entering the screen or the release bind.
    ClientEntered(ClientHandle),
    /// The local capture for a client has been fully released.
    ClientLeft(ClientHandle),
    ClipboardText(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureType {
    /// a normal input capture
    Default,
    /// A capture only interested in [`CaptureEvent::Begin`] events.
    /// The capture is released immediately, if there is no
    /// Default capture at the same position.
    EnterOnly,
}

#[derive(Clone, Debug)]
enum CaptureRequest {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position, CaptureType),
    /// destory a capture client
    Destroy(CaptureHandle),
    /// reenable input capture
    Reenable,
    /// set release bind
    SetReleaseBind(Vec<scancode::Linux>),
    /// update the cached local clipboard and optionally send it now
    SetClipboard {
        text: Option<String>,
        broadcast: bool,
    },
}

impl Capture {
    pub(crate) fn new(
        backend: Option<input_capture::Backend>,
        conn: Connection,
        release_bind: Vec<scancode::Linux>,
        control_arbiter: ControlArbiter,
    ) -> Self {
        observability::start_reporter();
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let cancellation_token = CancellationToken::new();
        let capture_task = CaptureTask {
            active_client: None,
            backend,
            cancellation_token: cancellation_token.clone(),
            captures: Default::default(),
            conn,
            event_tx,
            request_rx,
            release_bind: Rc::new(RefCell::new(release_bind)),
            state: Default::default(),
            switch_started_at: None,
            clipboard_text: None,
            active_ratio: None,
            waiting_for_ack_since: None,
            last_enter_sent_at: None,
            control_arbiter,
            pending_failed_release: None,
            pending_leaves: Default::default(),
            active_serial: None,
            next_serial: 1,
            active_scoped: false,
        };
        let task = TaskHandle::new(cancellation_token, spawn_local(capture_task.run()));
        Self {
            task,
            request_tx,
            event_rx,
        }
    }

    pub(crate) fn reenable(&self) {
        send(
            &self.request_tx,
            "capture reenable",
            CaptureRequest::Reenable,
        );
    }

    pub(crate) async fn terminate(&mut self) {
        self.task.terminate("input capture").await;
    }

    pub(crate) fn create(
        &self,
        target: CaptureTarget,
        pos: lan_mouse_ipc::Position,
        capture_type: CaptureType,
    ) {
        let pos = ipc_to_capture(pos);
        send(
            &self.request_tx,
            "capture create",
            CaptureRequest::Create(target.to_raw(), pos, capture_type),
        );
    }

    pub(crate) fn destroy(&self, target: CaptureTarget) {
        send(
            &self.request_tx,
            "capture destroy",
            CaptureRequest::Destroy(target.to_raw()),
        );
    }

    pub(crate) fn release(&self) {
        send(&self.request_tx, "capture release", CaptureRequest::Release);
    }

    /// The next capture event, or `None` once the capture task has stopped.
    pub(crate) async fn event(&mut self) -> Option<ICaptureEvent> {
        self.event_rx.recv().await
    }

    pub(crate) fn set_release_bind(&mut self, bind: Vec<scancode::Linux>) {
        send(
            &self.request_tx,
            "release bind",
            CaptureRequest::SetReleaseBind(bind),
        );
    }

    pub(crate) fn set_clipboard(&self, text: Option<String>, broadcast: bool) {
        send(
            &self.request_tx,
            "clipboard update",
            CaptureRequest::SetClipboard { text, broadcast },
        );
    }
}

/// debounce a statement `$st`, i.e. the statement is executed only if the
/// time since the previous execution is at least `$dur`.
/// `$prev` is used to keep track of this timestamp
macro_rules! debounce {
    ($prev:ident, $dur:expr, $st:stmt) => {
        let exec = match $prev.get() {
            None => true,
            Some(instant) if instant.elapsed() > $dur => true,
            _ => false,
        };
        if exec {
            $prev.replace(Some(Instant::now()));
            $st
        }
    };
}

struct CaptureTask {
    active_client: Option<CaptureHandle>,
    backend: Option<input_capture::Backend>,
    cancellation_token: CancellationToken,
    captures: Vec<(CaptureHandle, Position, CaptureType)>,
    conn: Connection,
    event_tx: Sender<ICaptureEvent>,
    release_bind: Rc<RefCell<Vec<scancode::Linux>>>,
    request_rx: Receiver<CaptureRequest>,
    state: State,
    switch_started_at: Option<Timestamp>,
    clipboard_text: Option<String>,
    /// where the active client was entered along the barrier edge
    /// (normalized); used for Enter retransmissions while waiting for Ack
    active_ratio: Option<f64>,
    /// when the task entered [`State::WaitingForAck`]; releases the capture
    /// after [`ENTER_ACK_TIMEOUT`]
    waiting_for_ack_since: Option<Instant>,
    /// last Enter/EnterAt transmission while waiting for the peer's Ack
    last_enter_sent_at: Option<Instant>,
    /// synchronous direction ownership shared with the emulation task
    control_arbiter: ControlArbiter,
    /// client whose backend release failed; notified after backend teardown
    pending_failed_release: Option<ClientHandle>,
    /// Locally initiated Leave packets awaiting the peer's Ack.
    pending_leaves: std::collections::HashMap<ClientHandle, PendingOutboundLeave>,
    active_serial: Option<u32>,
    next_serial: u32,
    active_scoped: bool,
}

impl CaptureTask {
    fn add_capture(&mut self, handle: CaptureHandle, pos: Position, capture_type: CaptureType) {
        if let Some(existing) = self.captures.iter_mut().find(|(h, ..)| *h == handle) {
            *existing = (handle, pos, capture_type);
        } else {
            self.captures.push((handle, pos, capture_type));
        }
    }

    fn remove_capture(&mut self, handle: CaptureHandle) {
        self.captures.retain(|&(h, ..)| handle != h);
    }

    fn is_default_capture_at(&self, pos: Position) -> bool {
        self.captures
            .iter()
            .any(|&(_, p, t)| p == pos && t == CaptureType::Default)
    }

    /// position and type of a registered capture, or `None` if the capture was
    /// already destroyed
    fn get_capture(&self, handle: CaptureHandle) -> Option<(Position, CaptureType)> {
        self.captures
            .iter()
            .find(|(h, ..)| *h == handle)
            .map(|&(_, pos, capture_type)| (pos, capture_type))
    }

    fn set_state(&mut self, state: State, reason: &'static str) {
        log::debug!(
            target: "crossdesk::state",
            "capture_state from={:?} to={state:?} reason={reason} client={:?}",
            self.state,
            self.active_client,
        );
        self.state = state;
    }

    async fn run(mut self) {
        loop {
            if let Err(e) = self.do_capture().await {
                log::warn!("input capture exited: {e}");
            }
            let mut close_tick = tokio::time::interval(ENTER_RETRY_INTERVAL);
            close_tick.tick().await;
            loop {
                tokio::select! {
                    r = self.request_rx.recv() => match r {
                        None => return,
                        Some(CaptureRequest::Reenable) => break,
                        Some(CaptureRequest::Create(h, p, t)) => self.add_capture(h, p, t),
                        Some(CaptureRequest::Destroy(h)) => {
                            self.remove_capture(h);
                            self.pending_leaves.remove(&h);
                        }
                        Some(CaptureRequest::Release) => { /* nothing to do */ }
                        Some(CaptureRequest::SetReleaseBind(bind)) => {
                            self.release_bind.borrow_mut().clone_from(&bind);
                        }
                        Some(CaptureRequest::SetClipboard { text, .. }) => {
                            self.clipboard_text = text;
                        }
                    },
                    Some((handle, event)) = self.conn.recv() => {
                        self.handle_disabled_wire_event(handle, event).await;
                    }
                    _ = close_tick.tick() => self.drive_pending_leaves().await,
                    _ = self.cancellation_token.cancelled() => return,
                }
            }
        }
    }

    async fn do_capture(&mut self) -> Result<(), InputCaptureError> {
        /* allow cancelling capture request */
        let mut capture = tokio::select! {
            r = InputCapture::new(self.backend) => r?,
            _ = self.cancellation_token.cancelled() => return Ok(()),
        };

        let _capture_guard = DropGuard::new(
            self.event_tx.clone(),
            ICaptureEvent::CaptureEnabled,
            ICaptureEvent::CaptureDisabled,
        );

        /* create barriers for active clients */
        let r = self.create_captures(&mut capture).await;
        if let Err(e) = r {
            capture.terminate().await?;
            return Err(e.into());
        }

        let mut r = self.do_capture_session(&mut capture).await;

        // A backend error must not leave the global direction arbiter or the
        // frontend role stuck in Controlling. Best-effort cleanup still sends
        // Leave and a ClientLeft notification before the backend is rebuilt.
        if self.active_client.is_some() {
            let notify_peer = self.remote_session_started();
            if let Err(error) = self.release_capture(&mut capture, None, notify_peer).await {
                log::warn!("failed to release capture after backend exit: {error}");
                if r.is_ok() {
                    r = Err(error.into());
                }
            }
        }

        // FIXME replace with async drop when stabilized
        let terminate_result = capture.terminate().await;
        // A failed backend release keeps ownership closed until teardown is
        // complete; only now is it safe for incoming emulation to acquire it.
        self.control_arbiter.release_outgoing();
        if let Some(handle) = self.pending_failed_release.take() {
            send(
                &self.event_tx,
                "client left after backend teardown",
                ICaptureEvent::ClientLeft(handle),
            );
        }
        terminate_result?;

        r
    }

    async fn create_captures(&mut self, capture: &mut InputCapture) -> Result<(), CaptureError> {
        let captures = self.captures.clone();
        for (handle, pos, _type) in captures {
            tokio::select! {
                r = capture.create(handle, pos) => r?,
                _ = self.cancellation_token.cancelled() => return Ok(()),
            }
        }
        Ok(())
    }

    async fn do_capture_session(
        &mut self,
        capture: &mut InputCapture,
    ) -> Result<(), InputCaptureError> {
        let mut heartbeat = tokio::time::interval(MODIFIER_HEARTBEAT_INTERVAL);
        let mut handoff_tick = tokio::time::interval(ENTER_RETRY_INTERVAL);
        // skip the immediate first tick — no events have been captured yet
        heartbeat.tick().await;
        handoff_tick.tick().await;
        loop {
            tokio::select! {
                event = capture.next() => match event {
                    Some(event) => self.handle_capture_event(capture, event?).await?,
                    None => return Ok(()),
                },
                Some((handle, event)) = self.conn.recv() => {
                    let event = match event {
                        WireEvent::Protocol(event) => event,
                        WireEvent::ClipboardText(text) => {
                            send(
                                &self.event_tx,
                                "remote clipboard text",
                                ICaptureEvent::ClipboardText(text),
                            );
                            continue;
                        }
                    };

                    if let ProtoEvent::Ack(serial) = event {
                        if let Some(pending) = self.pending_leaves.get(&handle).copied() {
                            if pending.serial == serial {
                                self.pending_leaves.remove(&handle);
                                if pending.scoped {
                                    log::debug!("client {handle} acknowledged Leave({serial})");
                                } else {
                                    // Legacy Enter and Leave both use Ack(0), so
                                    // the phase cannot be proven. Roll the DTLS
                                    // epoch immediately; either interpretation is
                                    // then safe and no delayed packet can reach the
                                    // next session.
                                    self.conn.close(handle).await;
                                }
                                continue;
                            }
                        }
                    }
                    if let ProtoEvent::Leave(serial) | ProtoEvent::LeaveAt { serial, .. } = event {
                        if let Some(pending) = self.pending_leaves.get(&handle).copied() {
                            if pending.serial == serial {
                                self.pending_leaves.remove(&handle);
                                if !pending.scoped {
                                    let _ = self.conn.send(ProtoEvent::Ack(serial), handle).await;
                                    self.conn.close(handle).await;
                                    continue;
                                }
                            }
                        }
                    }
                    match event {
                        // connection acknowlegded => set state to Sending
                        ProtoEvent::Ack(serial) if self.active_client == Some(handle)
                            && self.state == State::WaitingForAck
                            && self.ack_matches_active(serial) => {
                            log::info!("client {handle} acknowledged the connection!");
                            self.waiting_for_ack_since = None;
                            self.last_enter_sent_at = None;
                            self.set_state(State::Sending, "enter_acknowledged");
                            self.send_modifier_snapshot(capture, handle).await;
                            if let Some(started_at) = self.switch_started_at.take() {
                                observability::record_switch_ack(started_at);
                            }
                            self.send_clipboard_to(handle).await;
                        }
                        ProtoEvent::Ack(_) => {
                            log::debug!("ignoring stale Ack from client {handle}");
                        }
                        // client disconnected
                        ProtoEvent::Leave(serial)
                            if self.leave_matches_active(handle, serial) => {
                            log::info!("releasing capture: left remote client device region");
                            self.release_capture(capture, None, true).await?;
                            let _ = self.conn.send(ProtoEvent::Ack(serial), handle).await;
                        },
                        ProtoEvent::LeaveAt { serial, ratio }
                            if self.leave_matches_active(handle, serial) => {
                            log::info!("releasing capture: left remote client device region at {ratio:.3}");
                            let ratio = ratio.is_finite().then(|| ratio.clamp(0.0, 1.0));
                            self.release_capture(capture, ratio, true).await?;
                            let _ = self.conn.send(ProtoEvent::Ack(serial), handle).await;
                        },
                        ProtoEvent::Leave(serial) => {
                            let _ = self.conn.send(ProtoEvent::Ack(serial), handle).await;
                        }
                        ProtoEvent::LeaveAt { serial, .. } => {
                            let _ = self.conn.send(ProtoEvent::Ack(serial), handle).await;
                        },
                        _ => {}
                    }
                },
                e = self.request_rx.recv() => match e {
                    None => return Ok(()),
                    Some(CaptureRequest::Reenable) => { /* already active */ },
                    Some(CaptureRequest::Release) => {
                        let notify_peer = self.remote_session_started();
                        self.release_capture(capture, None, notify_peer).await?
                    },
                    Some(CaptureRequest::Create(h, p, t)) => {
                        self.add_capture(h, p, t);
                        capture.create(h, p).await?;
                    }
                    Some(CaptureRequest::Destroy(h)) => {
                        self.remove_capture(h);
                        self.pending_leaves.remove(&h);
                        let release_result = if self.active_client == Some(h) {
                            let notify_peer = self.remote_session_started();
                            self.release_capture(capture, None, notify_peer).await
                        } else {
                            Ok(())
                        };
                        let destroy_result = capture.destroy(h).await;
                        // A destroyed ClientHandle may be reused by the slab.
                        // Never let a close retry outlive its logical client
                        // and target a future peer with the same numeric id.
                        self.pending_leaves.remove(&h);
                        release_result?;
                        destroy_result?;
                    }
                    Some(CaptureRequest::SetReleaseBind(bind)) => {
                        self.release_bind.borrow_mut().clone_from(&bind);
                    }
                    Some(CaptureRequest::SetClipboard { text, broadcast }) => {
                        self.clipboard_text = text;
                        if broadcast {
                            if let Some(text) = self.clipboard_text.as_deref() {
                                self.conn.broadcast_clipboard(text).await;
                            }
                        }
                    }
                },
                _ = heartbeat.tick() => {
                    // Re-send the current modifier state so a dropped key-up
                    // over UDP doesn't leave the peer with a stuck modifier.
                    // Only needed while actively sending input.
                    if self.state == State::Sending {
                        if let Some(handle) = self.active_client {
                            self.send_modifier_snapshot(capture, handle).await;
                        }
                    }
                },
                _ = handoff_tick.tick() => {
                    self.drive_pending_leaves().await;

                    if self.state != State::WaitingForAck {
                        continue;
                    }

                    if self
                        .waiting_for_ack_since
                        .is_some_and(|since| since.elapsed() >= ENTER_ACK_TIMEOUT)
                    {
                        if let Some(handle) = self.active_client {
                            log::warn!(
                                "releasing capture: client {handle} did not acknowledge the connection within {ENTER_ACK_TIMEOUT:?}"
                            );
                        }
                        self.release_capture(capture, None, true).await?;
                        continue;
                    }

                    if self
                        .last_enter_sent_at
                        .is_none_or(|sent| sent.elapsed() >= ENTER_RETRY_INTERVAL)
                    {
                        if let Some((handle, event)) = self.current_enter_event() {
                            match self.conn.send(event, handle).await {
                                Ok(()) => {
                                    self.last_enter_sent_at = Some(Instant::now());
                                    if matches!(event, ProtoEvent::EnterSession { .. }) {
                                        self.active_scoped = true;
                                    }
                                }
                                Err(error) => {
                                    debounce!(
                                        PREV_LOG,
                                        RELEASE_LOG_DEBOUNCE,
                                        log::warn!("releasing capture: {error}")
                                    );
                                    let notify_peer = self.remote_session_started();
                                    self.release_capture(capture, None, notify_peer).await?;
                                }
                            }
                        }
                    }
                },
                _ = self.cancellation_token.cancelled() => break,
            }
        }
        Ok(())
    }

    async fn handle_capture_event(
        &mut self,
        capture: &mut InputCapture,
        event: (CaptureHandle, CaptureEvent),
    ) -> Result<(), CaptureError> {
        let captured_at = Timestamp::now();
        let (handle, event) = event;
        let is_input = matches!(event, CaptureEvent::Input(_));
        log::trace!("({handle}): {event:?}");

        if capture.keys_pressed(&self.release_bind.borrow()) {
            log::info!("releasing capture: release-bind pressed");
            let notify_peer = self.remote_session_started();
            return self.release_capture(capture, None, notify_peer).await;
        }

        // The backend can still deliver events for a handle we just destroyed;
        // there is nothing left to route them to.
        let Some((pos, capture_type)) = self.get_capture(handle) else {
            log::debug!("ignoring event for unregistered capture {handle}");
            return Ok(());
        };

        if let CaptureEvent::Begin { ratio } = event {
            send(
                &self.event_tx,
                "capture begin",
                ICaptureEvent::CaptureBegin {
                    target: CaptureTarget::from_raw(handle),
                    ratio,
                },
            );
        }

        // enter only capture (for incoming connections)
        if capture_type == CaptureType::EnterOnly {
            // if there is no active outgoing connection at the current capture,
            // we release the capture
            if !self.is_default_capture_at(pos) {
                log::info!("releasing capture: no active client at this position");
                capture.release(None).await?;
            }
            // we dont care about events from incoming handles except for releasing the capture
            return Ok(());
        }

        // activated a new client
        if let CaptureEvent::Begin { ratio } = event {
            self.active_ratio = ratio;
            if Some(handle) != self.active_client {
                if self.pending_leaves.contains_key(&handle) {
                    log::debug!(
                        "holding client {handle} at the edge until its previous Leave is acknowledged"
                    );
                    capture.release(None).await?;
                    return Ok(());
                }
                if !self.control_arbiter.try_acquire_outgoing() {
                    log::warn!(
                        "ignoring outgoing edge for client {handle}: incoming control owns the session"
                    );
                    capture.release(None).await?;
                    return Ok(());
                }
                let serial = self.next_serial;
                self.next_serial = self.next_serial.wrapping_add(1) & !CONTROL_SESSION_CLOSE_BIT;
                if self.next_serial == 0 {
                    self.next_serial = 1;
                }
                self.active_serial = Some(serial);
                self.active_scoped = false;
                self.active_client.replace(handle);
                self.switch_started_at = Some(Timestamp::now());
                self.waiting_for_ack_since = Some(Instant::now());
                self.last_enter_sent_at = None;
                self.set_state(State::WaitingForAck, "edge_entered");
                send(
                    &self.event_tx,
                    "client entered",
                    ICaptureEvent::ClientEntered(handle),
                );
            }
        }

        let event = match event {
            CaptureEvent::Begin { .. } => self
                .current_enter_event()
                .map(|(_, event)| event)
                .unwrap_or_else(|| ProtoEvent::Enter(capture_to_proto(pos.opposite()))),
            CaptureEvent::Input(e) => match self.state {
                // A bounded timer retransmits Enter. Motion and clicks are
                // deliberately held until the peer acknowledges the handoff.
                State::WaitingForAck | State::Idle => return Ok(()),
                State::Sending => self.current_input_event(e),
            },
        };

        match self.conn.send(event, handle).await {
            Ok(()) => {
                if matches!(
                    event,
                    ProtoEvent::Enter(_)
                        | ProtoEvent::EnterAt { .. }
                        | ProtoEvent::EnterSession { .. }
                ) {
                    self.last_enter_sent_at = Some(Instant::now());
                    if matches!(event, ProtoEvent::EnterSession { .. }) {
                        self.active_scoped = true;
                    }
                }
                if is_input {
                    observability::record_capture_to_send(captured_at);
                }
            }
            Err(e) => {
                debounce!(
                    PREV_LOG,
                    RELEASE_LOG_DEBOUNCE,
                    log::warn!("releasing capture: {e}")
                );
                let notify_peer = self.remote_session_started();
                self.release_capture(capture, None, notify_peer).await?;
            }
        }
        Ok(())
    }

    fn current_enter_event(&self) -> Option<(CaptureHandle, ProtoEvent)> {
        let handle = self.active_client?;
        let (pos, capture_type) = self.get_capture(handle)?;
        if capture_type != CaptureType::Default {
            return None;
        }
        let pos = capture_to_proto(pos.opposite());
        let event = if self.conn.supports(handle, CAPABILITY_CONTROL_SESSION) {
            ProtoEvent::EnterSession {
                pos,
                serial: self.active_serial?,
                ratio: self.active_ratio.unwrap_or(0.5),
            }
        } else {
            match self.active_ratio {
                Some(ratio) if self.conn.supports(handle, CAPABILITY_ENTER_POSITION) => {
                    ProtoEvent::EnterAt { pos, ratio }
                }
                _ => ProtoEvent::Enter(pos),
            }
        };
        Some((handle, event))
    }

    fn remote_session_started(&self) -> bool {
        should_notify_peer(self.state, self.last_enter_sent_at)
    }

    fn ack_matches_active(&self, serial: u32) -> bool {
        session_serial_matches(self.active_scoped, self.active_serial, serial)
    }

    fn leave_matches_active(&self, handle: ClientHandle, serial: u32) -> bool {
        self.active_client == Some(handle)
            && close_serial_matches(self.active_scoped, self.active_serial, serial)
    }

    fn current_input_event(&self, event: Event) -> ProtoEvent {
        if self.active_scoped {
            ProtoEvent::InputSession {
                serial: self.active_serial.unwrap_or(0),
                event,
            }
        } else {
            ProtoEvent::Input(event)
        }
    }

    fn queue_pending_leave(
        &mut self,
        handle: ClientHandle,
        event: ProtoEvent,
        serial: u32,
        scoped: bool,
    ) {
        self.pending_leaves.insert(
            handle,
            PendingOutboundLeave {
                event,
                serial,
                scoped,
                started_at: Instant::now(),
                last_sent: Instant::now(),
            },
        );
    }

    async fn drive_pending_leaves(&mut self) {
        let expired = self
            .pending_leaves
            .iter()
            .filter_map(|(&handle, pending)| {
                (pending.started_at.elapsed() >= LEAVE_ACK_TIMEOUT).then_some(handle)
            })
            .collect::<Vec<_>>();
        for handle in expired {
            log::warn!(
                "closing client {handle}: Leave was not acknowledged within {LEAVE_ACK_TIMEOUT:?}"
            );
            self.pending_leaves.remove(&handle);
            self.conn.close(handle).await;
        }

        let retries = self
            .pending_leaves
            .iter_mut()
            .filter_map(|(&handle, pending)| {
                if pending.last_sent.elapsed() >= ENTER_RETRY_INTERVAL {
                    pending.last_sent = Instant::now();
                    Some((handle, pending.event))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (handle, event) in retries {
            let _ = self.conn.send(event, handle).await;
        }
    }

    async fn handle_disabled_wire_event(&mut self, handle: ClientHandle, event: WireEvent) {
        let event = match event {
            WireEvent::Protocol(event) => event,
            WireEvent::ClipboardText(text) => {
                send(
                    &self.event_tx,
                    "remote clipboard text",
                    ICaptureEvent::ClipboardText(text),
                );
                return;
            }
        };

        match event {
            ProtoEvent::Ack(serial) => {
                if let Some(pending) = self.pending_leaves.get(&handle).copied() {
                    if pending.serial == serial {
                        self.pending_leaves.remove(&handle);
                        if !pending.scoped {
                            self.conn.close(handle).await;
                        }
                    }
                }
            }
            ProtoEvent::Leave(serial) | ProtoEvent::LeaveAt { serial, .. } => {
                let pending = self.pending_leaves.get(&handle).copied();
                let confirms_pending = pending.is_some_and(|pending| pending.serial == serial);
                if confirms_pending {
                    self.pending_leaves.remove(&handle);
                }
                let _ = self.conn.send(ProtoEvent::Ack(serial), handle).await;
                if pending.is_some_and(|pending| !pending.scoped && pending.serial == serial) {
                    self.conn.close(handle).await;
                    return;
                }
                if !confirms_pending {
                    let leave = ProtoEvent::Leave(serial);
                    self.queue_pending_leave(
                        handle,
                        leave,
                        serial,
                        serial & CONTROL_SESSION_CLOSE_BIT != 0,
                    );
                    let _ = self.conn.send(leave, handle).await;
                }
            }
            _ => {}
        }
    }

    async fn release_capture(
        &mut self,
        capture: &mut InputCapture,
        edge_ratio: Option<f64>,
        notify_peer: bool,
    ) -> Result<(), CaptureError> {
        self.switch_started_at = None;
        self.active_ratio = None;
        self.waiting_for_ack_since = None;
        self.last_enter_sent_at = None;
        let scoped = self.active_scoped;
        let session_serial = self.active_serial.unwrap_or(0);
        let serial = if scoped {
            session_serial | CONTROL_SESSION_CLOSE_BIT
        } else {
            0
        };
        self.active_serial = None;
        self.active_scoped = false;
        self.set_state(State::Idle, "capture_released");
        // If we have an active client, notify them we're leaving
        if let Some(handle) = self.active_client.take() {
            let pressed_keys = capture.take_pressed_keys();

            // Restore local input before doing any network I/O. In particular,
            // a slow or dead peer must never keep the macOS event tap in its
            // drop-and-warp state while cleanup packets are attempted.
            let release_result = capture.release(edge_ratio).await;
            if release_result.is_ok() {
                self.control_arbiter.release_outgoing();
            } else {
                self.pending_failed_release = Some(handle);
            }

            // Synthesize key-up events for every key still held in the
            // capture's pressed_keys set BEFORE sending Leave. Without
            // this, pressing the release-bind chord (typically all four
            // modifiers) leaves the peer with phantom held modifiers:
            // the down events were forwarded while capture was active,
            // but the matching up events arrive after the local tap
            // flips to passthrough and never reach the peer. The peer
            // then runs every subsequent keystroke through those held
            // mods until its watchdog times out (1+ s) or our Leave
            // arrives — and Leave can be lost over UDP/DTLS.
            for key in pressed_keys {
                let key_up = session_input(
                    scoped,
                    session_serial,
                    Event::Keyboard(KeyboardEvent::Key {
                        time: 0,
                        key: key as u32,
                        state: 0,
                    }),
                );
                if let Err(e) = self.conn.send(key_up, handle).await {
                    log::warn!("failed to send key-up to client {handle}: {e}");
                }
            }
            // Reset the modifier mask too. The peer's input-emulation
            // layer keeps a separate XKB-style modifier state that's
            // updated by KeyboardEvent::Modifiers, distinct from the
            // pressed_keys set drained above. Without this, an
            // already-locked CapsLock would survive the release.
            let mods_zero = session_input(
                scoped,
                session_serial,
                Event::Keyboard(KeyboardEvent::Modifiers {
                    depressed: 0,
                    latched: 0,
                    locked: 0,
                    group: 0,
                }),
            );
            if let Err(e) = self.conn.send(mods_zero, handle).await {
                log::warn!("failed to reset modifiers on client {handle}: {e}");
            }

            if notify_peer {
                let leave = match edge_ratio {
                    Some(ratio) if self.conn.supports(handle, CAPABILITY_ENTER_POSITION) => {
                        ProtoEvent::LeaveAt { serial, ratio }
                    }
                    _ => ProtoEvent::Leave(serial),
                };
                self.queue_pending_leave(handle, leave, serial, scoped);
                log::info!("sending Leave event to client {handle}");
                if let Err(e) = self.conn.send(leave, handle).await {
                    log::warn!("failed to send Leave to client {handle}: {e}");
                }
            }
            if release_result.is_ok() {
                send(
                    &self.event_tx,
                    "client left",
                    ICaptureEvent::ClientLeft(handle),
                );
            }
            release_result
        } else {
            capture.release(edge_ratio).await
        }
    }

    async fn send_clipboard_to(&self, handle: CaptureHandle) {
        let Some(text) = self.clipboard_text.as_deref() else {
            return;
        };
        if let Err(error) = self.conn.send_clipboard(text, handle).await {
            log::debug!("clipboard text was not sent to client {handle}: {error}");
        }
    }

    async fn send_modifier_snapshot(&self, capture: &InputCapture, handle: CaptureHandle) {
        let (depressed, latched, locked, group) = capture.modifier_state();
        let event = self.current_input_event(Event::Keyboard(KeyboardEvent::Modifiers {
            depressed,
            latched,
            locked,
            group,
        }));
        if let Err(error) = self.conn.send(event, handle).await {
            log::debug!("failed to send modifier snapshot: {error}");
        }
    }
}

thread_local! {
    static PREV_LOG: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn session_input(scoped: bool, serial: u32, event: Event) -> ProtoEvent {
    if scoped {
        ProtoEvent::InputSession { serial, event }
    } else {
        ProtoEvent::Input(event)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Idle,
    WaitingForAck,
    Sending,
}

fn session_serial_matches(scoped: bool, active: Option<u32>, received: u32) -> bool {
    if scoped {
        active == Some(received)
    } else {
        received == 0
    }
}

fn should_notify_peer(state: State, last_enter_sent_at: Option<Instant>) -> bool {
    state == State::Sending || last_enter_sent_at.is_some()
}

fn close_serial_matches(scoped: bool, active: Option<u32>, received: u32) -> bool {
    if scoped {
        active.is_some_and(|serial| serial | CONTROL_SESSION_CLOSE_BIT == received)
    } else {
        received == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The raw encoding is what reaches the capture backends, so it has to
    /// survive refactoring unchanged.
    #[test]
    fn capture_targets_round_trip_through_their_raw_handle() {
        for target in [
            CaptureTarget::Client(0),
            CaptureTarget::Client(1),
            CaptureTarget::Client(INCOMING_TRIGGER_BEGIN - 1),
            CaptureTarget::IncomingTrigger(0),
            CaptureTarget::IncomingTrigger(1),
            CaptureTarget::IncomingTrigger(u64::MAX - INCOMING_TRIGGER_BEGIN),
        ] {
            assert_eq!(CaptureTarget::from_raw(target.to_raw()), target);
        }
    }

    #[test]
    fn client_and_incoming_handles_do_not_collide() {
        assert_eq!(CaptureTarget::Client(7).to_raw(), 7);
        assert_eq!(
            CaptureTarget::IncomingTrigger(0).to_raw(),
            INCOMING_TRIGGER_BEGIN
        );
        assert_ne!(
            CaptureTarget::Client(0).to_raw(),
            CaptureTarget::IncomingTrigger(0).to_raw()
        );
    }

    #[test]
    fn scoped_sessions_reject_delayed_legacy_or_previous_generation_packets() {
        assert!(session_serial_matches(false, Some(7), 0));
        assert!(!session_serial_matches(false, Some(7), 7));

        assert!(session_serial_matches(true, Some(7), 7));
        assert!(!session_serial_matches(true, Some(7), 0));
        assert!(!session_serial_matches(true, Some(7), 6));

        assert!(close_serial_matches(
            true,
            Some(7),
            CONTROL_SESSION_CLOSE_BIT | 7
        ));
        assert!(!close_serial_matches(true, Some(7), 7));
        assert!(!close_serial_matches(true, Some(7), 0));
    }

    #[test]
    fn failed_first_enter_does_not_create_a_phantom_leave_handshake() {
        assert!(!should_notify_peer(State::WaitingForAck, None));
        assert!(!should_notify_peer(State::Idle, None));
        assert!(should_notify_peer(
            State::WaitingForAck,
            Some(Instant::now())
        ));
        assert!(should_notify_peer(State::Sending, None));
    }
}

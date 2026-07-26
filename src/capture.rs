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
use lan_mouse_proto::{CAPABILITY_ENTER_POSITION, ProtoEvent, WireEvent};
use tokio::task::spawn_local;
use tokio_util::sync::CancellationToken;

use crate::{
    connect::Connection,
    observability::{self, Timestamp},
    position::{capture_to_proto, ipc_to_capture},
    task::{DropGuard, Receiver, Sender, TaskHandle, channel, send},
};

/// minimum time between two "releasing capture" warnings, so a peer that is
/// down does not flood the log with one line per captured input event
const RELEASE_LOG_DEBOUNCE: Duration = Duration::from_millis(500);

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
}

impl CaptureTask {
    fn add_capture(&mut self, handle: CaptureHandle, pos: Position, capture_type: CaptureType) {
        self.captures.push((handle, pos, capture_type));
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
            loop {
                tokio::select! {
                    r = self.request_rx.recv() => match r {
                        None => return,
                        Some(CaptureRequest::Reenable) => break,
                        Some(CaptureRequest::Create(h, p, t)) => self.add_capture(h, p, t),
                        Some(CaptureRequest::Destroy(h)) => self.remove_capture(h),
                        Some(CaptureRequest::Release) => { /* nothing to do */ }
                        Some(CaptureRequest::SetReleaseBind(bind)) => {
                            self.release_bind.borrow_mut().clone_from(&bind);
                        }
                        Some(CaptureRequest::SetClipboard { text, .. }) => {
                            self.clipboard_text = text;
                        }
                    },
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

        let r = self.do_capture_session(&mut capture).await;

        // FIXME replace with async drop when stabilized
        capture.terminate().await?;

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

                    if self.active_client.is_some_and(|active| handle != active) {
                        // Only Ack and Leave from the current input target are relevant.
                        continue;
                    }
                    match event {
                        // connection acknowlegded => set state to Sending
                        ProtoEvent::Ack(_) => {
                            log::info!("client {handle} acknowledged the connection!");
                            self.set_state(State::Sending, "enter_acknowledged");
                            if let Some(started_at) = self.switch_started_at.take() {
                                observability::record_switch_ack(started_at);
                            }
                            self.send_clipboard_to(handle).await;
                        }
                        // client disconnected
                        ProtoEvent::Leave(_) => {
                            log::info!("releasing capture: left remote client device region");
                            self.release_capture(capture, None).await?;
                        },
                        ProtoEvent::LeaveAt { ratio, .. } => {
                            log::info!("releasing capture: left remote client device region at {ratio:.3}");
                            let ratio = ratio.is_finite().then(|| ratio.clamp(0.0, 1.0));
                            self.release_capture(capture, ratio).await?;
                        },
                        _ => {}
                    }
                },
                e = self.request_rx.recv() => match e {
                    None => return Ok(()),
                    Some(CaptureRequest::Reenable) => { /* already active */ },
                    Some(CaptureRequest::Release) => self.release_capture(capture, None).await?,
                    Some(CaptureRequest::Create(h, p, t)) => {
                        self.add_capture(h, p, t);
                        capture.create(h, p).await?;
                    }
                    Some(CaptureRequest::Destroy(h)) => {
                        self.remove_capture(h);
                        capture.destroy(h).await?;
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
            return self.release_capture(capture, None).await;
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
                self.active_client.replace(handle);
                self.switch_started_at = Some(Timestamp::now());
                self.set_state(State::WaitingForAck, "edge_entered");
                send(
                    &self.event_tx,
                    "client entered",
                    ICaptureEvent::ClientEntered(handle),
                );
            }
        }

        let opposite_pos = capture_to_proto(pos.opposite());

        // Prefer the position-carrying Enter when we know the crossing point
        // and the peer supports it. The first Enter may still go out as the
        // legacy event while the peer's Hello is in flight; the WaitingForAck
        // retransmissions upgrade to EnterAt once capabilities are known.
        let enter_event = match self.active_ratio {
            Some(ratio) if self.conn.supports(handle, CAPABILITY_ENTER_POSITION) => {
                ProtoEvent::EnterAt {
                    pos: opposite_pos,
                    ratio,
                }
            }
            _ => ProtoEvent::Enter(opposite_pos),
        };

        let event = match event {
            CaptureEvent::Begin { .. } => enter_event,
            CaptureEvent::Input(e) => match self.state {
                // connection not acknowledged, repeat `Enter` event
                State::WaitingForAck => enter_event,
                State::Sending => ProtoEvent::Input(e),
            },
        };

        match self.conn.send(event, handle).await {
            Ok(()) => {
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
                self.switch_started_at = None;
                capture.release(None).await?;
            }
        }
        Ok(())
    }

    async fn release_capture(
        &mut self,
        capture: &mut InputCapture,
        edge_ratio: Option<f64>,
    ) -> Result<(), CaptureError> {
        self.switch_started_at = None;
        self.active_ratio = None;
        // If we have an active client, notify them we're leaving
        if let Some(handle) = self.active_client.take() {
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
            for key in capture.take_pressed_keys() {
                let key_up = ProtoEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key: key as u32,
                    state: 0,
                }));
                if let Err(e) = self.conn.send(key_up, handle).await {
                    log::warn!("failed to send key-up to client {handle}: {e}");
                }
            }
            // Reset the modifier mask too. The peer's input-emulation
            // layer keeps a separate XKB-style modifier state that's
            // updated by KeyboardEvent::Modifiers, distinct from the
            // pressed_keys set drained above. Without this, an
            // already-locked CapsLock would survive the release.
            let mods_zero = ProtoEvent::Input(Event::Keyboard(KeyboardEvent::Modifiers {
                depressed: 0,
                latched: 0,
                locked: 0,
                group: 0,
            }));
            if let Err(e) = self.conn.send(mods_zero, handle).await {
                log::warn!("failed to reset modifiers on client {handle}: {e}");
            }

            log::info!("sending Leave event to client {handle}");
            if let Err(e) = self.conn.send(ProtoEvent::Leave(0), handle).await {
                log::warn!("failed to send Leave to client {handle}: {e}");
            }
        }
        capture.release(edge_ratio).await
    }

    async fn send_clipboard_to(&self, handle: CaptureHandle) {
        let Some(text) = self.clipboard_text.as_deref() else {
            return;
        };
        if let Err(error) = self.conn.send_clipboard(text, handle).await {
            log::debug!("clipboard text was not sent to client {handle}: {error}");
        }
    }
}

thread_local! {
    static PREV_LOG: Cell<Option<Instant>> = const { Cell::new(None) };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    WaitingForAck,
    Sending,
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
}

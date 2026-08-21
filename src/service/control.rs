//! Mutually exclusive control-session role.
//!
//! Capture and emulation run in separate tasks, but product semantics require
//! one device to be either controlling or controlled at any instant. Keeping
//! that decision here gives the service and frontend a single source of truth.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use lan_mouse_ipc::{ClientHandle, ControlMode, ControlState};

const IDLE: u8 = 0;
const OUTGOING: u8 = 1;
const INCOMING: u8 = 2;
const ALLOW_OUTGOING: u8 = 1 << 0;
const ALLOW_INCOMING: u8 = 1 << 1;

/// The synchronous ownership gate shared by capture and emulation tasks.
///
/// Service events are asynchronous and therefore too late to arbitrate two
/// simultaneous edge crossings. The task that wins this CAS owns the input
/// direction until it releases the matching value.
#[derive(Clone, Debug)]
pub(crate) struct ControlArbiter {
    direction: Arc<AtomicU8>,
    permissions: Arc<AtomicU8>,
}

impl ControlArbiter {
    pub(crate) fn new(mode: ControlMode) -> Self {
        Self {
            direction: Arc::new(AtomicU8::new(IDLE)),
            permissions: Arc::new(AtomicU8::new(permissions(mode))),
        }
    }

    pub(crate) fn set_mode(&self, mode: ControlMode) {
        self.permissions.store(permissions(mode), Ordering::Release);
    }

    pub(crate) fn try_acquire_outgoing(&self) -> bool {
        self.try_acquire(OUTGOING, ALLOW_OUTGOING)
    }

    pub(crate) fn try_acquire_incoming(&self) -> bool {
        self.try_acquire(INCOMING, ALLOW_INCOMING)
    }

    pub(crate) fn release_outgoing(&self) {
        self.release(OUTGOING);
    }

    pub(crate) fn release_incoming(&self) {
        self.release(INCOMING);
    }

    fn try_acquire(&self, direction: u8, permission: u8) -> bool {
        if self.permissions.load(Ordering::Acquire) & permission == 0 {
            return false;
        }
        let acquired = self
            .direction
            .compare_exchange(IDLE, direction, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if acquired && self.permissions.load(Ordering::Acquire) & permission == 0 {
            self.release(direction);
            false
        } else {
            acquired
        }
    }

    fn release(&self, direction: u8) {
        let _ =
            self.direction
                .compare_exchange(direction, IDLE, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl Default for ControlArbiter {
    fn default() -> Self {
        Self::new(ControlMode::Bidirectional)
    }
}

fn permissions(mode: ControlMode) -> u8 {
    match mode {
        ControlMode::Bidirectional => ALLOW_OUTGOING | ALLOW_INCOMING,
        ControlMode::SendOnly => ALLOW_OUTGOING,
        ControlMode::ReceiveOnly => ALLOW_INCOMING,
    }
}

#[derive(Debug)]
pub(crate) struct ControlSession {
    mode: ControlMode,
    state: ControlState,
}

impl ControlSession {
    pub(crate) fn new(mode: ControlMode) -> Self {
        Self {
            mode,
            state: idle_state(mode),
        }
    }

    pub(crate) fn mode(&self) -> ControlMode {
        self.mode
    }

    pub(crate) fn state(&self) -> &ControlState {
        &self.state
    }

    pub(crate) fn set_mode(&mut self, mode: ControlMode) {
        self.mode = mode;
        self.state = idle_state(mode);
    }

    pub(crate) fn begin_controlling(&mut self, handle: ClientHandle) -> bool {
        if !self.may_start_sending() {
            return false;
        }
        self.state = ControlState::Controlling { handle };
        true
    }

    pub(crate) fn begin_controlled(&mut self, addr: SocketAddr, fingerprint: String) -> bool {
        if !self.may_accept_controller() {
            return false;
        }
        self.state = ControlState::ControlledBy { addr, fingerprint };
        true
    }

    pub(crate) fn begin_switching(&mut self) {
        self.state = ControlState::Switching;
    }

    pub(crate) fn reset(&mut self) {
        self.state = idle_state(self.mode);
    }

    pub(crate) fn may_start_sending(&self) -> bool {
        self.mode != ControlMode::ReceiveOnly && self.is_idle()
    }

    pub(crate) fn may_accept_controller(&self) -> bool {
        self.mode != ControlMode::SendOnly && self.is_idle()
    }

    pub(crate) fn wants_outgoing_barriers(&self) -> bool {
        self.mode != ControlMode::ReceiveOnly
            && !matches!(
                self.state,
                ControlState::ControlledBy { .. } | ControlState::Switching
            )
    }

    fn is_idle(&self) -> bool {
        matches!(
            self.state,
            ControlState::Idle | ControlState::ReadyToReceive
        )
    }
}

fn idle_state(mode: ControlMode) -> ControlState {
    if mode == ControlMode::ReceiveOnly {
        ControlState::ReadyToReceive
    } else {
        ControlState::Idle
    }
}

pub(crate) fn pending_mode(current: ControlMode, requested: ControlMode) -> Option<ControlMode> {
    (current != requested).then_some(requested)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn controlling_and_controlled_are_mutually_exclusive() {
        let mut session = ControlSession::new(ControlMode::Bidirectional);

        assert!(session.begin_controlling(7));
        assert!(!session.begin_controlled(addr(4242), "aa:bb".into()));
        assert_eq!(session.state(), &ControlState::Controlling { handle: 7 });

        session.reset();
        assert!(session.begin_controlled(addr(4242), "aa:bb".into()));
        assert!(!session.begin_controlling(7));
    }

    #[test]
    fn receive_only_waits_for_control_and_has_no_outgoing_barriers() {
        let session = ControlSession::new(ControlMode::ReceiveOnly);

        assert_eq!(session.state(), &ControlState::ReadyToReceive);
        assert!(!session.may_start_sending());
        assert!(session.may_accept_controller());
        assert!(!session.wants_outgoing_barriers());
    }

    #[test]
    fn send_only_rejects_incoming_control() {
        let mut session = ControlSession::new(ControlMode::SendOnly);

        assert!(!session.begin_controlled(addr(4242), "aa:bb".into()));
        assert!(session.begin_controlling(1));
    }

    #[test]
    fn arbiter_allows_exactly_one_direction() {
        let arbiter = ControlArbiter::default();

        assert!(arbiter.try_acquire_outgoing());
        assert!(!arbiter.try_acquire_incoming());
        arbiter.release_incoming();
        assert!(!arbiter.try_acquire_incoming());
        arbiter.release_outgoing();
        assert!(arbiter.try_acquire_incoming());
        assert!(!arbiter.try_acquire_outgoing());
        arbiter.release_incoming();
        assert!(arbiter.try_acquire_outgoing());
    }

    #[test]
    fn arbiter_enforces_mode_before_async_tasks_observe_it() {
        let arbiter = ControlArbiter::new(ControlMode::ReceiveOnly);
        assert!(!arbiter.try_acquire_outgoing());
        assert!(arbiter.try_acquire_incoming());
        arbiter.release_incoming();

        arbiter.set_mode(ControlMode::SendOnly);
        assert!(!arbiter.try_acquire_incoming());
        assert!(arbiter.try_acquire_outgoing());
    }

    #[test]
    fn switching_mode_request_can_be_replaced_or_cancelled() {
        let current = ControlMode::Bidirectional;
        let mut pending = pending_mode(current, ControlMode::ReceiveOnly);
        assert_eq!(pending, Some(ControlMode::ReceiveOnly));

        pending = pending_mode(current, ControlMode::SendOnly);
        assert_eq!(pending, Some(ControlMode::SendOnly));

        pending = pending_mode(current, current);
        assert_eq!(pending, None);
    }
}

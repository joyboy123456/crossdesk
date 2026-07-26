//! Bookkeeping for peers that connected to this device.
//!
//! Each such peer gets an enter-only capture barrier at the screen edge it
//! claims, so moving the pointer there hands control back to it. This module
//! owns the mapping between peers and their barriers, and decides what capture
//! work a peer announcing itself implies - the caller performs it.

use std::{collections::HashMap, net::SocketAddr};

use lan_mouse_ipc::Position;

use crate::capture::CaptureTarget;

/// A peer that connected to this device, and the barrier registered for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Incoming {
    pub(crate) target: CaptureTarget,
    pub(crate) fingerprint: String,
    pub(crate) pos: Position,
}

/// The capture work required after a peer announced itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Registration {
    /// the peer is already registered exactly like this
    Unchanged,
    /// register a barrier for a peer we had not seen
    Added(CaptureTarget),
    /// the peer moved or changed identity: replace its barrier
    Replaced {
        destroy: CaptureTarget,
        create: CaptureTarget,
    },
}

/// The incoming connections and their capture barriers.
///
/// A single map, so a peer's address and its barrier cannot disagree - the
/// two tables this replaced had to be kept in step by hand, and a mismatch
/// used to be fatal.
#[derive(Default)]
pub(crate) struct IncomingTracker {
    connections: HashMap<SocketAddr, Incoming>,
    next_trigger: u64,
}

impl IncomingTracker {
    /// Register `addr`, or update what we know about it.
    ///
    /// Returns the capture work the caller has to carry out.
    pub(crate) fn register(
        &mut self,
        addr: SocketAddr,
        pos: Position,
        fingerprint: String,
    ) -> Registration {
        match self.connections.get(&addr) {
            Some(known) if known.pos == pos && known.fingerprint == fingerprint => {
                Registration::Unchanged
            }
            Some(known) => {
                let destroy = known.target;
                let create = self.insert(addr, pos, fingerprint);
                Registration::Replaced { destroy, create }
            }
            None => Registration::Added(self.insert(addr, pos, fingerprint)),
        }
    }

    /// Forget `addr`, returning the barrier that has to be destroyed.
    pub(crate) fn remove(&mut self, addr: SocketAddr) -> Option<CaptureTarget> {
        self.connections
            .remove(&addr)
            .map(|incoming| incoming.target)
    }

    /// The peer that owns `target`, if it is an incoming connection's barrier.
    pub(crate) fn addr_of(&self, target: CaptureTarget) -> Option<SocketAddr> {
        self.connections
            .iter()
            .find(|(_, incoming)| incoming.target == target)
            .map(|(addr, _)| *addr)
    }

    fn insert(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) -> CaptureTarget {
        // Triggers are never reused, so a barrier belonging to a peer that
        // just went away can't be confused with a fresh one.
        let target = CaptureTarget::IncomingTrigger(self.next_trigger);
        self.next_trigger += 1;
        self.connections.insert(
            addr,
            Incoming {
                target,
                fingerprint,
                pos,
            },
        );
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([192, 168, 0, 2], port))
    }

    #[test]
    fn a_new_peer_gets_a_barrier() {
        let mut tracker = IncomingTracker::default();

        let Registration::Added(target) = tracker.register(addr(1), Position::Left, "aa:bb".into())
        else {
            panic!("expected a new registration");
        };
        assert_eq!(tracker.addr_of(target), Some(addr(1)));
    }

    #[test]
    fn re_announcing_the_same_peer_changes_nothing() {
        let mut tracker = IncomingTracker::default();
        tracker.register(addr(1), Position::Left, "aa:bb".into());

        assert_eq!(
            tracker.register(addr(1), Position::Left, "aa:bb".into()),
            Registration::Unchanged,
            "an unchanged peer must not churn its capture barrier"
        );
    }

    #[test]
    fn a_moved_peer_replaces_its_barrier() {
        let mut tracker = IncomingTracker::default();
        let Registration::Added(first) = tracker.register(addr(1), Position::Left, "aa:bb".into())
        else {
            panic!("expected a new registration");
        };

        let Registration::Replaced { destroy, create } =
            tracker.register(addr(1), Position::Right, "aa:bb".into())
        else {
            panic!("expected a replacement");
        };
        assert_eq!(destroy, first);
        assert_ne!(create, first, "the new barrier must be distinguishable");
        assert_eq!(tracker.addr_of(first), None);
        assert_eq!(tracker.addr_of(create), Some(addr(1)));
    }

    #[test]
    fn a_peer_that_changed_identity_replaces_its_barrier() {
        let mut tracker = IncomingTracker::default();
        tracker.register(addr(1), Position::Left, "aa:bb".into());

        assert!(matches!(
            tracker.register(addr(1), Position::Left, "cc:dd".into()),
            Registration::Replaced { .. }
        ));
    }

    #[test]
    fn peers_are_tracked_independently() {
        let mut tracker = IncomingTracker::default();
        let Registration::Added(first) = tracker.register(addr(1), Position::Left, "aa:bb".into())
        else {
            panic!("expected a new registration");
        };
        let Registration::Added(second) =
            tracker.register(addr(2), Position::Right, "cc:dd".into())
        else {
            panic!("expected a new registration");
        };

        assert_ne!(first, second);
        assert_eq!(tracker.remove(addr(1)), Some(first));
        assert_eq!(tracker.addr_of(second), Some(addr(2)));
    }

    #[test]
    fn removing_an_unknown_peer_is_harmless() {
        let mut tracker = IncomingTracker::default();
        assert_eq!(tracker.remove(addr(1)), None);
    }

    #[test]
    fn a_reconnecting_peer_gets_a_fresh_barrier() {
        let mut tracker = IncomingTracker::default();
        let Registration::Added(first) = tracker.register(addr(1), Position::Left, "aa:bb".into())
        else {
            panic!("expected a new registration");
        };
        tracker.remove(addr(1));

        let Registration::Added(second) = tracker.register(addr(1), Position::Left, "aa:bb".into())
        else {
            panic!("expected a new registration after removal");
        };
        assert_ne!(
            first, second,
            "reusing the barrier would let a late event from the old \
             connection address the new one"
        );
    }
}

//! Capability bits advertised by connected peers.

use std::{cell::RefCell, collections::HashMap, net::SocketAddr, rc::Rc};

/// Capability bits received in a peer's `Hello`, keyed by socket address.
///
/// Shared by the outgoing ([`crate::connect`]) and incoming ([`crate::listen`])
/// sides, which each track the peers of their own direction.
#[derive(Clone, Default)]
pub(crate) struct PeerCapabilities {
    capabilities: Rc<RefCell<HashMap<SocketAddr, u32>>>,
}

impl PeerCapabilities {
    pub(crate) fn set(&self, addr: SocketAddr, capabilities: u32) {
        self.capabilities.borrow_mut().insert(addr, capabilities);
    }

    pub(crate) fn remove(&self, addr: SocketAddr) {
        self.capabilities.borrow_mut().remove(&addr);
    }

    /// whether the peer at `addr` advertised all bits of `capability`
    pub(crate) fn supports(&self, addr: SocketAddr, capability: u32) -> bool {
        self.capabilities
            .borrow()
            .get(&addr)
            .is_some_and(|advertised| advertised & capability == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u32 = 1 << 0;
    const B: u32 = 1 << 1;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn unknown_peer_supports_nothing() {
        assert!(!PeerCapabilities::default().supports(addr(1), A));
    }

    #[test]
    fn supports_reports_advertised_bits_only() {
        let capabilities = PeerCapabilities::default();
        capabilities.set(addr(1), A);
        assert!(capabilities.supports(addr(1), A));
        assert!(!capabilities.supports(addr(1), B));
        assert!(!capabilities.supports(addr(1), A | B));
        assert!(!capabilities.supports(addr(2), A));
    }

    #[test]
    fn remove_forgets_the_peer() {
        let capabilities = PeerCapabilities::default();
        capabilities.set(addr(1), A);
        capabilities.remove(addr(1));
        assert!(!capabilities.supports(addr(1), A));
    }
}

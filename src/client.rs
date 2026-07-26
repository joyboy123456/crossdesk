use std::{
    cell::RefCell,
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    rc::Rc,
};

use slab::Slab;

use lan_mouse_ipc::{ClientConfig, ClientHandle, ClientState, Position};

use crate::config::ConfigClient;

#[derive(Clone, Default)]
pub struct ClientManager {
    clients: Rc<RefCell<Slab<(ClientConfig, ClientState)>>>,
}

impl ClientManager {
    /// get all clients
    pub fn clients(&self) -> Vec<(ClientConfig, ClientState)> {
        self.clients
            .borrow()
            .iter()
            .map(|(_, c)| c.clone())
            .collect::<Vec<_>>()
    }

    pub fn add_with_config(&self, config_client: ConfigClient) -> ClientHandle {
        let config = ClientConfig {
            hostname: config_client.hostname,
            fix_ips: config_client.ips.into_iter().collect(),
            port: config_client.port,
            pos: config_client.pos,
            cmd: config_client.enter_hook,
        };
        self.add_configured_client(config, config_client.active)
    }

    /// add a new client to this manager
    pub fn add_client(&self) -> ClientHandle {
        self.clients.borrow_mut().insert(Default::default()) as ClientHandle
    }

    /// Add a client with its complete configuration and initial active state.
    pub fn add_configured_client(&self, config: ClientConfig, active: bool) -> ClientHandle {
        let state = ClientState {
            active,
            ips: HashSet::from_iter(config.fix_ips.iter().copied()),
            ..Default::default()
        };
        self.clients.borrow_mut().insert((config, state)) as ClientHandle
    }

    /// activate the given client
    /// returns, whether the client was activated
    pub fn activate_client(&self, handle: ClientHandle) -> bool {
        let mut clients = self.clients.borrow_mut();
        match clients.get_mut(handle as usize) {
            Some((_, s)) if !s.active => {
                s.active = true;
                true
            }
            _ => false,
        }
    }

    /// deactivate the given client
    /// returns, whether the client was deactivated
    pub fn deactivate_client(&self, handle: ClientHandle) -> bool {
        let mut clients = self.clients.borrow_mut();
        match clients.get_mut(handle as usize) {
            Some((_, s)) if s.active => {
                s.active = false;
                true
            }
            _ => false,
        }
    }

    /// find a client by its address
    pub fn get_client(&self, addr: SocketAddr) -> Option<ClientHandle> {
        // since there shouldn't be more than a handful of clients at any given
        // time this is likely faster than using a HashMap
        self.clients
            .borrow()
            .iter()
            .find_map(|(k, (_, s))| {
                if s.active && s.ips.contains(&addr.ip()) {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    /// get the client at the given position
    pub fn client_at(&self, pos: Position) -> Option<ClientHandle> {
        self.clients
            .borrow()
            .iter()
            .find_map(|(k, (c, s))| {
                if s.active && c.pos == pos {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    pub(crate) fn get_hostname(&self, handle: ClientHandle) -> Option<String> {
        self.clients
            .borrow_mut()
            .get_mut(handle as usize)
            .and_then(|(c, _)| c.hostname.clone())
    }

    /// get the position of the corresponding client
    pub(crate) fn get_pos(&self, handle: ClientHandle) -> Option<Position> {
        self.clients
            .borrow()
            .get(handle as usize)
            .map(|(c, _)| c.pos)
    }

    /// remove a client from the list
    pub fn remove_client(&self, client: ClientHandle) -> Option<(ClientConfig, ClientState)> {
        // remove id from occupied ids
        self.clients.borrow_mut().try_remove(client as usize)
    }

    /// get the config & state of the given client
    pub fn get_state(&self, handle: ClientHandle) -> Option<(ClientConfig, ClientState)> {
        self.clients.borrow().get(handle as usize).cloned()
    }

    /// get the current config & state of all clients
    pub fn get_client_states(&self) -> Vec<(ClientHandle, ClientConfig, ClientState)> {
        self.clients
            .borrow()
            .iter()
            .map(|(k, v)| (k as ClientHandle, v.0.clone(), v.1.clone()))
            .collect()
    }

    /// update the fix ips of the client
    pub fn set_fix_ips(&self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        if let Some((c, _)) = self.clients.borrow_mut().get_mut(handle as usize) {
            c.fix_ips = fix_ips
        }
        self.update_ips(handle);
    }

    /// update the dns-ips of the client
    pub fn set_dns_ips(&self, handle: ClientHandle, dns_ips: Vec<IpAddr>) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.dns_ips = dns_ips
        }
        self.update_ips(handle);
    }

    fn update_ips(&self, handle: ClientHandle) {
        if let Some((c, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.ips = c
                .fix_ips
                .iter()
                .cloned()
                .chain(s.dns_ips.iter().cloned())
                .collect::<HashSet<_>>();
        }
    }

    /// update the hostname of the given client
    /// this automatically clears the active ip address and ips from dns
    pub fn set_hostname(&self, handle: ClientHandle, hostname: Option<String>) -> bool {
        let mut clients = self.clients.borrow_mut();
        let Some((c, s)) = clients.get_mut(handle as usize) else {
            return false;
        };

        // hostname changed
        if c.hostname != hostname {
            c.hostname = hostname;
            s.active_addr = None;
            s.dns_ips.clear();
            drop(clients);
            self.update_ips(handle);
            true
        } else {
            false
        }
    }

    /// update the port of the client
    pub(crate) fn set_port(&self, handle: ClientHandle, port: u16) {
        match self.clients.borrow_mut().get_mut(handle as usize) {
            Some((c, s)) if c.port != port => {
                c.port = port;
                s.active_addr = s.active_addr.map(|a| SocketAddr::new(a.ip(), port));
            }
            _ => {}
        };
    }

    /// update the position of the client
    /// returns true, if a change in capture position is required (pos changed & client is active)
    pub(crate) fn set_pos(&self, handle: ClientHandle, pos: Position) -> bool {
        match self.clients.borrow_mut().get_mut(handle as usize) {
            Some((c, s)) if c.pos != pos => {
                log::info!("update pos {handle} {} -> {}", c.pos, pos);
                c.pos = pos;
                s.active
            }
            _ => false,
        }
    }

    /// update the enter hook command of the client
    pub(crate) fn set_enter_hook(&self, handle: ClientHandle, enter_hook: Option<String>) {
        if let Some((c, _s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            c.cmd = enter_hook;
        }
    }

    /// set resolving status of the client
    pub(crate) fn set_resolving(&self, handle: ClientHandle, status: bool) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.resolving = status;
        }
    }

    /// get the enter hook command
    pub(crate) fn get_enter_cmd(&self, handle: ClientHandle) -> Option<String> {
        self.clients
            .borrow()
            .get(handle as usize)
            .and_then(|(c, _)| c.cmd.clone())
    }

    /// returns all clients that are currently registered
    pub(crate) fn registered_clients(&self) -> Vec<ClientHandle> {
        self.clients
            .borrow()
            .iter()
            .map(|(h, _)| h as ClientHandle)
            .collect()
    }

    /// returns all clients that are currently active
    pub(crate) fn active_clients(&self) -> Vec<ClientHandle> {
        self.clients
            .borrow()
            .iter()
            .filter(|(_, (_, s))| s.active)
            .map(|(h, _)| h as ClientHandle)
            .collect()
    }

    pub(crate) fn set_active_addr(&self, handle: ClientHandle, addr: Option<SocketAddr>) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.active_addr = addr;
        }
    }

    pub(crate) fn set_alive(&self, handle: ClientHandle, alive: bool) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.alive = alive;
        }
    }

    pub(crate) fn set_peer_commit(&self, handle: ClientHandle, commit: Option<[u8; 8]>) {
        if let Some((_, s)) = self.clients.borrow_mut().get_mut(handle as usize) {
            s.peer_commit = commit;
        }
    }

    pub(crate) fn active_addr(&self, handle: ClientHandle) -> Option<SocketAddr> {
        self.clients
            .borrow()
            .get(handle as usize)
            .and_then(|(_, s)| s.active_addr)
    }

    pub(crate) fn alive(&self, handle: ClientHandle) -> bool {
        self.clients
            .borrow()
            .get(handle as usize)
            .map(|(_, s)| s.alive)
            .unwrap_or(false)
    }

    pub(crate) fn get_port(&self, handle: ClientHandle) -> Option<u16> {
        self.clients
            .borrow()
            .get(handle as usize)
            .map(|(c, _)| c.port)
    }

    pub(crate) fn get_ips(&self, handle: ClientHandle) -> Option<HashSet<IpAddr>> {
        self.clients
            .borrow()
            .get(handle as usize)
            .map(|(_, s)| s.ips.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    fn configured(pos: Position, active: bool) -> (ClientManager, ClientHandle) {
        let manager = ClientManager::default();
        let handle = manager.add_configured_client(
            ClientConfig {
                hostname: Some("peer".into()),
                fix_ips: vec![ip(1)],
                port: 4242,
                pos,
                cmd: None,
            },
            active,
        );
        (manager, handle)
    }

    #[test]
    fn added_client_is_readable_and_removable() {
        let (manager, handle) = configured(Position::Right, true);

        let (config, state) = manager.get_state(handle).expect("client exists");
        assert_eq!(config.pos, Position::Right);
        assert_eq!(config.port, 4242);
        assert!(state.active);
        // add_configured_client seeds the reachable ips from the fixed ones
        assert!(state.ips.contains(&ip(1)));
        assert_eq!(manager.registered_clients(), vec![handle]);
        assert_eq!(manager.active_clients(), vec![handle]);

        assert!(manager.remove_client(handle).is_some());
        assert!(manager.get_state(handle).is_none());
        assert!(manager.registered_clients().is_empty());
        // removing twice must not panic - the service retries on IPC replay
        assert!(manager.remove_client(handle).is_none());
    }

    /// `Service::run` deactivates every client before activating them, because
    /// `activate_client` reports "changed" rather than "is active". Keep that
    /// contract: it decides whether a capture is (re)created.
    #[test]
    fn activation_reports_change_not_state() {
        let (manager, handle) = configured(Position::Left, false);

        assert!(manager.activate_client(handle), "first activation changes");
        assert!(
            !manager.activate_client(handle),
            "activating an active client is a no-op"
        );
        assert!(
            manager.deactivate_client(handle),
            "first deactivation changes"
        );
        assert!(
            !manager.deactivate_client(handle),
            "deactivating an inactive client is a no-op"
        );
    }

    #[test]
    fn unknown_handles_are_reported_not_panicked_on() {
        let manager = ClientManager::default();
        let unknown: ClientHandle = 99;

        assert!(!manager.activate_client(unknown));
        assert!(!manager.deactivate_client(unknown));
        assert!(!manager.set_pos(unknown, Position::Top));
        assert!(!manager.set_hostname(unknown, Some("x".into())));
        assert!(manager.get_state(unknown).is_none());
        assert!(manager.get_pos(unknown).is_none());
        assert!(manager.get_ips(unknown).is_none());
        assert!(manager.active_addr(unknown).is_none());
        assert!(!manager.alive(unknown));
        manager.set_port(unknown, 1);
        manager.set_alive(unknown, true);
        manager.set_active_addr(unknown, None);
    }

    #[test]
    fn lookup_by_position_and_address_only_finds_active_clients() {
        let (manager, handle) = configured(Position::Top, true);
        let addr = SocketAddr::new(ip(1), 4242);

        assert_eq!(manager.client_at(Position::Top), Some(handle));
        assert_eq!(manager.get_client(addr), Some(handle));
        assert_eq!(manager.client_at(Position::Bottom), None);

        manager.deactivate_client(handle);
        assert_eq!(manager.client_at(Position::Top), None);
        assert_eq!(manager.get_client(addr), None);
    }

    /// A position change only needs a capture update when the client is
    /// active, which is exactly what `set_pos` returns.
    #[test]
    fn set_pos_requests_capture_update_only_when_active() {
        let (manager, handle) = configured(Position::Left, true);

        assert!(manager.set_pos(handle, Position::Right));
        assert!(!manager.set_pos(handle, Position::Right), "same position");
        assert_eq!(manager.get_pos(handle), Some(Position::Right));

        manager.deactivate_client(handle);
        assert!(
            !manager.set_pos(handle, Position::Top),
            "inactive client needs no capture update"
        );
        assert_eq!(manager.get_pos(handle), Some(Position::Top));
    }

    #[test]
    fn hostname_change_invalidates_resolved_addresses() {
        let (manager, handle) = configured(Position::Left, true);
        manager.set_dns_ips(handle, vec![ip(2)]);
        manager.set_active_addr(handle, Some(SocketAddr::new(ip(2), 4242)));

        assert!(manager.set_hostname(handle, Some("other".into())));

        let (_, state) = manager.get_state(handle).expect("client exists");
        assert!(state.active_addr.is_none(), "stale address must be dropped");
        assert!(
            state.dns_ips.is_empty(),
            "stale dns results must be dropped"
        );
        assert_eq!(state.ips, HashSet::from([ip(1)]), "fixed ips remain");

        assert!(
            !manager.set_hostname(handle, Some("other".into())),
            "setting the same hostname is a no-op"
        );
    }

    #[test]
    fn reachable_ips_are_the_union_of_fixed_and_resolved() {
        let (manager, handle) = configured(Position::Left, true);
        manager.set_dns_ips(handle, vec![ip(2), ip(3)]);

        assert_eq!(
            manager.get_ips(handle).expect("client exists"),
            HashSet::from([ip(1), ip(2), ip(3)])
        );

        manager.set_fix_ips(handle, vec![ip(4)]);
        assert_eq!(
            manager.get_ips(handle).expect("client exists"),
            HashSet::from([ip(2), ip(3), ip(4)]),
            "replacing fixed ips keeps resolved ones"
        );
    }

    #[test]
    fn port_change_rewrites_the_active_address() {
        let (manager, handle) = configured(Position::Left, true);
        manager.set_active_addr(handle, Some(SocketAddr::new(ip(1), 4242)));

        manager.set_port(handle, 5000);

        assert_eq!(manager.get_port(handle), Some(5000));
        assert_eq!(
            manager.active_addr(handle),
            Some(SocketAddr::new(ip(1), 5000)),
            "the open connection must follow the new port"
        );
    }

    #[test]
    fn handles_are_not_reused_while_a_client_is_alive() {
        let manager = ClientManager::default();
        let first = manager.add_client();
        let second = manager.add_client();

        assert_ne!(first, second);
        manager.remove_client(first);
        assert!(manager.get_state(first).is_none());
        assert!(manager.get_state(second).is_some());
    }
}

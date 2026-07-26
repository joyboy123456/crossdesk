mod clipboard_state;
mod hooks;
mod incoming;
mod keys;

use crate::{
    capture::{Capture, CaptureTarget, CaptureType, ICaptureEvent},
    client::ClientManager,
    clipboard::{ClipboardEvent, ClipboardSync},
    config::{Config, ConfigClient},
    connect::Connection,
    crypto,
    dns::{DnsEvent, DnsResolver},
    emulation::{Emulation, EmulationEvent},
    listen::{DtlsListener, ListenerCreationError},
    task::{Receiver, Sender, channel, send},
};
use clipboard_state::{ClipboardAction, ClipboardState};
use futures::StreamExt;
use incoming::{IncomingTracker, Registration};
use keys::AuthorizedKeys;
use lan_mouse_ipc::{
    AsyncFrontendListener, ClientHandle, FrontendEvent, FrontendRequest, IpcError,
    IpcListenerCreationError, Position, Status,
};
use std::{
    collections::HashSet,
    io,
    net::{IpAddr, SocketAddr},
};
use thiserror::Error;
use tokio::signal;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    IpcListen(#[from] IpcListenerCreationError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ListenError(#[from] ListenerCreationError),
    #[error("failed to load certificate: `{0}`")]
    Certificate(#[from] crypto::Error),
}

/// The daemon: owns every subsystem and routes events between them.
///
/// The fields fall into three groups - the handles that drive the subsystems,
/// the state describing what is configured and connected, and the frontend
/// event queue. Each piece of state that has rules of its own lives in a
/// submodule ([`incoming`], [`clipboard_state`], [`keys`]) so those rules can
/// be tested without a running service; what stays here is the event loop and
/// the routing.
pub struct Service {
    /// configuration
    config: Config,
    /// input capture
    capture: Capture,
    /// input emulation
    emulation: Emulation,
    /// dns resolver
    resolver: DnsResolver,
    /// clipboard synchronization worker
    clipboard: ClipboardSync,
    /// frontend listener
    frontend_listener: AsyncFrontendListener,
    /// frontend events queued for sending
    frontend_events_tx: Sender<FrontendEvent>,
    frontend_events_rx: Receiver<FrontendEvent>,

    /// (outgoing) client information
    client_manager: ClientManager,
    /// peers that connected to this device
    incoming: IncomingTracker,
    /// fingerprints allowed to connect
    authorized_keys: AuthorizedKeys,
    /// clipboard synchronization state and loop suppression
    clipboard_state: ClipboardState,

    /// current port
    port: u16,
    /// the public key fingerprint for (D)TLS
    public_key_fingerprint: String,
    /// status of input capture (enabled / disabled)
    capture_status: Status,
    /// status of input emulation (enabled / disabled)
    emulation_status: Status,
    shutdown_requested: bool,
}

impl Service {
    pub async fn new(config: Config) -> Result<Self, ServiceError> {
        let client_manager = ClientManager::default();
        for client in config.clients() {
            client_manager.add_with_config(client);
        }

        // load certificate
        let cert = crypto::load_or_generate_key_and_cert(config.cert_path())?;
        let public_key_fingerprint = crypto::certificate_fingerprint(&cert);

        // create frontend communication adapter, exit if already running
        let frontend_listener = AsyncFrontendListener::new().await?;

        let authorized_keys = AuthorizedKeys::new(config.authorized_fingerprints());
        // listener + connection
        let listener =
            DtlsListener::new(config.port(), cert.clone(), authorized_keys.shared()).await?;
        let conn = Connection::new(cert.clone(), client_manager.clone());

        // input capture + emulation
        let capture_backend = config.capture_backend().map(|b| b.into());
        let capture = Capture::new(capture_backend, conn, config.release_bind());
        let emulation_backend = config.emulation_backend().map(|b| b.into());
        let emulation = Emulation::new(emulation_backend, listener);

        // create dns resolver
        let resolver = DnsResolver::new()?;

        let port = config.port();
        let clipboard_enabled = config.clipboard_sync();
        let clipboard = ClipboardSync::new(clipboard_enabled);
        let (frontend_events_tx, frontend_events_rx) = channel();
        let service = Self {
            config,
            capture,
            emulation,
            frontend_listener,
            resolver,
            authorized_keys,
            public_key_fingerprint,
            client_manager,
            frontend_events_tx,
            frontend_events_rx,
            port,
            capture_status: Default::default(),
            emulation_status: Default::default(),
            incoming: Default::default(),
            shutdown_requested: false,
            clipboard,
            clipboard_state: ClipboardState::new(clipboard_enabled),
        };
        Ok(service)
    }

    pub async fn run(&mut self) -> Result<(), ServiceError> {
        let active = self.client_manager.active_clients();
        for handle in active.iter() {
            // small hack: `activate_client()` checks, if the client
            // is already active in client_manager and does not create a
            // capture barrier in that case so we have to deactivate it first
            self.client_manager.deactivate_client(*handle);
        }

        for handle in active {
            self.activate_client(handle);
        }

        while !self.shutdown_requested {
            tokio::select! {
                request = self.frontend_listener.next() => self.handle_frontend_request(request),
                Some(event) = self.frontend_events_rx.recv() => {
                    self.frontend_listener.broadcast(event).await;
                }
                event = self.emulation.event() => match event {
                    Some(event) => self.handle_emulation_event(event),
                    None => self.subsystem_stopped("input emulation"),
                },
                event = self.capture.event() => match event {
                    Some(event) => self.handle_capture_event(event),
                    None => self.subsystem_stopped("input capture"),
                },
                event = self.resolver.event() => match event {
                    Some(event) => self.handle_resolver_event(event),
                    None => self.subsystem_stopped("dns resolver"),
                },
                event = self.clipboard.event() => match event {
                    Some(event) => self.handle_clipboard_event(event),
                    None => self.subsystem_stopped("clipboard sync"),
                },
                changed = self.config.changed() => match changed {
                    Ok(()) => self.handle_config_change(),
                    Err(e) => log::warn!("config file watcher: {e}"),
                },
                r = signal::ctrl_c() => {
                    if let Err(e) = r {
                        log::warn!("could not listen for CTRL+C: {e}");
                    }
                    self.shutdown_requested = true;
                },
            }
        }

        log::info!("terminating service ...");
        log::debug!("terminating capture ...");
        self.capture.terminate().await;
        log::debug!("terminating emulation ...");
        self.emulation.terminate().await;
        log::debug!("terminating dns resolver ...");
        self.resolver.terminate().await;
        log::debug!("terminating clipboard sync ...");
        self.clipboard.terminate().await;

        Ok(())
    }

    /// A subsystem's event channel ended, which only happens once its task is
    /// gone. Nothing will drive that half of the service anymore, so stop
    /// rather than spin on a channel that is closed for good.
    fn subsystem_stopped(&mut self, what: &str) {
        log::error!("{what} stopped unexpectedly, shutting down");
        self.shutdown_requested = true;
    }

    fn handle_frontend_request(&mut self, request: Option<Result<FrontendRequest, IpcError>>) {
        let request = match request {
            Some(Ok(r)) => r,
            Some(Err(e)) => return log::error!("error receiving request: {e}"),
            None => return self.subsystem_stopped("frontend ipc listener"),
        };
        match request {
            FrontendRequest::Activate(handle, active) => {
                self.set_client_active(handle, active);
                self.save_config();
            }
            FrontendRequest::AuthorizeKey(desc, fp) => {
                self.add_authorized_key(desc, fp);
                self.save_config();
            }
            FrontendRequest::ChangePort(port) => self.change_port(port),
            FrontendRequest::Create => {
                self.add_client();
                self.save_config();
            }
            FrontendRequest::CreateConfigured { config, active } => {
                self.add_configured_client(config, active);
                self.save_config();
            }
            FrontendRequest::Delete(handle) => {
                self.remove_client(handle);
                self.save_config();
            }
            FrontendRequest::EnableCapture => self.capture.reenable(),
            FrontendRequest::EnableEmulation => self.emulation.reenable(),
            FrontendRequest::SetClipboardSync(enabled) => {
                self.set_clipboard_enabled(enabled);
                self.save_config();
            }
            FrontendRequest::Enumerate() => self.enumerate(),
            FrontendRequest::UpdateFixIps(handle, fix_ips) => {
                self.update_fix_ips(handle, fix_ips);
                self.save_config();
            }
            FrontendRequest::UpdateHostname(handle, host) => {
                self.update_hostname(handle, host);
                self.save_config();
            }
            FrontendRequest::UpdatePort(handle, port) => {
                self.update_port(handle, port);
                self.save_config();
            }
            FrontendRequest::UpdatePosition(handle, pos) => {
                self.update_pos(handle, pos);
                self.save_config();
            }
            FrontendRequest::ResolveDns(handle) => self.resolve(handle),
            FrontendRequest::Sync => self.sync_frontend(),
            FrontendRequest::RemoveAuthorizedKey(key) => {
                self.remove_authorized_key(key);
                self.save_config();
            }
            FrontendRequest::UpdateEnterHook(handle, enter_hook) => {
                self.update_enter_hook(handle, enter_hook)
            }
            FrontendRequest::SaveConfiguration => self.save_config(),
            FrontendRequest::ShutdownService => self.shutdown_requested = true,
        }
    }

    fn save_config(&mut self) {
        let clients = self.client_manager.clients();
        let clients = clients
            .into_iter()
            .map(|(c, s)| ConfigClient {
                ips: HashSet::from_iter(c.fix_ips),
                hostname: c.hostname,
                port: c.port,
                pos: c.pos,
                active: s.active,
                enter_hook: c.cmd,
            })
            .collect();
        self.config.set_clients(clients);
        self.config
            .set_authorized_keys(self.authorized_keys.snapshot());
        if let Err(e) = self.config.write_back() {
            log::warn!("failed to write config: {e}");
        }
    }

    fn handle_config_change(&mut self) {
        for h in self.client_manager.registered_clients() {
            self.remove_client(h);
        }
        for c in self.config.clients() {
            let handle = self.client_manager.add_with_config(c);
            log::info!("added client {handle}");
            let Some((c, s)) = self.client_manager.get_state(handle) else {
                continue;
            };
            if s.active {
                self.client_manager.deactivate_client(handle);
                self.activate_client(handle);
            }
            self.notify_frontend(FrontendEvent::Created(handle, c, s));
        }
        let release_bind = self.config.release_bind();
        self.capture.set_release_bind(release_bind);
        self.authorized_keys
            .replace(&self.config.authorized_fingerprints());
        self.set_clipboard_enabled(self.config.clipboard_sync());
        self.sync_frontend();
    }

    fn handle_emulation_event(&mut self, event: EmulationEvent) {
        match event {
            EmulationEvent::ConnectionAttempt { fingerprint } => {
                self.notify_frontend(FrontendEvent::ConnectionAttempt { fingerprint });
            }
            EmulationEvent::Entered {
                addr,
                pos,
                fingerprint,
            } => self.register_incoming(addr, pos, fingerprint),
            EmulationEvent::Disconnected { addr } => {
                if let Some(target) = self.incoming.remove(addr) {
                    self.capture.destroy(target);
                    self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
                }
            }
            EmulationEvent::PortChanged(port) => match port {
                Ok(port) => {
                    self.port = port;
                    self.notify_frontend(FrontendEvent::PortChanged(port, None));
                }
                Err(e) => self
                    .notify_frontend(FrontendEvent::PortChanged(self.port, Some(format!("{e}")))),
            },
            EmulationEvent::EmulationDisabled => {
                self.emulation_status = Status::Disabled;
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
            }
            EmulationEvent::EmulationEnabled => {
                self.emulation_status = Status::Enabled;
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
            }
            EmulationEvent::ReleaseNotify => self.capture.release(),
            EmulationEvent::Connected { addr, fingerprint } => {
                self.notify_frontend(FrontendEvent::DeviceConnected { addr, fingerprint });
            }
            EmulationEvent::PeerHello { addr, commit } => {
                // Map the peer's source addr back to its client handle
                // and stamp the commit. Skip if we don't have an
                // outgoing client configured for this peer (incoming-
                // only setup) — there's nowhere to display the version
                // in that case anyway.
                if let Some(handle) = self.client_manager.get_client(addr) {
                    self.client_manager.set_peer_commit(handle, Some(commit));
                    self.broadcast_client(handle);
                }
            }
            EmulationEvent::ClipboardText(text) => self.apply_remote_clipboard(text),
        }
    }

    /// A peer announced which screen edge it sits at. Register it, and carry
    /// out whatever capture work that implies.
    fn register_incoming(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) {
        match self.incoming.register(addr, pos, fingerprint.clone()) {
            Registration::Unchanged => {}
            Registration::Added(target) => {
                self.capture.create(target, pos, CaptureType::EnterOnly);
                self.notify_frontend(FrontendEvent::DeviceEntered {
                    fingerprint,
                    addr,
                    pos,
                });
            }
            Registration::Replaced { destroy, create } => {
                self.capture.destroy(destroy);
                self.capture.create(create, pos, CaptureType::EnterOnly);
                self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
                self.notify_frontend(FrontendEvent::DeviceEntered {
                    fingerprint,
                    addr,
                    pos,
                });
            }
        }
    }

    fn handle_capture_event(&mut self, event: ICaptureEvent) {
        match event {
            ICaptureEvent::CaptureBegin { target, ratio } => {
                // we entered the capture zone for an incoming connection
                // => notify it that its capture should be released, telling
                // it where along the barrier the cursor crossed
                if let Some(addr) = self.incoming.addr_of(target) {
                    self.emulation.send_leave_event(addr, ratio);
                }
            }
            ICaptureEvent::CaptureDisabled => {
                self.capture_status = Status::Disabled;
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
            }
            ICaptureEvent::CaptureEnabled => {
                self.capture_status = Status::Enabled;
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
            }
            ICaptureEvent::ClientEntered(handle) => {
                log::info!("entering client {handle} ...");
                if let Some(cmd) = self.client_manager.get_enter_cmd(handle) {
                    hooks::spawn(cmd);
                }
            }
            ICaptureEvent::ClipboardText(text) => self.apply_remote_clipboard(text),
        }
    }

    fn handle_resolver_event(&mut self, event: DnsEvent) {
        let handle = match event {
            DnsEvent::Resolving(handle) => {
                self.client_manager.set_resolving(handle, true);
                handle
            }
            DnsEvent::Resolved(handle, hostname, ips) => {
                self.client_manager.set_resolving(handle, false);
                if let Err(e) = &ips {
                    log::warn!("could not resolve {hostname}: {e}");
                }
                let ips = ips.unwrap_or_default();
                self.client_manager.set_dns_ips(handle, ips);
                handle
            }
        };
        self.broadcast_client(handle);
    }

    fn resolve(&self, handle: ClientHandle) {
        if let Some(hostname) = self.client_manager.get_hostname(handle) {
            self.resolver.resolve(handle, hostname);
        }
    }

    fn sync_frontend(&mut self) {
        self.enumerate();
        self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
        self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
        self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
        self.notify_frontend(FrontendEvent::PublicKeyFingerprint(
            self.public_key_fingerprint.clone(),
        ));
        self.notify_clipboard_state();
        let keys = self.authorized_keys.snapshot();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn notify_frontend(&mut self, event: FrontendEvent) {
        send(&self.frontend_events_tx, "frontend event", event);
    }

    fn add_authorized_key(&mut self, desc: String, fp: String) {
        self.authorized_keys.authorize(fp, desc);
        let keys = self.authorized_keys.snapshot();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn remove_authorized_key(&mut self, fp: String) {
        self.authorized_keys.revoke(&fp);
        let keys = self.authorized_keys.snapshot();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn enumerate(&mut self) {
        let clients = self.client_manager.get_client_states();
        self.notify_frontend(FrontendEvent::Enumerate(clients));
    }

    fn add_client(&mut self) {
        let handle = self.client_manager.add_client();
        log::info!("added client {handle}");
        let Some((c, s)) = self.client_manager.get_state(handle) else {
            return;
        };
        self.notify_frontend(FrontendEvent::Created(handle, c, s));
    }

    fn add_configured_client(&mut self, config: lan_mouse_ipc::ClientConfig, active: bool) {
        let handle = self.client_manager.add_configured_client(config, false);
        log::info!("added configured client {handle}");
        let (config, state) = self.client_manager.get_state(handle).expect("new client");
        self.notify_frontend(FrontendEvent::Created(handle, config, state));
        if active {
            self.activate_client(handle);
        }
    }

    fn set_client_active(&mut self, handle: ClientHandle, active: bool) {
        if active {
            self.activate_client(handle);
        } else {
            self.deactivate_client(handle);
        }
    }

    fn deactivate_client(&mut self, handle: ClientHandle) {
        log::debug!("deactivating client {handle}");
        if self.client_manager.deactivate_client(handle) {
            self.capture.destroy(CaptureTarget::Client(handle));
            self.broadcast_client(handle);
            log::info!("deactivated client {handle}");
        }
    }

    fn activate_client(&mut self, handle: ClientHandle) {
        log::debug!("activating client {handle}");

        /* resolve dns on activate */
        self.resolve(handle);

        /* deactivate potential other client at this position */
        let Some(pos) = self.client_manager.get_pos(handle) else {
            return;
        };

        if let Some(other) = self.client_manager.client_at(pos) {
            if other != handle {
                self.deactivate_client(other);
            }
        }

        /* activate the client */
        if self.client_manager.activate_client(handle) {
            /* notify capture and frontends */
            self.capture
                .create(CaptureTarget::Client(handle), pos, CaptureType::Default);
            self.broadcast_client(handle);
            log::info!("activated client {handle} ({pos})");
        }
    }

    fn change_port(&mut self, port: u16) {
        if self.port != port {
            self.emulation.request_port_change(port);
        } else {
            self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
        }
    }

    fn remove_client(&mut self, handle: ClientHandle) {
        if self
            .client_manager
            .remove_client(handle)
            .map(|(_, s)| s.active)
            .unwrap_or(false)
        {
            self.capture.destroy(CaptureTarget::Client(handle));
        }
        self.notify_frontend(FrontendEvent::Deleted(handle));
    }

    fn update_fix_ips(&mut self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        self.client_manager.set_fix_ips(handle, fix_ips);
        self.broadcast_client(handle);
    }

    fn update_hostname(&mut self, handle: ClientHandle, hostname: Option<String>) {
        log::info!("hostname changed: {hostname:?}");
        if self.client_manager.set_hostname(handle, hostname.clone()) {
            self.resolve(handle);
        }
        self.broadcast_client(handle);
    }

    fn update_port(&mut self, handle: ClientHandle, port: u16) {
        self.client_manager.set_port(handle, port);
        self.broadcast_client(handle);
    }

    fn update_pos(&mut self, handle: ClientHandle, pos: Position) {
        // update state in event input emulator & input capture
        if self.client_manager.set_pos(handle, pos) {
            self.deactivate_client(handle);
            self.activate_client(handle);
        }
        self.broadcast_client(handle);
    }

    fn update_enter_hook(&mut self, handle: ClientHandle, enter_hook: Option<String>) {
        self.client_manager.set_enter_hook(handle, enter_hook);
        self.broadcast_client(handle);
    }

    fn broadcast_client(&mut self, handle: ClientHandle) {
        let event = self
            .client_manager
            .get_state(handle)
            .map(|(c, s)| FrontendEvent::State(handle, c, s))
            .unwrap_or(FrontendEvent::NoSuchClient(handle));
        self.notify_frontend(event);
    }

    fn handle_clipboard_event(&mut self, event: ClipboardEvent) {
        match event {
            ClipboardEvent::Availability(available) => {
                self.clipboard_state.set_available(available);
                self.notify_clipboard_state();
            }
            ClipboardEvent::LocalText(text) => {
                let action = self.clipboard_state.on_local_text(text);
                self.apply_clipboard_action(action);
            }
        }
    }

    fn apply_remote_clipboard(&mut self, text: String) {
        if !self.clipboard_state.accepts_remote_text(&text) {
            return;
        }
        log::debug!("applying remote clipboard text ({} bytes)", text.len());
        if self.clipboard.apply(text.clone()) {
            let action = self.clipboard_state.on_remote_text_applied(text);
            self.apply_clipboard_action(action);
        }
    }

    fn set_clipboard_enabled(&mut self, enabled: bool) {
        self.config.set_clipboard_sync(enabled);
        self.clipboard.set_enabled(enabled);
        let action = self.clipboard_state.set_enabled(enabled);
        self.apply_clipboard_action(action);
        self.notify_clipboard_state();
    }

    /// Push the decision [`ClipboardState`] made out to the two send paths.
    fn apply_clipboard_action(&mut self, action: ClipboardAction) {
        let ClipboardAction::Cache { text, broadcast } = action else {
            return;
        };
        self.capture.set_clipboard(text.clone(), broadcast);
        self.emulation.set_clipboard(text, broadcast);
    }

    fn notify_clipboard_state(&mut self) {
        self.notify_frontend(FrontendEvent::ClipboardState {
            enabled: self.clipboard_state.enabled(),
            available: self.clipboard_state.available(),
        });
    }
}

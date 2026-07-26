use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
};

use lan_mouse_ipc::{
    ClientConfig, ClientHandle, ClientState, DEFAULT_PORT, FrontendEvent, Position, Status,
};

#[derive(Clone, Debug)]
pub struct UiClient {
    pub handle: ClientHandle,
    pub config: ClientConfig,
    pub state: ClientState,
}

#[derive(Default)]
pub struct UiState {
    pub clients: BTreeMap<ClientHandle, UiClient>,
    pub authorized: HashMap<String, String>,
    pub pending_authorizations: Vec<String>,
    pub port: u16,
    pub fingerprint: String,
    pub capture_status: Status,
    pub emulation_status: Status,
    pub clipboard_enabled: bool,
    pub clipboard_available: bool,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            port: DEFAULT_PORT,
            clipboard_enabled: true,
            ..Default::default()
        }
    }

    pub fn apply(&mut self, event: FrontendEvent) {
        match event {
            FrontendEvent::Created(handle, config, state)
            | FrontendEvent::State(handle, config, state) => {
                self.clients.insert(
                    handle,
                    UiClient {
                        handle,
                        config,
                        state,
                    },
                );
            }
            FrontendEvent::Deleted(handle) | FrontendEvent::NoSuchClient(handle) => {
                self.clients.remove(&handle);
            }
            FrontendEvent::Enumerate(clients) => {
                self.clients = clients
                    .into_iter()
                    .map(|(handle, config, state)| {
                        (
                            handle,
                            UiClient {
                                handle,
                                config,
                                state,
                            },
                        )
                    })
                    .collect();
            }
            FrontendEvent::PortChanged(port, _) => self.port = port,
            FrontendEvent::CaptureStatus(status) => self.capture_status = status,
            FrontendEvent::EmulationStatus(status) => self.emulation_status = status,
            FrontendEvent::AuthorizedUpdated(authorized) => self.authorized = authorized,
            FrontendEvent::PublicKeyFingerprint(fingerprint) => self.fingerprint = fingerprint,
            FrontendEvent::ClipboardState { enabled, available } => {
                self.clipboard_enabled = enabled;
                self.clipboard_available = available;
            }
            FrontendEvent::ConnectionAttempt { fingerprint } => {
                if !self.pending_authorizations.contains(&fingerprint) {
                    self.pending_authorizations.push(fingerprint);
                }
            }
            FrontendEvent::Error(_)
            | FrontendEvent::DeviceConnected { .. }
            | FrontendEvent::DeviceEntered { .. }
            | FrontendEvent::IncomingDisconnected(_) => {}
        }
    }

    pub fn occupied(&self, pos: Position, except: Option<ClientHandle>) -> bool {
        self.clients.values().any(|client| {
            client.state.active && client.config.pos == pos && Some(client.handle) != except
        })
    }

    pub fn next_screen_number(&self, handle: ClientHandle) -> usize {
        self.clients
            .keys()
            .position(|candidate| *candidate == handle)
            .map_or(2, |index| index + 2)
    }
}

#[derive(Clone, Debug)]
pub struct DeviceDraft {
    pub hostname: String,
    pub ips: String,
    pub port: String,
    pub pos: Position,
    pub active: bool,
}

impl Default for DeviceDraft {
    fn default() -> Self {
        Self {
            hostname: String::new(),
            ips: String::new(),
            port: DEFAULT_PORT.to_string(),
            pos: Position::Right,
            active: true,
        }
    }
}

impl DeviceDraft {
    pub fn from_client(client: &UiClient) -> Self {
        Self {
            hostname: client.config.hostname.clone().unwrap_or_default(),
            ips: client
                .config
                .fix_ips
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            port: client.config.port.to_string(),
            pos: client.config.pos,
            active: client.state.active,
        }
    }

    pub fn validate(&self) -> Result<ClientConfig, String> {
        let hostname = self.hostname.trim();
        if !hostname.is_empty() && !valid_hostname(hostname) {
            return Err("主机名格式无效".into());
        }
        let hostname = (!hostname.is_empty()).then(|| hostname.to_owned());
        let fix_ips = parse_ips(&self.ips)?;
        if hostname.is_none() && fix_ips.is_empty() {
            return Err("请填写主机名或至少一个 IP 地址".into());
        }
        let port = self
            .port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| "端口必须是 1 到 65535 之间的数字".to_owned())?;

        Ok(ClientConfig {
            hostname,
            fix_ips,
            port,
            pos: self.pos,
            cmd: None,
        })
    }
}

fn valid_hostname(value: &str) -> bool {
    let value = value.strip_suffix('.').unwrap_or(value);
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn parse_ips(value: &str) -> Result<Vec<IpAddr>, String> {
    value
        .split([',', ';', '\n', ' '])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<IpAddr>()
                .map_err(|_| format!("IP 地址格式无效：{part}"))
        })
        .collect()
}

pub fn displayed_position(configured: Position, pending: Option<Position>) -> Position {
    pending.unwrap_or(configured)
}

pub fn position_label(pos: Position) -> &'static str {
    match pos {
        Position::Left => "左侧",
        Position::Right => "右侧",
        Position::Top => "上方",
        Position::Bottom => "下方",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_accepts_hostname_or_ip_and_rejects_bad_values() {
        let hostname = DeviceDraft {
            hostname: "mac-mini.local".into(),
            ..Default::default()
        };
        assert!(hostname.validate().is_ok());

        let ip = DeviceDraft {
            ips: "192.168.1.20, ::1".into(),
            ..Default::default()
        };
        assert_eq!(ip.validate().expect("valid IPs").fix_ips.len(), 2);

        assert!(DeviceDraft::default().validate().is_err());
        assert!(
            DeviceDraft {
                hostname: "mac".into(),
                port: "0".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            DeviceDraft {
                hostname: "bad host".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            DeviceDraft {
                hostname: "-mac.local".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn draft_accepts_port_boundaries_and_builds_complete_config() {
        for port in ["1", "65535"] {
            let draft = DeviceDraft {
                hostname: "mac-mini.local".into(),
                ips: "192.168.1.20 ::1".into(),
                port: port.into(),
                pos: Position::Top,
                active: false,
            };
            let config = draft.validate().expect("boundary port should be valid");
            assert_eq!(config.hostname.as_deref(), Some("mac-mini.local"));
            assert_eq!(config.fix_ips.len(), 2);
            assert_eq!(config.port.to_string(), port);
            assert_eq!(config.pos, Position::Top);
        }

        assert!(
            DeviceDraft {
                hostname: "mac".into(),
                port: "65536".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn state_reducer_replaces_enumeration_and_tracks_authorization() {
        let mut state = UiState::new();
        state.apply(FrontendEvent::Enumerate(vec![(
            7,
            ClientConfig {
                hostname: Some("mac".into()),
                pos: Position::Right,
                ..Default::default()
            },
            ClientState {
                active: true,
                ..Default::default()
            },
        )]));
        state.apply(FrontendEvent::ConnectionAttempt {
            fingerprint: "aa:bb".into(),
        });

        assert!(state.occupied(Position::Right, None));
        assert!(!state.occupied(Position::Right, Some(7)));
        assert_eq!(state.next_screen_number(7), 2);
        assert_eq!(state.pending_authorizations, ["aa:bb"]);
    }

    #[test]
    fn state_reducer_handles_create_update_offline_and_delete() {
        let mut state = UiState::new();
        let config = ClientConfig {
            hostname: Some("mac".into()),
            pos: Position::Left,
            ..Default::default()
        };
        state.apply(FrontendEvent::Created(
            4,
            config.clone(),
            ClientState {
                active: true,
                alive: false,
                ..Default::default()
            },
        ));
        assert!(state.occupied(Position::Left, None));
        assert!(!state.clients[&4].state.alive);

        let mut updated = config;
        updated.pos = Position::Bottom;
        state.apply(FrontendEvent::State(
            4,
            updated,
            ClientState {
                active: true,
                alive: true,
                ..Default::default()
            },
        ));
        assert!(!state.occupied(Position::Left, None));
        assert!(state.occupied(Position::Bottom, None));
        assert!(state.clients[&4].state.alive);

        state.apply(FrontendEvent::Deleted(4));
        assert!(state.clients.is_empty());
    }

    #[test]
    fn screen_numbers_are_stable_and_pending_layout_rolls_back() {
        let mut state = UiState::new();
        for (handle, name) in [(2, "mac"), (8, "workstation")] {
            state.apply(FrontendEvent::Created(
                handle,
                ClientConfig {
                    hostname: Some(name.into()),
                    ..Default::default()
                },
                ClientState::default(),
            ));
        }
        assert_eq!(state.next_screen_number(2), 2);
        assert_eq!(state.next_screen_number(8), 3);

        assert_eq!(
            displayed_position(Position::Right, Some(Position::Left)),
            Position::Left
        );
        assert_eq!(displayed_position(Position::Right, None), Position::Right);
    }
}

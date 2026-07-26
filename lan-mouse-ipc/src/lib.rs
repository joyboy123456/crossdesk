use std::{
    collections::{HashMap, HashSet},
    env::VarError,
    fmt::Display,
    io,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};
use thiserror::Error;

#[cfg(unix)]
use std::{
    env,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

mod connect;
mod connect_async;
mod listen;

pub use connect::{FrontendEventReader, FrontendRequestWriter, connect};
pub use connect_async::{AsyncFrontendEventReader, AsyncFrontendRequestWriter, connect_async};
pub use listen::AsyncFrontendListener;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error(transparent)]
    SocketPath(#[from] SocketPathError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("connection timed out")]
    Timeout,
}

#[derive(Debug, Error)]
pub enum IpcListenerCreationError {
    #[error("could not determine socket-path: `{0}`")]
    SocketPath(#[from] SocketPathError),
    #[error("service already running!")]
    AlreadyRunning,
    #[error("failed to bind lan-mouse socket: `{0}`")]
    Bind(io::Error),
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io error occured: `{0}`")]
    Io(#[from] io::Error),
    #[error("invalid json: `{0}`")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Listen(#[from] IpcListenerCreationError),
}

pub const DEFAULT_PORT: u16 = 4242;

#[derive(Debug, Default, Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

impl Position {
    pub fn opposite(&self) -> Self {
        match self {
            Position::Left => Position::Right,
            Position::Right => Position::Left,
            Position::Top => Position::Bottom,
            Position::Bottom => Position::Top,
        }
    }
}

#[derive(Debug, Error)]
#[error("not a valid position: {pos}")]
pub struct PositionParseError {
    pos: String,
}

impl FromStr for Position {
    type Err = PositionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            _ => Err(PositionParseError { pos: s.into() }),
        }
    }
}

impl Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Position::Left => "left",
                Position::Right => "right",
                Position::Top => "top",
                Position::Bottom => "bottom",
            }
        )
    }
}

impl TryFrom<&str> for Position {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "left" => Ok(Position::Left),
            "right" => Ok(Position::Right),
            "top" => Ok(Position::Top),
            "bottom" => Ok(Position::Bottom),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// hostname of this client
    pub hostname: Option<String>,
    /// fix ips, determined by the user
    pub fix_ips: Vec<IpAddr>,
    /// both active_addr and addrs can be None / empty so port needs to be stored seperately
    pub port: u16,
    /// position of a client on screen
    pub pos: Position,
    /// enter hook
    pub cmd: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            hostname: Default::default(),
            fix_ips: Default::default(),
            pos: Default::default(),
            cmd: None,
        }
    }
}

pub type ClientHandle = u64;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClientState {
    /// events should be sent to and received from the client
    pub active: bool,
    /// `active` address of the client, used to send data to.
    /// This should generally be the socket address where data
    /// was last received from.
    pub active_addr: Option<SocketAddr>,
    /// tracks whether or not the client is available for emulation
    pub alive: bool,
    /// ips from dns
    pub dns_ips: Vec<IpAddr>,
    /// all ip addresses associated with a particular client
    /// e.g. Laptops usually have at least an ethernet and a wifi port
    /// which have different ip addresses
    pub ips: HashSet<IpAddr>,
    /// client has pressed keys
    pub has_pressed_keys: bool,
    /// dns resolving in progress
    pub resolving: bool,
    /// Peer's build short commit hash from the [`Hello`] proto
    /// event. `None` means we haven't received a Hello yet — either
    /// the connection is fresh, or the peer is on an older build
    /// that predates the Hello event. The frontend uses this to
    /// soft-warn on version mismatch.
    pub peer_commit: Option<[u8; 8]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendEvent {
    /// a client was created
    Created(ClientHandle, ClientConfig, ClientState),
    /// no such client
    NoSuchClient(ClientHandle),
    /// state changed
    State(ClientHandle, ClientConfig, ClientState),
    /// the client was deleted
    Deleted(ClientHandle),
    /// new port, reason of failure (if failed)
    PortChanged(u16, Option<String>),
    /// list of all clients, used for initial state synchronization
    Enumerate(Vec<(ClientHandle, ClientConfig, ClientState)>),
    /// an error occured
    Error(String),
    /// capture status
    CaptureStatus(Status),
    /// emulation status
    EmulationStatus(Status),
    /// authorized public key fingerprints have been updated
    AuthorizedUpdated(HashMap<String, String>),
    /// public key fingerprint of this device
    PublicKeyFingerprint(String),
    /// text clipboard synchronization state and platform availability
    ClipboardState { enabled: bool, available: bool },
    /// new device connected
    DeviceConnected {
        addr: SocketAddr,
        fingerprint: String,
    },
    /// incoming device entered the screen
    DeviceEntered {
        fingerprint: String,
        addr: SocketAddr,
        pos: Position,
    },
    /// incoming disconnected
    IncomingDisconnected(SocketAddr),
    /// failed connection attempt (approval for fingerprint required)
    ConnectionAttempt { fingerprint: String },
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub enum FrontendRequest {
    /// activate/deactivate client
    Activate(ClientHandle, bool),
    /// add a new client
    Create,
    /// add a fully configured client
    CreateConfigured { config: ClientConfig, active: bool },
    /// change the listen port (recreate udp listener)
    ChangePort(u16),
    /// remove a client
    Delete(ClientHandle),
    /// request an enumeration of all clients
    Enumerate(),
    /// resolve dns
    ResolveDns(ClientHandle),
    /// update hostname
    UpdateHostname(ClientHandle, Option<String>),
    /// update port
    UpdatePort(ClientHandle, u16),
    /// update position
    UpdatePosition(ClientHandle, Position),
    /// update fix-ips
    UpdateFixIps(ClientHandle, Vec<IpAddr>),
    /// request reenabling input capture
    EnableCapture,
    /// request reenabling input emulation
    EnableEmulation,
    /// enable or disable UTF-8 text clipboard synchronization
    SetClipboardSync(bool),
    /// synchronize all state
    Sync,
    /// authorize fingerprint (description, fingerprint)
    AuthorizeKey(String, String),
    /// remove fingerprint (fingerprint)
    RemoveAuthorizedKey(String),
    /// change the hook command
    UpdateEnterHook(u64, Option<String>),
    /// save config file
    SaveConfiguration,
    /// gracefully stop the service
    ShutdownService,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Disabled,
    Enabled,
}

#[cfg(test)]
mod tests {
    use super::{
        ClientConfig, ClientState, DEFAULT_PORT, FrontendEvent, FrontendRequest, Position, Status,
    };

    /// Frontends and the service are built and shipped separately (the CLI
    /// may be an older build than the daemon it talks to), so the JSON
    /// representation of the IPC types is a compatibility contract. These
    /// expectations pin the variant names and field names; change them only
    /// alongside a deliberate IPC version bump.
    #[test]
    fn request_json_representation_is_frozen() {
        for (request, expected) in [
            (
                FrontendRequest::Activate(3, true),
                r#"{"Activate":[3,true]}"#,
            ),
            (FrontendRequest::Create, r#""Create""#),
            (FrontendRequest::ChangePort(4242), r#"{"ChangePort":4242}"#),
            (FrontendRequest::Delete(1), r#"{"Delete":1}"#),
            (FrontendRequest::Enumerate(), r#"{"Enumerate":[]}"#),
            (FrontendRequest::ResolveDns(1), r#"{"ResolveDns":1}"#),
            (
                FrontendRequest::UpdateHostname(1, Some("host".into())),
                r#"{"UpdateHostname":[1,"host"]}"#,
            ),
            (
                FrontendRequest::UpdatePosition(1, Position::Top),
                r#"{"UpdatePosition":[1,"top"]}"#,
            ),
            (FrontendRequest::EnableCapture, r#""EnableCapture""#),
            (FrontendRequest::EnableEmulation, r#""EnableEmulation""#),
            (
                FrontendRequest::SetClipboardSync(true),
                r#"{"SetClipboardSync":true}"#,
            ),
            (FrontendRequest::Sync, r#""Sync""#),
            (
                FrontendRequest::AuthorizeKey("desc".into(), "aa:bb".into()),
                r#"{"AuthorizeKey":["desc","aa:bb"]}"#,
            ),
            (FrontendRequest::SaveConfiguration, r#""SaveConfiguration""#),
            (FrontendRequest::ShutdownService, r#""ShutdownService""#),
        ] {
            let json = serde_json::to_string(&request).expect("serialize request");
            assert_eq!(json, expected, "encoding of {request:?} changed");
            let decoded: FrontendRequest =
                serde_json::from_str(&json).expect("deserialize request");
            assert_eq!(decoded, request);
        }
    }

    #[test]
    fn event_json_representation_is_frozen() {
        for (event, expected) in [
            (FrontendEvent::Deleted(2), r#"{"Deleted":2}"#),
            (FrontendEvent::NoSuchClient(2), r#"{"NoSuchClient":2}"#),
            (
                FrontendEvent::PortChanged(4242, None),
                r#"{"PortChanged":[4242,null]}"#,
            ),
            (FrontendEvent::Error("boom".into()), r#"{"Error":"boom"}"#),
            (
                FrontendEvent::CaptureStatus(Status::Enabled),
                r#"{"CaptureStatus":"Enabled"}"#,
            ),
            (
                FrontendEvent::EmulationStatus(Status::Disabled),
                r#"{"EmulationStatus":"Disabled"}"#,
            ),
            (
                FrontendEvent::PublicKeyFingerprint("aa:bb".into()),
                r#"{"PublicKeyFingerprint":"aa:bb"}"#,
            ),
            (
                FrontendEvent::ClipboardState {
                    enabled: true,
                    available: false,
                },
                r#"{"ClipboardState":{"enabled":true,"available":false}}"#,
            ),
            (
                FrontendEvent::ConnectionAttempt {
                    fingerprint: "aa:bb".into(),
                },
                r#"{"ConnectionAttempt":{"fingerprint":"aa:bb"}}"#,
            ),
        ] {
            let json = serde_json::to_string(&event).expect("serialize event");
            assert_eq!(json, expected, "encoding of {event:?} changed");
            serde_json::from_str::<FrontendEvent>(&json).expect("deserialize event");
        }
    }

    #[test]
    fn client_config_and_state_json_is_frozen() {
        let config = ClientConfig {
            hostname: Some("host".into()),
            fix_ips: vec!["10.0.0.1".parse().expect("valid test address")],
            port: 4242,
            pos: Position::Left,
            cmd: None,
        };
        assert_eq!(
            serde_json::to_string(&config).expect("serialize config"),
            r#"{"hostname":"host","fix_ips":["10.0.0.1"],"port":4242,"pos":"left","cmd":null}"#
        );

        // a default state must round-trip through an older peer's decoder,
        // so every field carries a serde-representable default
        let state = ClientState::default();
        let json = serde_json::to_string(&state).expect("serialize state");
        assert_eq!(
            json,
            r#"{"active":false,"active_addr":null,"alive":false,"dns_ips":[],"ips":[],"has_pressed_keys":false,"resolving":false,"peer_commit":null}"#
        );
        serde_json::from_str::<ClientState>(&json).expect("deserialize state");
    }

    #[test]
    fn defaults_are_frozen() {
        assert_eq!(DEFAULT_PORT, 4242);
        assert_eq!(ClientConfig::default().port, DEFAULT_PORT);
        assert_eq!(ClientConfig::default().pos, Position::Left);
        assert_eq!(Status::default(), Status::Disabled);
    }

    #[test]
    fn position_parses_from_its_own_display_form() {
        for pos in [
            Position::Left,
            Position::Right,
            Position::Top,
            Position::Bottom,
        ] {
            let rendered = pos.to_string();
            assert_eq!(
                Position::try_from(rendered.as_str()),
                Ok(pos),
                "{rendered} did not parse back"
            );
        }
    }

    #[test]
    fn configured_client_request_round_trips_as_json() {
        let request = FrontendRequest::CreateConfigured {
            config: ClientConfig {
                hostname: Some("mac-mini.local".into()),
                fix_ips: vec!["192.168.1.42".parse().expect("valid test address")],
                port: 4242,
                pos: Position::Right,
                cmd: None,
            },
            active: true,
        };

        let json = serde_json::to_string(&request).expect("serialize request");
        let decoded = serde_json::from_str(&json).expect("deserialize request");

        assert_eq!(request, decoded);
    }

    #[test]
    fn shutdown_request_round_trips_as_json() {
        let request = FrontendRequest::ShutdownService;
        let json = serde_json::to_string(&request).expect("serialize request");
        let decoded = serde_json::from_str(&json).expect("deserialize request");

        assert_eq!(request, decoded);
    }

    #[test]
    fn clipboard_setting_request_round_trips_as_json() {
        let request = FrontendRequest::SetClipboardSync(false);
        let json = serde_json::to_string(&request).expect("serialize request");
        let decoded = serde_json::from_str(&json).expect("deserialize request");

        assert_eq!(request, decoded);
    }
}

impl From<Status> for bool {
    fn from(status: Status) -> Self {
        match status {
            Status::Enabled => true,
            Status::Disabled => false,
        }
    }
}

#[cfg(unix)]
const LAN_MOUSE_SOCKET_NAME: &str = "lan-mouse-socket.sock";

#[derive(Debug, Error)]
pub enum SocketPathError {
    #[error("could not determine $XDG_RUNTIME_DIR: `{0}`")]
    XdgRuntimeDirNotFound(VarError),
    #[error("could not determine $HOME: `{0}`")]
    HomeDirNotFound(VarError),
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn default_socket_path() -> Result<PathBuf, SocketPathError> {
    let xdg_runtime_dir =
        env::var("XDG_RUNTIME_DIR").map_err(SocketPathError::XdgRuntimeDirNotFound)?;
    Ok(Path::new(xdg_runtime_dir.as_str()).join(LAN_MOUSE_SOCKET_NAME))
}

#[cfg(all(unix, target_os = "macos"))]
pub fn default_socket_path() -> Result<PathBuf, SocketPathError> {
    let home = env::var("HOME").map_err(SocketPathError::HomeDirNotFound)?;
    Ok(Path::new(home.as_str())
        .join("Library")
        .join("Caches")
        .join(LAN_MOUSE_SOCKET_NAME))
}

use crate::capture_test::TestCaptureArgs;
use crate::emulation_test::TestEmulationArgs;
use clap::{Parser, Subcommand, ValueEnum};
use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env::{self, VarError};
use std::fmt::Display;
use std::fs::{self, File};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::{collections::HashSet, io};
use thiserror::Error;
use toml_edit::{self, DocumentMut};

use lan_mouse_cli::CliArgs;
use lan_mouse_ipc::{DEFAULT_PORT, Position};

use input_event::scancode::{
    self,
    Linux::{KeyLeftAlt, KeyLeftCtrl, KeyLeftMeta, KeyLeftShift},
};

use shadow_rs::shadow;

shadow!(build);

/// Local build's 8-byte ASCII short commit hash, suitable for use
/// in [`lan_mouse_proto::ProtoEvent::Hello`]. Pads with `'?'` if
/// shadow_rs returns an unexpected length so the field is always
/// well-formed on the wire.
pub fn local_commit() -> [u8; 8] {
    let bytes = build::SHORT_COMMIT.as_bytes();
    let mut out = [b'?'; 8];
    let n = bytes.len().min(8);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

const CONFIG_FILE_NAME: &str = "config.toml";
const CERT_FILE_NAME: &str = "lan-mouse.pem";

fn default_path() -> Result<PathBuf, VarError> {
    #[cfg(unix)]
    let default_path = {
        let xdg_config_home =
            env::var("XDG_CONFIG_HOME").unwrap_or(format!("{}/.config", env::var("HOME")?));
        format!("{xdg_config_home}/lan-mouse/")
    };

    #[cfg(not(unix))]
    let default_path = {
        let app_data =
            env::var("LOCALAPPDATA").unwrap_or(format!("{}/.config", env::var("USERPROFILE")?));
        format!("{app_data}\\lan-mouse\\")
    };
    Ok(PathBuf::from(default_path))
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
struct ConfigToml {
    capture_backend: Option<CaptureBackend>,
    emulation_backend: Option<EmulationBackend>,
    port: Option<u16>,
    release_bind: Option<Vec<scancode::Linux>>,
    clipboard_sync: Option<bool>,
    cert_path: Option<PathBuf>,
    clients: Option<Vec<TomlClient>>,
    authorized_fingerprints: Option<HashMap<String, String>>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Eq, PartialEq)]
struct TomlClient {
    hostname: Option<String>,
    host_name: Option<String>,
    ips: Option<Vec<IpAddr>>,
    port: Option<u16>,
    position: Option<Position>,
    activate_on_startup: Option<bool>,
    enter_hook: Option<String>,
}

impl ConfigToml {
    fn new(path: &Path) -> Result<ConfigToml, ConfigError> {
        let config = fs::read_to_string(path)?;
        Ok(toml::from_str::<_>(&config)?)
    }
}

#[derive(Parser, Debug)]
#[command(author, version=build::CLAP_LONG_VERSION, about, long_about = None)]
struct Args {
    /// the listen port for lan-mouse
    #[arg(short, long)]
    port: Option<u16>,

    /// non-default config file location
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// capture backend override
    #[arg(long)]
    capture_backend: Option<CaptureBackend>,

    /// emulation backend override
    #[arg(long)]
    emulation_backend: Option<EmulationBackend>,

    /// path to non-default certificate location
    #[arg(long)]
    cert_path: Option<PathBuf>,

    /// subcommands
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// test input emulation
    TestEmulation(TestEmulationArgs),
    /// test input capture
    TestCapture(TestCaptureArgs),
    /// Lan Mouse commandline interface
    Cli(CliArgs),
    /// run in daemon mode
    Daemon,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
pub enum CaptureBackend {
    #[cfg(libei_capture)]
    #[serde(rename = "input-capture-portal")]
    InputCapturePortal,
    #[cfg(layer_shell_capture)]
    #[serde(rename = "layer-shell")]
    LayerShell,
    #[cfg(x11_capture)]
    #[serde(rename = "x11")]
    X11,
    #[cfg(windows)]
    #[serde(rename = "windows")]
    Windows,
    #[cfg(target_os = "macos")]
    #[serde(rename = "macos")]
    MacOs,
    #[serde(rename = "dummy")]
    Dummy,
}

impl Display for CaptureBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(libei_capture)]
            CaptureBackend::InputCapturePortal => write!(f, "input-capture-portal"),
            #[cfg(layer_shell_capture)]
            CaptureBackend::LayerShell => write!(f, "layer-shell"),
            #[cfg(x11_capture)]
            CaptureBackend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            CaptureBackend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            CaptureBackend::MacOs => write!(f, "MacOS"),
            CaptureBackend::Dummy => write!(f, "dummy"),
        }
    }
}

impl From<CaptureBackend> for input_capture::Backend {
    fn from(backend: CaptureBackend) -> Self {
        match backend {
            #[cfg(libei_capture)]
            CaptureBackend::InputCapturePortal => Self::InputCapturePortal,
            #[cfg(layer_shell_capture)]
            CaptureBackend::LayerShell => Self::LayerShell,
            #[cfg(x11_capture)]
            CaptureBackend::X11 => Self::X11,
            #[cfg(windows)]
            CaptureBackend::Windows => Self::Windows,
            #[cfg(target_os = "macos")]
            CaptureBackend::MacOs => Self::MacOs,
            CaptureBackend::Dummy => Self::Dummy,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
pub enum EmulationBackend {
    #[cfg(wlroots_emulation)]
    #[serde(rename = "wlroots")]
    Wlroots,
    #[cfg(libei_emulation)]
    #[serde(rename = "libei")]
    Libei,
    #[cfg(rdp_emulation)]
    #[serde(rename = "xdp")]
    Xdp,
    #[cfg(x11_emulation)]
    #[serde(rename = "x11")]
    X11,
    #[cfg(windows)]
    #[serde(rename = "windows")]
    Windows,
    #[cfg(target_os = "macos")]
    #[serde(rename = "macos")]
    MacOs,
    #[serde(rename = "dummy")]
    Dummy,
}

impl From<EmulationBackend> for input_emulation::Backend {
    fn from(backend: EmulationBackend) -> Self {
        match backend {
            #[cfg(wlroots_emulation)]
            EmulationBackend::Wlroots => Self::Wlroots,
            #[cfg(libei_emulation)]
            EmulationBackend::Libei => Self::Libei,
            #[cfg(rdp_emulation)]
            EmulationBackend::Xdp => Self::Xdp,
            #[cfg(x11_emulation)]
            EmulationBackend::X11 => Self::X11,
            #[cfg(windows)]
            EmulationBackend::Windows => Self::Windows,
            #[cfg(target_os = "macos")]
            EmulationBackend::MacOs => Self::MacOs,
            EmulationBackend::Dummy => Self::Dummy,
        }
    }
}

impl Display for EmulationBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(wlroots_emulation)]
            EmulationBackend::Wlroots => write!(f, "wlroots"),
            #[cfg(libei_emulation)]
            EmulationBackend::Libei => write!(f, "libei"),
            #[cfg(rdp_emulation)]
            EmulationBackend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(x11_emulation)]
            EmulationBackend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            EmulationBackend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            EmulationBackend::MacOs => write!(f, "macos"),
            EmulationBackend::Dummy => write!(f, "dummy"),
        }
    }
}

#[derive(Debug)]
pub struct Config {
    /// command line arguments
    args: Args,
    /// path to the certificate file used
    cert_path: PathBuf,
    /// path to the config file used
    config_path: PathBuf,
    /// path to config directory (parent of above)
    config_dir: PathBuf,
    /// the (optional) toml config and it's path
    config_toml: Option<ConfigToml>,
    // filesystem watcher
    watcher: notify::RecommendedWatcher,
    // channel for filesystem events
    watch_rx: tokio::sync::mpsc::Receiver<Result<notify::Event, notify::Error>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigClient {
    pub ips: HashSet<IpAddr>,
    pub hostname: Option<String>,
    pub port: u16,
    pub pos: Position,
    pub active: bool,
    pub enter_hook: Option<String>,
}

impl From<TomlClient> for ConfigClient {
    fn from(toml: TomlClient) -> Self {
        let active = toml.activate_on_startup.unwrap_or(false);
        let enter_hook = toml.enter_hook;
        let hostname = toml.hostname;
        let ips = HashSet::from_iter(toml.ips.into_iter().flatten());
        let port = toml.port.unwrap_or(DEFAULT_PORT);
        let pos = toml.position.unwrap_or_default();
        Self {
            ips,
            hostname,
            port,
            pos,
            active,
            enter_hook,
        }
    }
}

impl From<ConfigClient> for TomlClient {
    fn from(client: ConfigClient) -> Self {
        let hostname = client.hostname;
        let host_name = None;
        let mut ips = client.ips.into_iter().collect::<Vec<_>>();
        ips.sort();
        let ips = Some(ips);
        let port = if client.port == DEFAULT_PORT {
            None
        } else {
            Some(client.port)
        };
        let position = Some(client.pos);
        let activate_on_startup = if client.active { Some(true) } else { None };
        let enter_hook = client.enter_hook;
        Self {
            hostname,
            host_name,
            ips,
            port,
            position,
            activate_on_startup,
            enter_hook,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Var(#[from] VarError),
    #[error(transparent)]
    Watcher(#[from] notify::Error),
}

const DEFAULT_RELEASE_KEYS: [scancode::Linux; 4] =
    [KeyLeftCtrl, KeyLeftShift, KeyLeftMeta, KeyLeftAlt];

impl Config {
    pub fn new() -> Result<Self, ConfigError> {
        let args = Args::parse();

        // --config <file> overrules default location
        let config_path = args
            .config
            .clone()
            .unwrap_or(default_path()?.join(CONFIG_FILE_NAME));
        let config_dir = config_path
            .parent()
            .expect("config directory")
            .to_path_buf();

        // Ensure the config directory exists and write a default config file
        // if none is present. Runs on every Config::new(), regardless of which
        // entry path (GUI main, spawned daemon, CLI, test commands) we're on,
        // so a fresh Mac never hits "No such file or directory" on config.toml
        // and notify::Watcher (which requires the dir to exist on macOS
        // FSEvents and some Linux backends) has a concrete path to watch.
        fs::create_dir_all(&config_dir)?;
        if !config_path.exists() {
            let default_toml = toml::to_string_pretty(&ConfigToml::default())
                .expect("default ConfigToml serialization cannot fail");
            fs::write(&config_path, default_toml)?;
        }

        let config_toml = match ConfigToml::new(&config_path) {
            Err(e) => {
                log::warn!("{config_path:?}: {e}");
                log::warn!("Continuing without config file ...");
                None
            }
            Ok(c) => Some(c),
        };

        // --cert-path <file> overrules default location
        let cert_path = args
            .cert_path
            .clone()
            .or(config_toml.as_ref().and_then(|c| c.cert_path.clone()))
            .unwrap_or(default_path()?.join(CERT_FILE_NAME));

        let (tx, watch_rx) = tokio::sync::mpsc::channel(16);
        let watcher = RecommendedWatcher::new(
            move |res| {
                let _ = tx.blocking_send(res);
            },
            notify::Config::default(),
        )?;
        let mut config = Config {
            args,
            cert_path,
            config_path,
            config_dir,
            config_toml,
            watcher,
            watch_rx,
        };
        config.watch()?;
        Ok(config)
    }

    fn watch(&mut self) -> Result<(), notify::Error> {
        self.watcher
            .watch(&self.config_dir, notify::RecursiveMode::NonRecursive)?;
        Ok(())
    }

    fn unwatch(&mut self) -> Result<(), notify::Error> {
        self.watcher.unwatch(&self.config_dir)?;
        Ok(())
    }

    /// Wait until the config file changed on disk and was re-read.
    ///
    /// Never resolves once the watcher is gone, so a dead watcher degrades to
    /// "no hot reload" instead of taking the service down.
    pub async fn changed(&mut self) -> Result<(), notify::Error> {
        loop {
            let Some(event) = self.watch_rx.recv().await else {
                log::warn!("config file watcher stopped; hot reload is disabled");
                std::future::pending::<()>().await;
                unreachable!("pending never resolves");
            };
            let event = event?;
            if event.paths.contains(&self.config_path)
                && matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(ModifyKind::Data(_))
                        | EventKind::Remove(_)
                )
                && self.read_from_disk()?
            {
                return Ok(());
            }
        }
    }

    /// the command to run
    pub fn command(&self) -> Option<Command> {
        self.args.command.clone()
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// public key fingerprints authorized for connection
    pub fn authorized_fingerprints(&self) -> HashMap<String, String> {
        self.config_toml
            .as_ref()
            .and_then(|c| c.authorized_fingerprints.clone())
            .unwrap_or_default()
    }

    /// path to certificate
    pub fn cert_path(&self) -> &Path {
        &self.cert_path
    }

    /// optional input-capture backend override
    pub fn capture_backend(&self) -> Option<CaptureBackend> {
        self.args
            .capture_backend
            .or(self.config_toml.as_ref().and_then(|c| c.capture_backend))
    }

    /// optional input-emulation backend override
    pub fn emulation_backend(&self) -> Option<EmulationBackend> {
        self.args
            .emulation_backend
            .or(self.config_toml.as_ref().and_then(|c| c.emulation_backend))
    }

    /// the port to use (initially)
    pub fn port(&self) -> u16 {
        self.args
            .port
            .or(self.config_toml.as_ref().and_then(|c| c.port))
            .unwrap_or(DEFAULT_PORT)
    }

    /// list of configured clients
    pub fn clients(&self) -> Vec<ConfigClient> {
        self.config_toml
            .as_ref()
            .map(|c| c.clients.clone())
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .map(From::<TomlClient>::from)
            .collect()
    }

    /// release bind for returning control to the host
    pub fn release_bind(&self) -> Vec<scancode::Linux> {
        self.config_toml
            .as_ref()
            .and_then(|c| c.release_bind.clone())
            .unwrap_or(Vec::from_iter(DEFAULT_RELEASE_KEYS.iter().cloned()))
    }

    /// whether UTF-8 text clipboard synchronization is enabled
    pub fn clipboard_sync(&self) -> bool {
        self.config_toml
            .as_ref()
            .and_then(|config| config.clipboard_sync)
            .unwrap_or(true)
    }

    /// the parsed config file, creating an empty one if none was loaded
    fn config_toml_mut(&mut self) -> &mut ConfigToml {
        self.config_toml.get_or_insert_with(Default::default)
    }

    /// set configured clients
    pub fn set_clients(&mut self, clients: Vec<ConfigClient>) {
        if clients.is_empty() {
            return;
        }
        self.config_toml_mut().clients =
            Some(clients.into_iter().map(|c| c.into()).collect::<Vec<_>>());
    }

    /// enable or disable text clipboard synchronization
    pub fn set_clipboard_sync(&mut self, enabled: bool) {
        self.config_toml_mut().clipboard_sync = Some(enabled);
    }

    /// set authorized keys
    pub fn set_authorized_keys(&mut self, fingerprints: HashMap<String, String>) {
        self.config_toml_mut().authorized_fingerprints = Some(fingerprints);
    }

    pub fn read_from_disk(&mut self) -> Result<bool, io::Error> {
        log::info!("reading config from {:?}", self.config_path);

        let current_config = fs::read_to_string(&self.config_path)?;
        let current_config = match current_config.parse::<DocumentMut>() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("{:?} {e}", self.config_path());
                return Ok(false);
            }
        };
        let mut changed = false;
        match toml_edit::de::from_document::<ConfigToml>(current_config) {
            Ok(current_config) => {
                changed = self
                    .config_toml
                    .as_ref()
                    .is_none_or(|c| c != &current_config);
                self.config_toml.replace(current_config);
            }
            Err(e) => log::warn!("{:?} {e}", self.config_path()),
        };
        if changed {
            log::info!("config changed");
        } else {
            log::info!("config unchanged");
        }
        Ok(changed)
    }

    pub fn write_back(&mut self) -> Result<(), io::Error> {
        log::info!("writing config to {:?}", self.config_path);
        /* the new config */
        let new_config = self.config_toml.clone().unwrap_or_default();
        let new_config = toml_edit::ser::to_string_pretty(&new_config)
            .map_err(|e| io::Error::other(format!("could not serialize the configuration: {e}")))?;

        /*
         * TODO merge with current config file to preserve comments
         * => eventually we might want to split this up into clients configured
         * via the config file and clients managed through the GUI / frontend.
         * The latter should be saved to $XDG_DATA_HOME instead of $XDG_CONFIG_HOME,
         * and clients configured through .config could be made permanent.
         * For now we just override the config file.
         */

        let _ = self.unwatch();
        /* write new config to file */
        if let Some(p) = self.config_path().parent() {
            fs::create_dir_all(p)?;
        }
        {
            let mut f = File::create(self.config_path())?;
            f.write_all(new_config.as_bytes())?;
            f.sync_all()?;
        }

        let _ = self.watch();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config file is written by users by hand and rewritten by the
    /// service, so the accepted keys, their defaults and the shape of what we
    /// write back are a compatibility contract with existing installations.
    /// The document below mirrors the shipped `config.toml` example.
    const SAMPLE: &str = r#"
release_bind = [ "KeyA", "KeyS", "KeyD", "KeyF" ]
port = 4242
clipboard_sync = false
capture_backend = "windows"
emulation_backend = "windows"
cert_path = "/tmp/cert.pem"

[authorized_fingerprints]
"aa:bb" = "iridium"

[[clients]]
position = "right"
hostname = "iridium"
activate_on_startup = true
ips = ["192.168.178.156"]

[[clients]]
position = "left"
hostname = "thorium"
ips = ["192.168.178.189", "192.168.178.172"]
port = 4243
"#;

    #[test]
    fn sample_config_parses_into_the_expected_values() {
        let config: ConfigToml = toml::from_str(SAMPLE).expect("parse sample config");

        assert_eq!(config.port, Some(4242));
        assert_eq!(config.clipboard_sync, Some(false));
        assert_eq!(config.capture_backend, Some(CaptureBackend::Windows));
        assert_eq!(config.emulation_backend, Some(EmulationBackend::Windows));
        assert_eq!(config.cert_path, Some(PathBuf::from("/tmp/cert.pem")));
        assert_eq!(
            config.release_bind,
            Some(vec![
                scancode::Linux::KeyA,
                scancode::Linux::KeyS,
                scancode::Linux::KeyD,
                scancode::Linux::KeyF,
            ])
        );
        assert_eq!(
            config.authorized_fingerprints,
            Some(HashMap::from([("aa:bb".to_owned(), "iridium".to_owned())]))
        );

        let clients = config.clients.expect("clients section");
        assert_eq!(clients.len(), 2);

        let first: ConfigClient = clients[0].clone().into();
        assert_eq!(first.hostname.as_deref(), Some("iridium"));
        assert_eq!(first.pos, Position::Right);
        assert!(first.active, "activate_on_startup maps to active");
        assert_eq!(first.port, DEFAULT_PORT, "an absent port means the default");
        assert_eq!(
            first.ips,
            HashSet::from(["192.168.178.156".parse().expect("valid test address")])
        );

        let second: ConfigClient = clients[1].clone().into();
        assert_eq!(second.pos, Position::Left);
        assert!(!second.active, "activate_on_startup defaults to false");
        assert_eq!(second.port, 4243);
        assert_eq!(second.ips.len(), 2);
    }

    #[test]
    fn omitted_keys_fall_back_to_documented_defaults() {
        let config: ConfigToml = toml::from_str("").expect("parse empty config");

        assert_eq!(config.port, None, "no port key means Config::port decides");
        assert_eq!(config.clients, None);
        assert_eq!(config.clipboard_sync, None);
        assert_eq!(config.release_bind, None);

        // the fallbacks Config applies for the absent keys
        assert_eq!(DEFAULT_PORT, 4242);
        assert!(
            DEFAULT_RELEASE_KEYS.contains(&scancode::Linux::KeyLeftAlt),
            "the default release bind is the four left modifiers"
        );
    }

    /// A client that survives a write-back must come back identical, or the
    /// service silently rewrites the user's clients on every save.
    #[test]
    fn clients_round_trip_through_the_config_file() {
        let original: Vec<ConfigClient> = toml::from_str::<ConfigToml>(SAMPLE)
            .expect("parse sample config")
            .clients
            .expect("clients section")
            .into_iter()
            .map(Into::into)
            .collect();

        let written = ConfigToml {
            clients: Some(original.iter().cloned().map(Into::into).collect()),
            ..Default::default()
        };
        let serialized = toml_edit::ser::to_string_pretty(&written).expect("serialize config");
        let reparsed: Vec<ConfigClient> = toml::from_str::<ConfigToml>(&serialized)
            .expect("reparse written config")
            .clients
            .expect("clients section")
            .into_iter()
            .map(Into::into)
            .collect();

        assert_eq!(reparsed, original);
    }

    #[test]
    fn default_port_is_omitted_when_writing_clients_back() {
        let client = ConfigClient {
            ips: HashSet::new(),
            hostname: Some("peer".into()),
            port: DEFAULT_PORT,
            pos: Position::Top,
            active: false,
            enter_hook: None,
        };

        let toml_client: TomlClient = client.into();
        assert_eq!(
            toml_client.port, None,
            "writing the default port back would pin it across upgrades"
        );
        assert_eq!(
            toml_client.activate_on_startup, None,
            "inactive clients omit the key rather than writing false"
        );
    }

    #[test]
    fn local_commit_is_always_eight_bytes() {
        assert_eq!(local_commit().len(), 8);
    }

    /// clap's own consistency checks: catches duplicate short flags, invalid
    /// value parsers and the like at test time rather than at first run.
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }

    #[test]
    fn cli_subcommands_and_flags_are_stable() {
        use clap::Parser;

        assert!(matches!(
            Args::parse_from(["crossdesk", "daemon"]).command,
            Some(Command::Daemon)
        ));
        assert!(matches!(
            Args::parse_from(["crossdesk", "test-capture"]).command,
            Some(Command::TestCapture(_))
        ));
        assert!(matches!(
            Args::parse_from(["crossdesk", "test-emulation"]).command,
            Some(Command::TestEmulation(_))
        ));
        assert!(
            Args::parse_from(["crossdesk"]).command.is_none(),
            "no subcommand starts the default (GUI) mode"
        );

        let args = Args::parse_from(["crossdesk", "--port", "1234"]);
        assert_eq!(args.port, Some(1234));
    }
}

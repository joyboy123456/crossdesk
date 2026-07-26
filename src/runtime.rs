use crate::{
    capture_test,
    config::{Command, Config, ConfigError},
    emulation_test,
    service::{Service, ServiceError},
};
use env_logger::Env;
use input_capture::InputCaptureError;
use input_emulation::InputEmulationError;
use lan_mouse_cli::CliError;
use lan_mouse_ipc::{IpcError, IpcListenerCreationError};
use std::{future::Future, io, process};
#[cfg(feature = "gui")]
use std::{
    process::Child,
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::task::LocalSet;

/// how long the GUI waits for a service it started to exit on its own before
/// killing it
#[cfg(feature = "gui")]
const SERVICE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// how long to wait for the IPC socket when probing for a running service
#[cfg(feature = "gui")]
const SERVICE_PROBE_TIMEOUT: Duration = Duration::from_millis(75);

/// polling interval while waiting for the owned service to exit
#[cfg(feature = "gui")]
const SERVICE_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Anything that can end the process.
///
/// Each variant names its subsystem: this error is what the user sees on
/// stderr, and a bare "No such file or directory" gives them nothing to act
/// on.
#[derive(Debug, Error)]
pub enum CrossDeskError {
    #[error("service: {0}")]
    Service(#[from] ServiceError),
    #[error("frontend ipc: {0}")]
    Ipc(#[from] IpcError),
    #[error("configuration: {0}")]
    Config(#[from] ConfigError),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("input capture: {0}")]
    Capture(#[from] InputCaptureError),
    #[error("input emulation: {0}")]
    Emulation(#[from] InputEmulationError),
    #[cfg(feature = "gui")]
    #[error("user interface: {0}")]
    Ui(#[from] crossdesk_ui::UiError),
    #[error("command line interface: {0}")]
    Cli(#[from] CliError),
}

pub fn main() {
    let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
    let _ = env_logger::Builder::from_env(env).try_init();

    if let Err(error) = run() {
        log::error!("{error}");
        process::exit(1);
    }
}

pub fn run() -> Result<(), CrossDeskError> {
    let config = Config::new()?;
    match config.command() {
        Some(Command::TestEmulation(args)) => run_async(emulation_test::run(config, args))?,
        Some(Command::TestCapture(args)) => run_async(capture_test::run(config, args))?,
        Some(Command::Cli(args)) => run_async(lan_mouse_cli::run(args))?,
        Some(Command::Daemon) => run_daemon(config)?,
        None => run_default(config)?,
    }

    Ok(())
}

/// Run the service in this process. A service that is already running is not
/// an error: the daemon simply hands over to the existing one.
fn run_daemon(config: Config) -> Result<(), CrossDeskError> {
    match run_async(run_service(config)) {
        Err(CrossDeskError::Service(ServiceError::IpcListen(
            IpcListenerCreationError::AlreadyRunning,
        ))) => {
            log::info!("service already running");
            Ok(())
        }
        result => result,
    }
}

#[cfg(feature = "gui")]
fn run_default(_config: Config) -> Result<(), CrossDeskError> {
    let mut service = start_service_if_needed()?;
    let result = crossdesk_ui::run(crate::config::local_commit(), service.is_some());

    if let Some(child) = service.as_mut() {
        finish_owned_service(child)?;
    }

    result?;
    Ok(())
}

#[cfg(not(feature = "gui"))]
fn run_default(config: Config) -> Result<(), CrossDeskError> {
    run_daemon(config)
}

fn run_async<F, E>(future: F) -> Result<(), CrossDeskError>
where
    F: Future<Output = Result<(), E>>,
    CrossDeskError: From<E>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    Ok(runtime.block_on(LocalSet::new().run_until(future))?)
}

#[cfg(feature = "gui")]
fn start_service_if_needed() -> Result<Option<Child>, io::Error> {
    if service_is_running() {
        log::info!("using existing service");
        return Ok(None);
    }

    let child = process::Command::new(std::env::current_exe()?)
        .args(std::env::args().skip(1))
        .arg("daemon")
        .spawn()?;
    Ok(Some(child))
}

#[cfg(feature = "gui")]
fn service_is_running() -> bool {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    else {
        return false;
    };
    runtime
        .block_on(lan_mouse_ipc::connect_async(Some(SERVICE_PROBE_TIMEOUT)))
        .is_ok()
}

#[cfg(feature = "gui")]
fn finish_owned_service(child: &mut Child) -> Result<(), io::Error> {
    let deadline = Instant::now() + SERVICE_SHUTDOWN_TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            log::warn!("service did not stop in time; terminating child process");
            child.kill()?;
            child.wait()?;
            return Ok(());
        }
        thread::sleep(SERVICE_SHUTDOWN_POLL_INTERVAL);
    }
}

async fn run_service(config: Config) -> Result<(), ServiceError> {
    let release_bind = config.release_bind();
    let config_path = config.config_path().to_owned();
    let mut service = Service::new(config).await?;
    log::info!("using config: {config_path:?}");
    log::info!("Press {release_bind:?} to release the mouse");
    service.run().await?;
    log::info!("service exited");
    Ok(())
}

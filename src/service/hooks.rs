//! User-configured commands run when the pointer enters a client.

use tokio::process::Command;

/// Run `cmd` through the platform's shell, so hooks can use pipes, quoting and
/// shell built-ins the way a user would type them.
fn shell_command(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(cmd);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-c").arg(cmd);
        command
    }
}

/// Run an enter hook in the background.
///
/// Detached on purpose: the hook is a user convenience, so nothing about
/// switching screens waits for it, and every failure is only logged.
pub(crate) fn spawn(cmd: String) {
    tokio::task::spawn_local(async move {
        log::info!("running enter hook: {cmd}");
        let mut child = match shell_command(&cmd).spawn() {
            Ok(child) => child,
            Err(e) => return log::warn!("could not run enter hook `{cmd}`: {e}"),
        };
        match child.wait().await {
            Ok(status) if status.success() => log::info!("{cmd} exited successfully"),
            Ok(status) => log::warn!("{cmd} exited with {status}"),
            Err(e) => log::warn!("{cmd}: {e}"),
        }
    });
}

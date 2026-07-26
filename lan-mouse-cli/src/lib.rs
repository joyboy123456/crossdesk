use clap::{Args, Parser, Subcommand};
use futures::StreamExt;

use std::{net::IpAddr, time::Duration};
use thiserror::Error;

use lan_mouse_ipc::{
    ClientHandle, ConnectionError, FrontendEvent, FrontendRequest, IpcError, Position,
    connect_async,
};

#[derive(Debug, Error)]
pub enum CliError {
    /// is the service running?
    #[error("could not connect: `{0}` - is the service running?")]
    ServiceNotRunning(#[from] ConnectionError),
    #[error("error communicating with service: {0}")]
    Ipc(#[from] IpcError),
}

#[derive(Parser, Clone, Debug, PartialEq, Eq)]
#[command(name = "lan-mouse-cli", about = "LanMouse CLI interface")]
pub struct CliArgs {
    #[command(subcommand)]
    command: CliSubcommand,
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
struct Client {
    #[arg(long)]
    hostname: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    ips: Option<Vec<IpAddr>>,
    #[arg(long)]
    enter_hook: Option<String>,
}

#[derive(Clone, Subcommand, Debug, PartialEq, Eq)]
enum CliSubcommand {
    /// add a new client
    AddClient(Client),
    /// remove an existing client
    RemoveClient { id: ClientHandle },
    /// activate a client
    Activate { id: ClientHandle },
    /// deactivate a client
    Deactivate { id: ClientHandle },
    /// list configured clients
    List,
    /// change hostname
    SetHost {
        id: ClientHandle,
        host: Option<String>,
    },
    /// change port
    SetPort { id: ClientHandle, port: u16 },
    /// set position
    SetPosition { id: ClientHandle, pos: Position },
    /// set ips
    SetIps { id: ClientHandle, ips: Vec<IpAddr> },
    /// re-enable capture
    EnableCapture,
    /// re-enable emulation
    EnableEmulation,
    /// authorize a public key
    AuthorizeKey {
        description: String,
        sha256_fingerprint: String,
    },
    /// deauthorize a public key
    RemoveAuthorizedKey { sha256_fingerprint: String },
    /// save configuration to file
    SaveConfig,
}

pub async fn run(args: CliArgs) -> Result<(), CliError> {
    execute(args.command).await?;
    Ok(())
}

async fn execute(cmd: CliSubcommand) -> Result<(), CliError> {
    let (mut rx, mut tx) = connect_async(Some(Duration::from_millis(500))).await?;
    match cmd {
        CliSubcommand::AddClient(Client {
            hostname,
            port,
            ips,
            enter_hook,
        }) => {
            tx.request(FrontendRequest::Create).await?;
            while let Some(e) = rx.next().await {
                if let FrontendEvent::Created(handle, _, _) = e? {
                    if let Some(hostname) = hostname {
                        tx.request(FrontendRequest::UpdateHostname(handle, Some(hostname)))
                            .await?;
                    }
                    if let Some(port) = port {
                        tx.request(FrontendRequest::UpdatePort(handle, port))
                            .await?;
                    }
                    if let Some(ips) = ips {
                        tx.request(FrontendRequest::UpdateFixIps(handle, ips))
                            .await?;
                    }
                    if let Some(enter_hook) = enter_hook {
                        tx.request(FrontendRequest::UpdateEnterHook(handle, Some(enter_hook)))
                            .await?;
                    }
                    break;
                }
            }
        }
        CliSubcommand::RemoveClient { id } => tx.request(FrontendRequest::Delete(id)).await?,
        CliSubcommand::Activate { id } => tx.request(FrontendRequest::Activate(id, true)).await?,
        CliSubcommand::Deactivate { id } => {
            tx.request(FrontendRequest::Activate(id, false)).await?
        }
        CliSubcommand::List => {
            tx.request(FrontendRequest::Enumerate()).await?;
            while let Some(e) = rx.next().await {
                if let FrontendEvent::Enumerate(clients) = e? {
                    for (handle, config, state) in clients {
                        let host = config.hostname.unwrap_or("unknown".to_owned());
                        let port = config.port;
                        let pos = config.pos;
                        let active = state.active;
                        let ips = state.ips;
                        println!(
                            "id {handle}: {host}:{port} ({pos}) active: {active}, ips: {ips:?}"
                        );
                    }
                    break;
                }
            }
        }
        CliSubcommand::SetHost { id, host } => {
            tx.request(FrontendRequest::UpdateHostname(id, host))
                .await?
        }
        CliSubcommand::SetPort { id, port } => {
            tx.request(FrontendRequest::UpdatePort(id, port)).await?
        }
        CliSubcommand::SetPosition { id, pos } => {
            tx.request(FrontendRequest::UpdatePosition(id, pos)).await?
        }
        CliSubcommand::SetIps { id, ips } => {
            tx.request(FrontendRequest::UpdateFixIps(id, ips)).await?
        }
        CliSubcommand::EnableCapture => tx.request(FrontendRequest::EnableCapture).await?,
        CliSubcommand::EnableEmulation => tx.request(FrontendRequest::EnableEmulation).await?,
        CliSubcommand::AuthorizeKey {
            description,
            sha256_fingerprint,
        } => {
            tx.request(FrontendRequest::AuthorizeKey(
                description,
                sha256_fingerprint,
            ))
            .await?
        }
        CliSubcommand::RemoveAuthorizedKey { sha256_fingerprint } => {
            tx.request(FrontendRequest::RemoveAuthorizedKey(sha256_fingerprint))
                .await?
        }
        CliSubcommand::SaveConfig => tx.request(FrontendRequest::SaveConfiguration).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    /// Users script against these subcommands, so their names and argument
    /// order are a compatibility contract.
    #[test]
    fn cli_definition_is_valid() {
        CliArgs::command().debug_assert();
    }

    fn parse(args: &[&str]) -> CliSubcommand {
        CliArgs::parse_from(std::iter::once("cli").chain(args.iter().copied())).command
    }

    #[test]
    fn subcommands_parse_as_documented() {
        assert_eq!(parse(&["list"]), CliSubcommand::List);
        assert_eq!(parse(&["activate", "3"]), CliSubcommand::Activate { id: 3 });
        assert_eq!(
            parse(&["deactivate", "3"]),
            CliSubcommand::Deactivate { id: 3 }
        );
        assert_eq!(
            parse(&["remove-client", "3"]),
            CliSubcommand::RemoveClient { id: 3 }
        );
        assert_eq!(
            parse(&["set-port", "3", "4242"]),
            CliSubcommand::SetPort { id: 3, port: 4242 }
        );
        assert_eq!(
            parse(&["set-position", "3", "right"]),
            CliSubcommand::SetPosition {
                id: 3,
                pos: Position::Right
            }
        );
        assert_eq!(
            parse(&["set-host", "3", "peer"]),
            CliSubcommand::SetHost {
                id: 3,
                host: Some("peer".into())
            }
        );
        assert_eq!(parse(&["enable-capture"]), CliSubcommand::EnableCapture);
        assert_eq!(parse(&["enable-emulation"]), CliSubcommand::EnableEmulation);
        assert_eq!(parse(&["save-config"]), CliSubcommand::SaveConfig);
        assert_eq!(
            parse(&["authorize-key", "desc", "aa:bb"]),
            CliSubcommand::AuthorizeKey {
                description: "desc".into(),
                sha256_fingerprint: "aa:bb".into()
            }
        );
    }

    #[test]
    fn add_client_flags_are_stable() {
        assert_eq!(
            parse(&[
                "add-client",
                "--hostname",
                "peer",
                "--port",
                "4243",
                "--ips",
                "10.0.0.1",
                "--enter-hook",
                "echo hi",
            ]),
            CliSubcommand::AddClient(Client {
                hostname: Some("peer".into()),
                port: Some(4243),
                ips: Some(vec!["10.0.0.1".parse().expect("valid test address")]),
                enter_hook: Some("echo hi".into()),
            })
        );

        // every flag is optional: a bare add-client must stay valid
        assert_eq!(
            parse(&["add-client"]),
            CliSubcommand::AddClient(Client {
                hostname: None,
                port: None,
                ips: None,
                enter_hook: None,
            })
        );
    }
}

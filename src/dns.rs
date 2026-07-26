use std::{collections::HashMap, io, net::IpAddr};

use tokio::net::lookup_host;
use tokio::task::spawn_local;

use tokio_util::sync::CancellationToken;

use lan_mouse_ipc::ClientHandle;

use crate::task::{Receiver, Sender, TaskHandle, channel, send};

pub(crate) struct DnsResolver {
    task: TaskHandle,
    request_tx: Sender<DnsRequest>,
    event_rx: Receiver<DnsEvent>,
}

struct DnsRequest {
    handle: ClientHandle,
    hostname: String,
}

pub(crate) enum DnsEvent {
    Resolving(ClientHandle),
    Resolved(ClientHandle, String, io::Result<Vec<IpAddr>>),
}

struct DnsTask {
    request_rx: Receiver<DnsRequest>,
    event_tx: Sender<DnsEvent>,
    cancellation_token: CancellationToken,
    active_tasks: HashMap<ClientHandle, tokio::task::JoinHandle<()>>,
}

impl DnsResolver {
    pub(crate) fn new() -> io::Result<Self> {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let cancellation_token = CancellationToken::new();
        let dns_task = DnsTask {
            active_tasks: Default::default(),
            request_rx,
            event_tx,
            cancellation_token: cancellation_token.clone(),
        };
        let task = TaskHandle::new(cancellation_token, spawn_local(dns_task.run()));
        Ok(Self {
            task,
            event_rx,
            request_tx,
        })
    }

    pub(crate) fn resolve(&self, handle: ClientHandle, hostname: String) {
        send(
            &self.request_tx,
            "dns request",
            DnsRequest { handle, hostname },
        );
    }

    /// The next resolver event, or `None` once the resolver has stopped.
    pub(crate) async fn event(&mut self) -> Option<DnsEvent> {
        self.event_rx.recv().await
    }

    pub(crate) async fn terminate(&mut self) {
        self.task.terminate("dns resolver").await;
    }
}

impl DnsTask {
    async fn run(mut self) {
        let cancellation_token = self.cancellation_token.clone();
        tokio::select! {
            _ = self.do_dns() => {},
            _ = cancellation_token.cancelled() => {},
        }
    }

    async fn do_dns(&mut self) {
        while let Some(dns_request) = self.request_rx.recv().await {
            let DnsRequest { handle, hostname } = dns_request;

            /* abort previous dns task */
            let previous_task = self.active_tasks.remove(&handle);
            if let Some(task) = previous_task {
                if !task.is_finished() {
                    task.abort();
                }
            }

            send(&self.event_tx, "dns progress", DnsEvent::Resolving(handle));

            /* spawn task for dns request */
            let event_tx = self.event_tx.clone();
            let cancellation_token = self.cancellation_token.clone();

            let task = spawn_local(async move {
                tokio::select! {
                    result = resolve_hostname(&hostname) => {
                        send(
                            &event_tx,
                            "dns result",
                            DnsEvent::Resolved(handle, hostname, result),
                        );
                    }
                    _ = cancellation_token.cancelled() => {},
                }
            });
            self.active_tasks.insert(handle, task);
        }
    }
}

/// Resolve `hostname` via the operating system's full name-resolution
/// stack (`getaddrinfo` on Unix, GetAddrInfoEx on Windows). This walks
/// `/etc/nsswitch.conf` on Linux — picking up mDNS via Avahi, /etc/hosts,
/// and DNS — and uses Bonjour for `.local` names on macOS. Pure-DNS
/// resolvers like hickory miss all of those, which is why a Bonjour
/// hostname (e.g. `JKMBP-M4-Max.local`) wouldn't resolve before.
///
/// Port `0` is a placeholder — `lookup_host` requires `host:port` but we
/// only care about the IPs at this stage; the actual port is appended at
/// connection time.
async fn resolve_hostname(hostname: &str) -> io::Result<Vec<IpAddr>> {
    let addrs = lookup_host((hostname, 0)).await?;
    Ok(addrs.map(|sa| sa.ip()).collect())
}

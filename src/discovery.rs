//! LAN discovery responder.
//!
//! Answers [`DISCOVERY_PROBE`] datagrams with an [`Announcement`] naming the
//! OS hostname and the port the service currently listens on, so frontends
//! can offer one-click "add device" without the user typing IP addresses.
//! The responder only ever *answers* probes; what it reveals (hostname,
//! port) is what any LAN host could learn by scanning anyway.

use std::{
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
};

use lan_mouse_proto::discovery::{Announcement, DISCOVERY_PORT, DISCOVERY_PROBE};
use tokio::{net::UdpSocket, task::spawn_local};
use tokio_util::sync::CancellationToken;

use crate::task::TaskHandle;

/// The probe is a fixed magic and the reply is bounded by
/// [`lan_mouse_proto::discovery::MAX_ANNOUNCEMENT_SIZE`], so a small buffer
/// sees every whole datagram.
const RECV_BUFFER_SIZE: usize = 512;

pub(crate) struct DiscoveryResponder {
    task: Option<TaskHandle>,
}

impl DiscoveryResponder {
    /// Bind the discovery port and answer probes until terminated.
    ///
    /// A bind failure (port already taken, sandbox restrictions) disables
    /// discovery instead of failing the service: input sharing works without
    /// it, only the "find devices" button comes up empty.
    pub(crate) async fn start(service_port: Arc<AtomicU16>) -> Self {
        let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).await {
            Ok(socket) => socket,
            Err(error) => {
                log::warn!(
                    "LAN discovery disabled: could not bind UDP port {DISCOVERY_PORT}: {error}"
                );
                return Self { task: None };
            }
        };

        let token = CancellationToken::new();
        let task = {
            let token = token.clone();
            TaskHandle::new(
                token.clone(),
                spawn_local(respond_loop(socket, service_port, token)),
            )
        };
        log::info!("answering LAN discovery probes on UDP port {DISCOVERY_PORT}");
        Self { task: Some(task) }
    }

    pub(crate) async fn terminate(&mut self) {
        if let Some(task) = &mut self.task {
            task.terminate("discovery responder").await;
        }
    }
}

async fn respond_loop(socket: UdpSocket, service_port: Arc<AtomicU16>, token: CancellationToken) {
    let hostname = os_hostname();
    let mut buf = [0u8; RECV_BUFFER_SIZE];
    loop {
        tokio::select! {
            _ = token.cancelled() => return,
            received = socket.recv_from(&mut buf) => {
                let Ok((len, peer)) = received else {
                    continue;
                };
                if &buf[..len] != DISCOVERY_PROBE {
                    continue;
                }
                let port = service_port.load(Ordering::Relaxed);
                let Ok(announcement) = Announcement::new(port, hostname.clone()) else {
                    log::warn!("not answering discovery probe: hostname is not encodable");
                    continue;
                };
                if let Err(error) = socket.send_to(&announcement.encode(), peer).await {
                    log::debug!("discovery reply to {peer} failed: {error}");
                }
            }
        }
    }
}

fn os_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::task::LocalSet;

    /// Run `f` the way the service runs: a current-thread runtime with a
    /// `LocalSet`, like `task::tests::run_local` but with IO enabled.
    fn run_local<F: std::future::Future<Output = ()> + 'static>(f: F) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("build test runtime");
        runtime.block_on(LocalSet::new().run_until(f));
    }

    #[test]
    fn probe_is_answered_with_an_announcement() {
        run_local(async {
            // Bind an ephemeral port and drive the loop directly, so the
            // test does not depend on the fixed discovery port being free.
            let responder_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind responder");
            let responder_addr = responder_socket.local_addr().expect("responder address");
            let token = CancellationToken::new();
            let task = spawn_local(respond_loop(
                responder_socket,
                Arc::new(AtomicU16::new(4242)),
                token.clone(),
            ));

            let prober = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind prober");
            prober
                .send_to(DISCOVERY_PROBE, responder_addr)
                .await
                .expect("send probe");
            // unknown datagrams must be ignored, not answered
            prober
                .send_to(b"hello", responder_addr)
                .await
                .expect("send garbage");

            let mut buf = [0u8; RECV_BUFFER_SIZE];
            let (len, _) = tokio::time::timeout(Duration::from_secs(5), prober.recv_from(&mut buf))
                .await
                .expect("announcement arrives in time")
                .expect("receive announcement");
            let announcement = Announcement::decode(&buf[..len]).expect("valid announcement");
            assert_eq!(announcement.port, 4242);

            token.cancel();
            task.await.expect("responder stops");
        });
    }
}

//! One-click LAN scan behind the "add device" button.
//!
//! [`start_scan`] runs on its own thread with a small tokio runtime, like
//! the IPC bridge: it broadcasts a discovery probe, collects announcements
//! for a short window and reports them back over a channel, repainting the
//! UI as results arrive. Everything about the wire format lives in
//! [`lan_mouse_proto::discovery`]; this module is only the async plumbing.

use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    thread,
    time::Duration,
};

use async_channel::{Receiver, Sender};
use eframe::egui;
use lan_mouse_proto::discovery::{Announcement, DISCOVERY_PORT, DISCOVERY_PROBE};

/// how long one scan listens for announcements
const SCAN_TIMEOUT: Duration = Duration::from_secs(2);

/// bounded per scan; far beyond plausible device counts on a LAN
const SCAN_EVENT_CAPACITY: usize = 64;

/// A device that answered a discovery probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoundDevice {
    /// hostname the device announced, empty when it has none
    pub hostname: String,
    /// address the announcement came from
    pub addr: IpAddr,
    /// port the device's service listens on
    pub port: u16,
}

impl FoundDevice {
    pub fn display_name(&self) -> &str {
        if self.hostname.is_empty() {
            "未知设备"
        } else {
            &self.hostname
        }
    }
}

pub enum ScanEvent {
    Found(FoundDevice),
    /// the scan could not run at all (e.g. no usable network socket)
    Failed(String),
    /// the listen window closed; no more events follow
    Finished,
}

/// Broadcast a probe and report what answers, on a background thread.
pub fn start_scan(ctx: egui::Context) -> Receiver<ScanEvent> {
    let (tx, rx) = async_channel::bounded(SCAN_EVENT_CAPACITY);
    thread::Builder::new()
        .name("crossdesk-scan".into())
        .spawn(move || run_worker(tx, ctx))
        .expect("spawn CrossDesk LAN scan");
    rx
}

fn run_worker(tx: Sender<ScanEvent>, ctx: egui::Context) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("build CrossDesk scan runtime");
    runtime.block_on(scan(tx, ctx));
}

async fn scan(tx: Sender<ScanEvent>, ctx: egui::Context) {
    let target = SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT));
    if let Err(error) = scan_target(target, &tx, &ctx).await {
        emit(&tx, &ctx, ScanEvent::Failed(error));
    }
    emit(&tx, &ctx, ScanEvent::Finished);
}

async fn scan_target(
    target: SocketAddr,
    tx: &Sender<ScanEvent>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let socket = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|error| format!("无法创建扫描套接字：{error}"))?;
    socket
        .set_broadcast(true)
        .map_err(|error| format!("无法启用广播：{error}"))?;
    socket
        .send_to(DISCOVERY_PROBE, target)
        .await
        .map_err(|error| format!("无法发送探测包：{error}"))?;

    let mut seen = HashSet::new();
    // comfortably larger than any encoded announcement (281 bytes)
    let mut buf = [0u8; 512];
    loop {
        let received = tokio::time::timeout(SCAN_TIMEOUT, socket.recv_from(&mut buf)).await;
        let Ok(Ok((len, peer))) = received else {
            // timeout (window over) or a transient recv error: either way the
            // scan is done - errors on an unconnected UDP socket are not
            // worth surfacing mid-scan
            return Ok(());
        };
        let Some(announcement) = Announcement::decode(&buf[..len]) else {
            continue;
        };
        if !seen.insert(peer.ip()) {
            continue;
        }
        emit(
            tx,
            ctx,
            ScanEvent::Found(FoundDevice {
                hostname: announcement.hostname,
                addr: peer.ip(),
                port: announcement.port,
            }),
        );
    }
}

fn emit(tx: &Sender<ScanEvent>, ctx: &egui::Context, event: ScanEvent) {
    // a full or closed channel means the dialog went away; either way the
    // event is simply not needed anymore
    let _ = tx.try_send(event);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_falls_back_for_nameless_devices() {
        let named = FoundDevice {
            hostname: "mac-mini".into(),
            addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 4242,
        };
        let nameless = FoundDevice {
            hostname: String::new(),
            ..named.clone()
        };
        assert_eq!(named.display_name(), "mac-mini");
        assert_eq!(nameless.display_name(), "未知设备");
    }

    #[test]
    fn scan_collects_announcements_and_ignores_garbage() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("build test runtime");
        runtime.block_on(async {
            // Directed probe instead of a broadcast: the test does not depend
            // on the machine's network setup, only on loopback.
            let responder = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind test responder");
            let responder_addr = responder.local_addr().expect("responder address");
            let answer = tokio::spawn(async move {
                let mut buf = [0u8; 64];
                let (len, peer) = responder.recv_from(&mut buf).await.expect("receive probe");
                assert_eq!(&buf[..len], DISCOVERY_PROBE);
                let announcement = Announcement::new(4242, "test-peer").expect("valid");
                responder
                    .send_to(&announcement.encode(), peer)
                    .await
                    .expect("send announcement");
                // garbage must not crash or produce an event
                responder
                    .send_to(b"garbage", peer)
                    .await
                    .expect("send garbage");
            });

            let (tx, rx) = async_channel::bounded(SCAN_EVENT_CAPACITY);
            scan_target(responder_addr, &tx, &egui::Context::default())
                .await
                .expect("scan succeeds");
            answer.await.expect("responder done");

            let mut found = Vec::new();
            while let Ok(ScanEvent::Found(device)) = rx.try_recv() {
                found.push(device);
            }
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].hostname, "test-peer");
            assert_eq!(found[0].port, 4242);
        });
    }
}

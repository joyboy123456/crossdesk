use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use async_channel::{Receiver, Sender, TryRecvError, TrySendError};
use eframe::egui;
use futures::StreamExt;
use lan_mouse_ipc::{FrontendEvent, FrontendRequest, connect_async};

const EVENT_CAPACITY: usize = 256;
const REQUEST_CAPACITY: usize = 64;

#[derive(Debug)]
pub enum BridgeEvent {
    Connected,
    Disconnected(String),
    Frontend(FrontendEvent),
}

pub struct Bridge {
    requests: Sender<FrontendRequest>,
    events: Receiver<BridgeEvent>,
    overflowed: Arc<AtomicBool>,
    #[cfg(test)]
    test_requests: Option<Receiver<FrontendRequest>>,
    #[cfg(test)]
    test_events: Option<Sender<BridgeEvent>>,
}

impl Bridge {
    pub fn start(ctx: egui::Context) -> Self {
        let (request_tx, request_rx) = async_channel::bounded(REQUEST_CAPACITY);
        let (event_tx, event_rx) = async_channel::bounded(EVENT_CAPACITY);
        let overflowed = Arc::new(AtomicBool::new(false));
        let overflowed_worker = overflowed.clone();

        thread::Builder::new()
            .name("crossdesk-ipc".into())
            .spawn(move || run_worker(request_rx, event_tx, overflowed_worker, ctx))
            .expect("spawn CrossDesk IPC bridge");

        Self {
            requests: request_tx,
            events: event_rx,
            overflowed,
            #[cfg(test)]
            test_requests: None,
            #[cfg(test)]
            test_events: None,
        }
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        let (request_tx, request_rx) = async_channel::bounded(REQUEST_CAPACITY);
        let (event_tx, event_rx) = async_channel::bounded(EVENT_CAPACITY);
        Self {
            requests: request_tx,
            events: event_rx,
            overflowed: Arc::new(AtomicBool::new(false)),
            test_requests: Some(request_rx),
            test_events: Some(event_tx),
        }
    }

    #[cfg(test)]
    pub fn try_test_request(&self) -> Option<FrontendRequest> {
        self.test_requests.as_ref()?.try_recv().ok()
    }

    #[cfg(test)]
    pub fn inject_test_event(&self, event: BridgeEvent) {
        self.test_events
            .as_ref()
            .expect("test event sender")
            .try_send(event)
            .expect("test event queue has capacity");
    }

    pub fn request(&self, request: FrontendRequest) -> Result<(), String> {
        self.requests
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    self.overflowed.store(true, Ordering::Release);
                    "操作队列已满，请稍后重试".to_owned()
                }
                TrySendError::Closed(_) => "后台服务连接已关闭".to_owned(),
            })
    }

    pub fn try_event(&self) -> Option<BridgeEvent> {
        self.events.try_recv().ok()
    }

    pub fn resync_if_overflowed(&self) -> bool {
        if !self.overflowed.swap(false, Ordering::AcqRel) {
            return false;
        }

        if self.requests.try_send(FrontendRequest::Sync).is_ok() {
            true
        } else {
            self.overflowed.store(true, Ordering::Release);
            false
        }
    }
}

fn run_worker(
    request_rx: Receiver<FrontendRequest>,
    event_tx: Sender<BridgeEvent>,
    overflowed: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("build CrossDesk IPC runtime");

    runtime.block_on(async move {
        let mut retry = Duration::from_millis(200);
        loop {
            match connect_async(Some(Duration::from_secs(3))).await {
                Ok((mut frontend_rx, mut frontend_tx)) => {
                    discard_pending_requests(&request_rx);
                    if request_rx.is_closed() {
                        return;
                    }
                    emit(&event_tx, BridgeEvent::Connected, &overflowed, &ctx);
                    if frontend_tx.request(FrontendRequest::Sync).await.is_err() {
                        continue;
                    }
                    retry = Duration::from_millis(200);

                    loop {
                        tokio::select! {
                            event = frontend_rx.next() => match event {
                                Some(Ok(event)) => emit(
                                    &event_tx,
                                    BridgeEvent::Frontend(event),
                                    &overflowed,
                                    &ctx,
                                ),
                                Some(Err(error)) => {
                                    emit(
                                        &event_tx,
                                        BridgeEvent::Disconnected(error.to_string()),
                                        &overflowed,
                                        &ctx,
                                    );
                                    break;
                                }
                                None => {
                                    emit(
                                        &event_tx,
                                        BridgeEvent::Disconnected("后台服务已断开".into()),
                                        &overflowed,
                                        &ctx,
                                    );
                                    break;
                                }
                            },
                            request = request_rx.recv() => match request {
                                Ok(request) => {
                                    if let Err(error) = frontend_tx.request(request).await {
                                        emit(
                                            &event_tx,
                                            BridgeEvent::Disconnected(error.to_string()),
                                            &overflowed,
                                            &ctx,
                                        );
                                        break;
                                    }
                                }
                                Err(_) => return,
                            }
                        }
                    }
                }
                Err(error) => emit(
                    &event_tx,
                    BridgeEvent::Disconnected(error.to_string()),
                    &overflowed,
                    &ctx,
                ),
            }

            discard_pending_requests(&request_rx);
            if request_rx.is_closed() {
                return;
            }
            tokio::time::sleep(retry).await;
            retry = retry.saturating_mul(2).min(Duration::from_secs(3));
        }
    });
}

fn discard_pending_requests(request_rx: &Receiver<FrontendRequest>) -> usize {
    let mut discarded = 0;
    loop {
        match request_rx.try_recv() {
            Ok(_) => discarded += 1,
            Err(TryRecvError::Empty | TryRecvError::Closed) => return discarded,
        }
    }
}

fn emit(
    event_tx: &Sender<BridgeEvent>,
    event: BridgeEvent,
    overflowed: &AtomicBool,
    ctx: &egui::Context,
) {
    if event_tx.try_send(event).is_err() {
        overflowed.store(true, Ordering::Release);
    }
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_overflow_schedules_sync_when_capacity_returns() {
        let bridge = Bridge::for_test();
        for _ in 0..REQUEST_CAPACITY {
            bridge.request(FrontendRequest::Sync).expect("fill queue");
        }
        assert!(bridge.request(FrontendRequest::Sync).is_err());

        assert_eq!(bridge.try_test_request(), Some(FrontendRequest::Sync));
        assert!(bridge.resync_if_overflowed());

        let mut requests = Vec::new();
        while let Some(request) = bridge.try_test_request() {
            requests.push(request);
        }
        assert_eq!(requests.len(), REQUEST_CAPACITY);
        assert_eq!(requests.last(), Some(&FrontendRequest::Sync));
    }

    #[test]
    fn disconnect_discards_requests_waiting_for_the_old_connection() {
        let bridge = Bridge::for_test();
        bridge
            .request(FrontendRequest::UpdatePosition(
                3,
                lan_mouse_ipc::Position::Left,
            ))
            .expect("queue position update");
        bridge
            .request(FrontendRequest::Activate(4, true))
            .expect("queue activation");

        let requests = bridge
            .test_requests
            .as_ref()
            .expect("test request receiver");
        assert_eq!(discard_pending_requests(requests), 2);
        assert!(bridge.try_test_request().is_none());
    }
}

use super::*;
use std::{
    sync::{Mutex, mpsc as std_mpsc},
    time::Instant,
};

#[derive(Default)]
struct AlwaysChanged;

impl ChangeDetector for AlwaysChanged {
    fn should_read(&mut self, _force: bool) -> bool {
        true
    }

    fn reset(&mut self) {}
}

#[derive(Default)]
struct ForceOnly;

impl ChangeDetector for ForceOnly {
    fn should_read(&mut self, force: bool) -> bool {
        force
    }

    fn reset(&mut self) {}
}

struct BlockingReader {
    started_tx: std_mpsc::Sender<()>,
    release_rx: std_mpsc::Receiver<Result<String, ClipboardReadError>>,
}

impl ClipboardBackend for BlockingReader {
    fn get_text(&mut self) -> Result<String, ClipboardReadError> {
        let _ = self.started_tx.send(());
        self.release_rx
            .recv()
            .unwrap_or_else(|_| Err(ClipboardReadError::Transient("reader released".into())))
    }

    fn set_text(&mut self, _text: String) -> Result<(), ClipboardWriteError> {
        unreachable!("reader backend must not write")
    }
}

struct ScriptedWriter {
    calls_tx: std_mpsc::Sender<String>,
    results: VecDeque<Result<(), ClipboardWriteError>>,
}

impl ClipboardBackend for ScriptedWriter {
    fn get_text(&mut self) -> Result<String, ClipboardReadError> {
        unreachable!("writer backend must not read")
    }

    fn set_text(&mut self, text: String) -> Result<(), ClipboardWriteError> {
        let _ = self.calls_tx.send(text);
        self.results.pop_front().unwrap_or(Ok(()))
    }
}

struct BlockingWriter {
    calls_tx: std_mpsc::Sender<String>,
    release_rx: std_mpsc::Receiver<Result<(), ClipboardWriteError>>,
}

impl ClipboardBackend for BlockingWriter {
    fn get_text(&mut self) -> Result<String, ClipboardReadError> {
        unreachable!("writer backend must not read")
    }

    fn set_text(&mut self, text: String) -> Result<(), ClipboardWriteError> {
        let _ = self.calls_tx.send(text);
        self.release_rx.recv().unwrap_or_else(|_| {
            Err(ClipboardWriteError::MayHaveChanged(
                "writer released".into(),
            ))
        })
    }
}

fn one_shot_factory(backend: impl ClipboardBackend + 'static) -> ClipboardFactory {
    let backend = Arc::new(Mutex::new(Some(
        Box::new(backend) as Box<dyn ClipboardBackend>
    )));
    Arc::new(move || {
        backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| "test clipboard was already created".into())
    })
}

fn test_sync(
    reader: impl ClipboardBackend + 'static,
    writer: impl ClipboardBackend + 'static,
) -> ClipboardSync {
    test_sync_with_detector(reader, writer, Box::new(AlwaysChanged))
}

fn test_sync_with_detector(
    reader: impl ClipboardBackend + 'static,
    writer: impl ClipboardBackend + 'static,
    change_detector: Box<dyn ChangeDetector>,
) -> ClipboardSync {
    ClipboardSync::start(
        true,
        Duration::from_millis(10),
        one_shot_factory(reader),
        one_shot_factory(writer),
        change_detector,
    )
}

async fn wait_for_event(
    sync: &mut ClipboardSync,
    predicate: impl Fn(&ClipboardEvent) -> bool,
) -> ClipboardEvent {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = sync.event().await.expect("clipboard event channel closed");
            if predicate(&event) {
                return event;
            }
        }
    })
    .await
    .expect("timed out waiting for clipboard event")
}

async fn assert_no_remote_applied(sync: &mut ClipboardSync) {
    let result = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            let event = sync.event().await?;
            if matches!(event, ClipboardEvent::RemoteTextApplied(_)) {
                return Some(event);
            }
        }
    })
    .await;
    assert!(result.is_err(), "failed write was reported as applied");
}

#[tokio::test]
async fn blocked_read_does_not_delay_remote_write_or_emit_stale_text() {
    let (read_started_tx, read_started_rx) = std_mpsc::channel();
    let (release_read_tx, release_read_rx) = std_mpsc::channel();
    let (write_calls_tx, write_calls_rx) = std_mpsc::channel();
    let reader = BlockingReader {
        started_tx: read_started_tx,
        release_rx: release_read_rx,
    };
    let writer = ScriptedWriter {
        calls_tx: write_calls_tx,
        results: VecDeque::from([Ok(())]),
    };
    let mut sync = test_sync(reader, writer);

    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not start");
    assert!(sync.queue_remote_text("remote".into()));
    assert_eq!(
        write_calls_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("remote write was blocked by the reader"),
        "remote"
    );
    assert_eq!(
        wait_for_event(&mut sync, |event| {
            matches!(event, ClipboardEvent::RemoteTextApplied(_))
        })
        .await,
        ClipboardEvent::RemoteTextApplied("remote".into())
    );

    release_read_tx
        .send(Ok("stale".into()))
        .expect("release stale read");
    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not begin its next poll");
    let stale = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            let event = sync.event().await?;
            if event == ClipboardEvent::LocalText("stale".into()) {
                return Some(event);
            }
        }
    })
    .await;
    assert!(
        stale.is_err(),
        "stale read was broadcast after remote write"
    );

    release_read_tx
        .send(Ok("remote".into()))
        .expect("release matching remote read");
    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not begin its third poll");
    let echo = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            let event = sync.event().await?;
            if event == ClipboardEvent::LocalText("remote".into()) {
                return Some(event);
            }
        }
    })
    .await;
    assert!(echo.is_err(), "remote write echoed back as a local change");

    release_read_tx
        .send(Ok("fresh".into()))
        .expect("release fresh local read");
    assert_eq!(
        wait_for_event(&mut sync, |event| {
            event == &ClipboardEvent::LocalText("fresh".into())
        })
        .await,
        ClipboardEvent::LocalText("fresh".into())
    );

    let started = Instant::now();
    tokio::time::timeout(Duration::from_millis(500), sync.terminate())
        .await
        .expect("termination waited for the blocked reader");
    assert!(started.elapsed() < Duration::from_millis(500));
    drop(release_read_tx);
}

#[tokio::test]
async fn failed_write_stays_pending_until_it_succeeds() {
    let (read_started_tx, read_started_rx) = std_mpsc::channel();
    let (release_read_tx, release_read_rx) = std_mpsc::channel();
    let (write_calls_tx, write_calls_rx) = std_mpsc::channel();
    let reader = BlockingReader {
        started_tx: read_started_tx,
        release_rx: release_read_rx,
    };
    let writer = ScriptedWriter {
        calls_tx: write_calls_tx,
        results: VecDeque::from([
            Err(ClipboardWriteError::MayHaveChanged("busy".into())),
            Err(ClipboardWriteError::MayHaveChanged("busy".into())),
            Ok(()),
        ]),
    };
    let mut sync = test_sync(reader, writer);

    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not start");
    assert!(sync.queue_remote_text("retry".into()));
    assert_eq!(
        write_calls_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("missing failed write attempt"),
        "retry"
    );
    assert_no_remote_applied(&mut sync).await;
    for _ in 0..2 {
        assert_eq!(
            write_calls_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("pending text was not retried"),
            "retry"
        );
    }
    assert_eq!(
        wait_for_event(&mut sync, |event| {
            matches!(event, ClipboardEvent::RemoteTextApplied(_))
        })
        .await,
        ClipboardEvent::RemoteTextApplied("retry".into())
    );

    sync.terminate().await;
    drop(release_read_tx);
}

#[tokio::test]
async fn occupied_write_invalidates_older_read_and_keeps_retrying() {
    let (read_started_tx, read_started_rx) = std_mpsc::channel();
    let (release_read_tx, release_read_rx) = std_mpsc::channel();
    let (write_calls_tx, write_calls_rx) = std_mpsc::channel();
    let reader = BlockingReader {
        started_tx: read_started_tx,
        release_rx: release_read_rx,
    };
    let writer = ScriptedWriter {
        calls_tx: write_calls_tx,
        results: VecDeque::from([Err(ClipboardWriteError::Occupied("busy".into())), Ok(())]),
    };
    let mut sync = test_sync(reader, writer);

    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not start");
    assert!(sync.queue_remote_text("remote".into()));
    assert_eq!(
        write_calls_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("remote write did not start"),
        "remote"
    );
    release_read_tx
        .send(Ok("older-local".into()))
        .expect("release older local read");
    assert_eq!(
        write_calls_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("occupied remote write was not retried"),
        "remote"
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = sync.event().await.expect("clipboard event channel closed");
            assert_ne!(
                event,
                ClipboardEvent::LocalText("older-local".into()),
                "pre-write read cancelled the pending remote value"
            );
            if event == ClipboardEvent::RemoteTextApplied("remote".into()) {
                break;
            }
        }
    })
    .await
    .expect("remote retry was not reported as applied");

    sync.terminate().await;
    drop(release_read_tx);
}

#[tokio::test]
async fn latest_remote_text_wins_while_an_older_write_is_in_flight() {
    let (read_started_tx, read_started_rx) = std_mpsc::channel();
    let (release_read_tx, release_read_rx) = std_mpsc::channel();
    let (write_calls_tx, write_calls_rx) = std_mpsc::channel();
    let (release_write_tx, release_write_rx) = std_mpsc::channel();
    let reader = BlockingReader {
        started_tx: read_started_tx,
        release_rx: release_read_rx,
    };
    let writer = BlockingWriter {
        calls_tx: write_calls_tx,
        release_rx: release_write_rx,
    };
    let mut sync = test_sync(reader, writer);

    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not start");
    release_read_tx
        .send(Ok("original".into()))
        .expect("release original local read");
    wait_for_event(&mut sync, |event| {
        event == &ClipboardEvent::LocalText("original".into())
    })
    .await;
    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not block again");

    assert!(sync.queue_remote_text("older".into()));
    assert_eq!(
        write_calls_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("older write did not start"),
        "older"
    );
    assert!(sync.queue_remote_text("original".into()));
    release_write_tx.send(Ok(())).expect("release older write");
    assert_eq!(
        wait_for_event(&mut sync, |event| {
            event == &ClipboardEvent::RemoteTextApplied("older".into())
        })
        .await,
        ClipboardEvent::RemoteTextApplied("older".into())
    );

    assert_eq!(
        write_calls_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("latest write did not start"),
        "original"
    );
    release_write_tx.send(Ok(())).expect("release latest write");
    assert_eq!(
        wait_for_event(&mut sync, |event| {
            event == &ClipboardEvent::RemoteTextApplied("original".into())
        })
        .await,
        ClipboardEvent::RemoteTextApplied("original".into())
    );

    sync.terminate().await;
    drop(release_read_tx);
}

#[tokio::test]
async fn transient_read_error_is_retried_once() {
    let (read_started_tx, read_started_rx) = std_mpsc::channel();
    let (release_read_tx, release_read_rx) = std_mpsc::channel();
    let (write_calls_tx, _write_calls_rx) = std_mpsc::channel();
    let reader = BlockingReader {
        started_tx: read_started_tx,
        release_rx: release_read_rx,
    };
    let writer = ScriptedWriter {
        calls_tx: write_calls_tx,
        results: VecDeque::new(),
    };
    let mut sync = test_sync(reader, writer);

    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not start");
    release_read_tx
        .send(Err(ClipboardReadError::Transient("temporary".into())))
        .expect("release failed read");
    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("transient read was not retried");
    release_read_tx
        .send(Ok("recovered".into()))
        .expect("release successful retry");
    assert_eq!(
        wait_for_event(&mut sync, |event| {
            event == &ClipboardEvent::LocalText("recovered".into())
        })
        .await,
        ClipboardEvent::LocalText("recovered".into())
    );

    sync.terminate().await;
    drop(release_read_tx);
}

#[tokio::test]
async fn latest_disabled_state_survives_a_blocked_reader() {
    let (read_started_tx, read_started_rx) = std_mpsc::channel();
    let (release_read_tx, release_read_rx) = std_mpsc::channel();
    let (write_calls_tx, _write_calls_rx) = std_mpsc::channel();
    let reader = BlockingReader {
        started_tx: read_started_tx,
        release_rx: release_read_rx,
    };
    let writer = ScriptedWriter {
        calls_tx: write_calls_tx,
        results: VecDeque::new(),
    };
    let mut sync = test_sync(reader, writer);

    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not start");
    sync.set_enabled(false);
    sync.set_enabled(true);
    sync.set_enabled(false);
    release_read_tx
        .send(Ok("ignored".into()))
        .expect("release blocked read");
    assert!(
        read_started_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "reader continued polling after the latest disabled state"
    );

    sync.terminate().await;
    drop(release_read_tx);
}

#[tokio::test]
async fn disable_then_enable_forces_a_read_even_when_final_state_is_unchanged() {
    let (read_started_tx, read_started_rx) = std_mpsc::channel();
    let (release_read_tx, release_read_rx) = std_mpsc::channel();
    let (write_calls_tx, _write_calls_rx) = std_mpsc::channel();
    let reader = BlockingReader {
        started_tx: read_started_tx,
        release_rx: release_read_rx,
    };
    let writer = ScriptedWriter {
        calls_tx: write_calls_tx,
        results: VecDeque::new(),
    };
    let mut sync = test_sync_with_detector(reader, writer, Box::new(ForceOnly));

    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader did not start");
    sync.set_enabled(false);
    sync.set_enabled(true);
    release_read_tx
        .send(Ok("before-toggle".into()))
        .expect("release pre-toggle read");
    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("re-enabling did not force a fresh read");
    let pre_toggle = tokio::time::timeout(Duration::from_millis(50), async {
        loop {
            let event = sync.event().await?;
            if event == ClipboardEvent::LocalText("before-toggle".into()) {
                return Some(event);
            }
        }
    })
    .await;
    assert!(
        pre_toggle.is_err(),
        "read started before the toggle was accepted afterwards"
    );
    release_read_tx
        .send(Ok("after-toggle".into()))
        .expect("release post-toggle read");
    assert_eq!(
        wait_for_event(&mut sync, |event| {
            event == &ClipboardEvent::LocalText("after-toggle".into())
        })
        .await,
        ClipboardEvent::LocalText("after-toggle".into())
    );

    sync.terminate().await;
    drop(release_read_tx);
}

#[test]
fn token_comparison_skips_unchanged_supported_clipboard() {
    let mut last = None;
    let local = |token| {
        Some(ClipboardChangeSnapshot {
            token,
            is_remote: false,
        })
    };
    assert!(should_read_clipboard_change(&mut last, local(7), false));
    assert!(!should_read_clipboard_change(&mut last, local(7), false));
    assert!(should_read_clipboard_change(&mut last, local(7), true));
    assert!(should_read_clipboard_change(&mut last, local(8), false));
    assert!(should_read_clipboard_change(&mut last, None, false));
}

#[test]
fn remote_placeholder_is_consumed_without_reading_its_contents() {
    let mut last = None;
    let remote = Some(ClipboardChangeSnapshot {
        token: 7,
        is_remote: true,
    });
    assert!(!should_read_clipboard_change(&mut last, remote, false));
    assert_eq!(last, Some(7));
    assert!(!should_read_clipboard_change(&mut last, remote, true));

    let local = Some(ClipboardChangeSnapshot {
        token: 8,
        is_remote: false,
    });
    assert!(should_read_clipboard_change(&mut last, local, false));
}

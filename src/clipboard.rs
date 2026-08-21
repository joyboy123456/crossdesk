use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use arboard::Clipboard;
use tokio::sync::mpsc;

#[cfg(target_os = "macos")]
const POLL_INTERVAL: Duration = Duration::from_millis(50);
#[cfg(not(target_os = "macos"))]
const POLL_INTERVAL: Duration = Duration::from_millis(350);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const COORDINATOR_INTERVAL: Duration = Duration::from_millis(25);
const APPLY_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const REQUEST_CAPACITY: usize = 8;
const READER_REQUEST_CAPACITY: usize = 2;
const EVENT_CAPACITY: usize = 8;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClipboardEvent {
    LocalText(String),
    RemoteTextApplied(String),
    Availability(bool),
}

enum ClipboardRequest {
    Apply(String),
    SetEnabled,
    ReadComplete {
        revision: u64,
        config_epoch: u64,
        retry: bool,
        result: Result<String, ClipboardReadError>,
    },
    Availability(bool),
    Terminate,
}

enum ReaderRequest {
    Wake,
    Terminate,
}

#[derive(Debug, PartialEq, Eq)]
enum ClipboardReadError {
    ContentNotAvailable,
    Transient(String),
}

#[derive(Debug, PartialEq, Eq)]
enum ClipboardWriteError {
    Occupied(String),
    MayHaveChanged(String),
}

impl fmt::Display for ClipboardWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Occupied(error) | Self::MayHaveChanged(error) => formatter.write_str(error),
        }
    }
}

trait ClipboardBackend: Send {
    fn get_text(&mut self) -> Result<String, ClipboardReadError>;
    fn set_text(&mut self, text: String) -> Result<(), ClipboardWriteError>;
}

impl ClipboardBackend for Clipboard {
    fn get_text(&mut self) -> Result<String, ClipboardReadError> {
        Clipboard::get_text(self).map_err(|error| match error {
            arboard::Error::ContentNotAvailable => ClipboardReadError::ContentNotAvailable,
            error => ClipboardReadError::Transient(error.to_string()),
        })
    }

    fn set_text(&mut self, text: String) -> Result<(), ClipboardWriteError> {
        Clipboard::set_text(self, text).map_err(|error| match error {
            arboard::Error::ClipboardOccupied => ClipboardWriteError::Occupied(error.to_string()),
            error => ClipboardWriteError::MayHaveChanged(error.to_string()),
        })
    }
}

type ClipboardFactory =
    Arc<dyn Fn() -> Result<Box<dyn ClipboardBackend>, String> + Send + Sync + 'static>;

trait ChangeDetector: Send {
    fn should_read(&mut self, force: bool) -> bool;
    fn reset(&mut self);
}

#[derive(Default)]
struct PlatformChangeDetector {
    last_token: Option<isize>,
}

impl ChangeDetector for PlatformChangeDetector {
    fn should_read(&mut self, force: bool) -> bool {
        should_read_clipboard_change(&mut self.last_token, clipboard_change_snapshot(), force)
    }

    fn reset(&mut self) {
        self.last_token = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClipboardChangeSnapshot {
    token: isize,
    is_remote: bool,
}

fn should_read_clipboard_change(
    last_token: &mut Option<isize>,
    snapshot: Option<ClipboardChangeSnapshot>,
    force: bool,
) -> bool {
    let Some(snapshot) = snapshot else {
        // Platforms without a cheap sequence number retain text polling.
        return true;
    };
    let changed = force || *last_token != Some(snapshot.token);
    *last_token = Some(snapshot.token);
    if changed && snapshot.is_remote {
        log::debug!("skipping Apple Universal Clipboard placeholder");
    }
    changed && !snapshot.is_remote
}

#[cfg(target_os = "macos")]
fn clipboard_change_snapshot() -> Option<ClipboardChangeSnapshot> {
    use objc2::{ClassType, msg_send, rc::Retained};
    use objc2_app_kit::NSPasteboard;

    let pasteboard: Option<Retained<NSPasteboard>> =
        unsafe { msg_send![NSPasteboard::class(), generalPasteboard] };
    pasteboard.map(|pasteboard| {
        const REMOTE_CLIPBOARD_TYPE: &str = "com.apple.is-remote-clipboard";
        let is_remote = pasteboard.types().is_some_and(|types| {
            types
                .iter()
                .any(|kind| kind.to_string() == REMOTE_CLIPBOARD_TYPE)
        });
        ClipboardChangeSnapshot {
            token: pasteboard.changeCount(),
            is_remote,
        }
    })
}

#[cfg(not(target_os = "macos"))]
fn clipboard_change_snapshot() -> Option<ClipboardChangeSnapshot> {
    None
}

pub(crate) struct ClipboardSync {
    request_tx: SyncSender<ClipboardRequest>,
    event_rx: mpsc::Receiver<ClipboardEvent>,
    stop: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    config_epoch: Arc<AtomicU64>,
    coordinator: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
}

impl ClipboardSync {
    pub(crate) fn new(enabled: bool) -> Self {
        Self::start(
            enabled,
            POLL_INTERVAL,
            system_clipboard_factory(),
            system_clipboard_factory(),
            Box::<PlatformChangeDetector>::default(),
        )
    }

    fn start(
        enabled: bool,
        poll_interval: Duration,
        reader_factory: ClipboardFactory,
        writer_factory: ClipboardFactory,
        change_detector: Box<dyn ChangeDetector>,
    ) -> Self {
        let (request_tx, request_rx) = sync_channel(REQUEST_CAPACITY);
        let (reader_request_tx, reader_request_rx) = sync_channel(READER_REQUEST_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let enabled_state = Arc::new(AtomicBool::new(enabled));
        let config_epoch = Arc::new(AtomicU64::new(0));
        let revision = Arc::new(AtomicU64::new(0));
        let reset_read = Arc::new(AtomicBool::new(false));
        let retry_read = Arc::new(AtomicBool::new(false));
        let write_pending = Arc::new(AtomicBool::new(false));

        let coordinator_stop = Arc::clone(&stop);
        let coordinator_revision = Arc::clone(&revision);
        let coordinator_enabled = Arc::clone(&enabled_state);
        let coordinator_config_epoch = Arc::clone(&config_epoch);
        let coordinator_reset_read = Arc::clone(&reset_read);
        let coordinator_retry_read = Arc::clone(&retry_read);
        let coordinator_write_pending = Arc::clone(&write_pending);
        let coordinator = thread::Builder::new()
            .name("crossdesk-clipboard-writer".into())
            .spawn(move || {
                run_coordinator(
                    request_rx,
                    reader_request_tx,
                    event_tx,
                    coordinator_stop,
                    coordinator_revision,
                    coordinator_enabled,
                    coordinator_config_epoch,
                    coordinator_reset_read,
                    coordinator_retry_read,
                    coordinator_write_pending,
                    writer_factory,
                    enabled,
                )
            })
            .expect("spawn clipboard writer");

        let reader_stop = Arc::clone(&stop);
        let reader_revision = Arc::clone(&revision);
        let reader_enabled = Arc::clone(&enabled_state);
        let reader_config_epoch = Arc::clone(&config_epoch);
        let reader_reset_read = Arc::clone(&reset_read);
        let reader_retry_read = Arc::clone(&retry_read);
        let reader_write_pending = Arc::clone(&write_pending);
        let reader_tx = request_tx.clone();
        let reader = thread::Builder::new()
            .name("crossdesk-clipboard-reader".into())
            .spawn(move || {
                run_reader(
                    reader_request_rx,
                    reader_tx,
                    reader_stop,
                    reader_revision,
                    reader_enabled,
                    reader_config_epoch,
                    reader_reset_read,
                    reader_retry_read,
                    reader_write_pending,
                    reader_factory,
                    change_detector,
                    poll_interval,
                )
            })
            .expect("spawn clipboard reader");

        Self {
            request_tx,
            event_rx,
            stop,
            enabled: enabled_state,
            config_epoch,
            coordinator: Some(coordinator),
            reader: Some(reader),
        }
    }

    pub(crate) async fn event(&mut self) -> Option<ClipboardEvent> {
        self.event_rx.recv().await
    }

    pub(crate) fn queue_remote_text(&self, text: String) -> bool {
        match self.request_tx.try_send(ClipboardRequest::Apply(text)) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("failed to queue remote clipboard text: {error}");
                false
            }
        }
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        self.config_epoch.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self.request_tx.try_send(ClipboardRequest::SetEnabled) {
            log::warn!("failed to update clipboard sync state: {error}");
        }
    }

    /// Stop the writer without waiting for a platform read that is already blocked.
    ///
    /// macOS may spend several seconds materializing Universal Clipboard data.
    /// The reader is detached after receiving the shared stop signal and exits
    /// when that platform call returns; joining it would reintroduce the same
    /// delay during application shutdown.
    pub(crate) async fn terminate(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.request_tx.try_send(ClipboardRequest::Terminate);
        let coordinator = self.coordinator.take();
        let _ = self.reader.take();

        let Some(coordinator) = coordinator else {
            return;
        };
        match tokio::task::spawn_blocking(move || coordinator.join()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => log::warn!("clipboard writer panicked"),
            Err(error) => log::warn!("failed to join clipboard writer: {error}"),
        }
    }
}

impl Drop for ClipboardSync {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.request_tx.try_send(ClipboardRequest::Terminate);
    }
}

fn system_clipboard_factory() -> ClipboardFactory {
    Arc::new(|| {
        Clipboard::new()
            .map(|clipboard| Box::new(clipboard) as Box<dyn ClipboardBackend>)
            .map_err(|error| error.to_string())
    })
}

struct PendingRemote {
    text: String,
    failures: u32,
}

#[allow(clippy::too_many_arguments)]
fn run_coordinator(
    request_rx: Receiver<ClipboardRequest>,
    reader_request_tx: SyncSender<ReaderRequest>,
    event_tx: mpsc::Sender<ClipboardEvent>,
    stop: Arc<AtomicBool>,
    revision: Arc<AtomicU64>,
    enabled_state: Arc<AtomicBool>,
    config_epoch: Arc<AtomicU64>,
    reset_read: Arc<AtomicBool>,
    retry_read: Arc<AtomicBool>,
    write_pending: Arc<AtomicBool>,
    writer_factory: ClipboardFactory,
    mut enabled: bool,
) {
    let mut writer = match create_writer(&writer_factory) {
        Ok(writer) => Some(writer),
        Err(error) => {
            log::warn!("clipboard writer is unavailable: {error}");
            None
        }
    };
    let mut last_text = None;
    let mut pending_remote: Option<PendingRemote> = None;
    let mut retry_at = Instant::now();
    let mut pending_events = VecDeque::new();
    let mut observed_config_epoch = config_epoch.load(Ordering::Acquire);

    while !stop.load(Ordering::Acquire) {
        sync_config_state(
            &enabled_state,
            &config_epoch,
            &mut observed_config_epoch,
            &mut enabled,
            &mut last_text,
            &mut pending_remote,
            &mut pending_events,
            &reader_request_tx,
            &write_pending,
        );

        if enabled && pending_remote.is_some() && Instant::now() >= retry_at {
            let text = pending_remote
                .as_ref()
                .expect("pending remote text disappeared")
                .text
                .clone();
            if writer.is_none() {
                writer = match create_writer(&writer_factory) {
                    Ok(writer) => Some(writer),
                    Err(error) => {
                        record_remote_failure(
                            pending_remote
                                .as_mut()
                                .expect("pending remote text disappeared"),
                            &error,
                        );
                        retry_at = Instant::now() + APPLY_RETRY_INTERVAL;
                        None
                    }
                };
            }
            if let Some(clipboard) = writer.as_mut() {
                match apply_remote_text(clipboard.as_mut(), &revision, &text) {
                    Ok(()) => {
                        write_pending.store(false, Ordering::Release);
                        pending_remote = None;
                        last_text = Some(text.clone());
                        pending_events.push_back(ClipboardEvent::RemoteTextApplied(text));
                    }
                    Err(error) => {
                        record_remote_failure(
                            pending_remote
                                .as_mut()
                                .expect("pending remote text disappeared"),
                            &error,
                        );
                        retry_at = Instant::now() + APPLY_RETRY_INTERVAL;
                    }
                }
            }
        }

        if !flush_events(&event_tx, &mut pending_events) {
            break;
        }

        let mut wait = if pending_events.is_empty() {
            RETRY_INTERVAL
        } else {
            COORDINATOR_INTERVAL
        };
        if enabled && pending_remote.is_some() {
            wait = wait.min(retry_at.saturating_duration_since(Instant::now()));
        }

        match request_rx.recv_timeout(wait) {
            Ok(ClipboardRequest::Apply(text)) => {
                if enabled {
                    write_pending.store(true, Ordering::Release);
                    pending_remote = Some(PendingRemote { text, failures: 0 });
                    retry_at = Instant::now();
                    let _ = reader_request_tx.try_send(ReaderRequest::Wake);
                }
            }
            Ok(ClipboardRequest::SetEnabled) => {
                sync_config_state(
                    &enabled_state,
                    &config_epoch,
                    &mut observed_config_epoch,
                    &mut enabled,
                    &mut last_text,
                    &mut pending_remote,
                    &mut pending_events,
                    &reader_request_tx,
                    &write_pending,
                );
            }
            Ok(ClipboardRequest::ReadComplete {
                revision: read_revision,
                config_epoch: read_config_epoch,
                retry,
                result,
            }) => {
                let current_revision = revision.load(Ordering::Acquire);
                if !enabled {
                    continue;
                }
                if !current_revision.is_multiple_of(2)
                    || read_revision != current_revision
                    || read_config_epoch != observed_config_epoch
                {
                    reset_read.store(true, Ordering::Release);
                    let _ = reader_request_tx.try_send(ReaderRequest::Wake);
                    continue;
                }
                match result {
                    Ok(text) if last_text.as_deref() != Some(text.as_str()) => {
                        pending_remote = None;
                        write_pending.store(false, Ordering::Release);
                        last_text = Some(text.clone());
                        pending_events.push_back(ClipboardEvent::LocalText(text));
                    }
                    Ok(_) | Err(ClipboardReadError::ContentNotAvailable) => {}
                    Err(ClipboardReadError::Transient(error)) if !retry => {
                        log::debug!("retrying clipboard read after error: {error}");
                        retry_read.store(true, Ordering::Release);
                        let _ = reader_request_tx.try_send(ReaderRequest::Wake);
                    }
                    Err(ClipboardReadError::Transient(error)) => {
                        log::warn!("clipboard text remains unavailable after retry: {error}");
                    }
                }
            }
            Ok(ClipboardRequest::Availability(available)) => {
                pending_events.push_back(ClipboardEvent::Availability(available));
            }
            Ok(ClipboardRequest::Terminate) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }

    stop.store(true, Ordering::Release);
    write_pending.store(false, Ordering::Release);
    let _ = reader_request_tx.try_send(ReaderRequest::Terminate);
}

#[allow(clippy::too_many_arguments)]
fn sync_config_state(
    enabled_state: &AtomicBool,
    config_epoch: &AtomicU64,
    observed_epoch: &mut u64,
    enabled: &mut bool,
    last_text: &mut Option<String>,
    pending_remote: &mut Option<PendingRemote>,
    pending_events: &mut VecDeque<ClipboardEvent>,
    reader_request_tx: &SyncSender<ReaderRequest>,
    write_pending: &AtomicBool,
) {
    let latest_epoch = config_epoch.load(Ordering::Acquire);
    if latest_epoch == *observed_epoch {
        return;
    }
    *observed_epoch = latest_epoch;
    *enabled = enabled_state.load(Ordering::Acquire);
    *last_text = None;
    *pending_remote = None;
    write_pending.store(false, Ordering::Release);
    pending_events.retain(|event| matches!(event, ClipboardEvent::Availability(_)));
    let _ = reader_request_tx.try_send(ReaderRequest::Wake);
}

fn create_writer(factory: &ClipboardFactory) -> Result<Box<dyn ClipboardBackend>, String> {
    factory()
}

fn record_remote_failure(pending: &mut PendingRemote, error: &impl fmt::Display) {
    pending.failures = pending.failures.saturating_add(1);
    if pending.failures == 1 || pending.failures.is_multiple_of(20) {
        log::warn!(
            "failed to apply remote clipboard text (attempt {}): {error}",
            pending.failures
        );
    } else {
        log::debug!(
            "remote clipboard remains pending (attempt {}): {error}",
            pending.failures
        );
    }
}

fn apply_remote_text(
    clipboard: &mut dyn ClipboardBackend,
    revision: &AtomicU64,
    text: &str,
) -> Result<(), ClipboardWriteError> {
    let stable_revision = revision.load(Ordering::Acquire);
    debug_assert!(stable_revision.is_multiple_of(2));
    revision.store(stable_revision.wrapping_add(1), Ordering::Release);
    let result = clipboard.set_text(text.to_owned());
    // Every attempt invalidates reads that started before it. An Occupied
    // error is commonly caused by that very reader on Windows, so accepting
    // its pre-write result would incorrectly cancel the pending remote value.
    revision.store(stable_revision.wrapping_add(2), Ordering::Release);
    result
}
fn flush_events(
    event_tx: &mpsc::Sender<ClipboardEvent>,
    pending: &mut VecDeque<ClipboardEvent>,
) -> bool {
    while let Some(event) = pending.pop_front() {
        match event_tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(event)) => {
                pending.push_front(event);
                return true;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn run_reader(
    request_rx: Receiver<ReaderRequest>,
    coordinator_tx: SyncSender<ClipboardRequest>,
    stop: Arc<AtomicBool>,
    revision: Arc<AtomicU64>,
    enabled_state: Arc<AtomicBool>,
    config_epoch: Arc<AtomicU64>,
    reset_read: Arc<AtomicBool>,
    retry_read: Arc<AtomicBool>,
    write_pending: Arc<AtomicBool>,
    factory: ClipboardFactory,
    mut change_detector: Box<dyn ChangeDetector>,
    poll_interval: Duration,
) {
    let mut clipboard = create_reader(&factory, &coordinator_tx);
    let mut enabled = enabled_state.load(Ordering::Acquire);
    let mut observed_config_epoch = config_epoch.load(Ordering::Acquire);
    let mut force_read = enabled;
    let mut next_read_is_retry = false;

    while !stop.load(Ordering::Acquire) {
        let latest_config_epoch = config_epoch.load(Ordering::Acquire);
        if latest_config_epoch != observed_config_epoch {
            observed_config_epoch = latest_config_epoch;
            enabled = enabled_state.load(Ordering::Acquire);
            change_detector.reset();
            force_read = enabled;
            next_read_is_retry = false;
        }
        if reset_read.swap(false, Ordering::AcqRel) {
            change_detector.reset();
            force_read = enabled;
            next_read_is_retry = false;
        }
        if retry_read.swap(false, Ordering::AcqRel) {
            change_detector.reset();
            force_read = enabled;
            next_read_is_retry = true;
        }

        if force_read && clipboard.is_some() && !write_pending.load(Ordering::Acquire) {
            poll_clipboard(
                &mut clipboard,
                &coordinator_tx,
                &stop,
                &revision,
                &config_epoch,
                change_detector.as_mut(),
                true,
                next_read_is_retry,
            );
            force_read = false;
            next_read_is_retry = false;
            continue;
        }

        let wait = if clipboard.is_some() {
            poll_interval
        } else {
            RETRY_INTERVAL
        };
        match request_rx.recv_timeout(wait) {
            Ok(ReaderRequest::Wake) => {}
            Ok(ReaderRequest::Terminate) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if clipboard.is_none() {
                    clipboard = create_reader(&factory, &coordinator_tx);
                    change_detector.reset();
                    force_read = clipboard.is_some() && enabled;
                } else if enabled && !force_read && !write_pending.load(Ordering::Acquire) {
                    poll_clipboard(
                        &mut clipboard,
                        &coordinator_tx,
                        &stop,
                        &revision,
                        &config_epoch,
                        change_detector.as_mut(),
                        false,
                        false,
                    );
                }
            }
        }
    }
}
fn create_reader(
    factory: &ClipboardFactory,
    coordinator_tx: &SyncSender<ClipboardRequest>,
) -> Option<Box<dyn ClipboardBackend>> {
    match factory() {
        Ok(clipboard) => {
            let _ = coordinator_tx.send(ClipboardRequest::Availability(true));
            Some(clipboard)
        }
        Err(error) => {
            log::warn!("clipboard reader is unavailable: {error}");
            let _ = coordinator_tx.send(ClipboardRequest::Availability(false));
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_clipboard(
    clipboard: &mut Option<Box<dyn ClipboardBackend>>,
    coordinator_tx: &SyncSender<ClipboardRequest>,
    stop: &AtomicBool,
    revision: &AtomicU64,
    config_epoch: &AtomicU64,
    change_detector: &mut dyn ChangeDetector,
    force: bool,
    retry: bool,
) {
    let Some(clipboard) = clipboard.as_deref_mut() else {
        return;
    };
    let read_revision = revision.load(Ordering::Acquire);
    let read_config_epoch = config_epoch.load(Ordering::Acquire);
    if !read_revision.is_multiple_of(2) || !change_detector.should_read(force) {
        return;
    }

    let result = clipboard.get_text();
    if stop.load(Ordering::Acquire) {
        return;
    }
    let _ = coordinator_tx.send(ClipboardRequest::ReadComplete {
        revision: read_revision,
        config_epoch: read_config_epoch,
        retry,
        result,
    });
}

#[cfg(test)]
mod tests;

use crate::CaptureEvent;
use input_event::EventKind;
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

const REPORT_INTERVAL_SECONDS: u64 = 5;

/// Metrics classification of a [`CaptureEvent`]: either the session-start
/// marker, or one of the shared [`EventKind`] categories.
#[derive(Clone, Copy)]
pub(crate) enum CaptureEventKind {
    Begin,
    Input(EventKind),
}

pub(crate) fn event_kind(event: &CaptureEvent) -> CaptureEventKind {
    match event {
        CaptureEvent::Begin => CaptureEventKind::Begin,
        CaptureEvent::Input(event) => CaptureEventKind::Input(EventKind::of(event)),
    }
}

pub(crate) fn record_enqueued(queue: &'static str, kind: CaptureEventKind, current_depth: usize) {
    ENQUEUED.record(kind);
    update_depth(current_depth);
    report_if_due(queue);
}

#[cfg(windows)]
pub(crate) fn record_full_drop(queue: &'static str, kind: CaptureEventKind, capacity: usize) {
    FULL_DROPS.record(kind);
    update_depth(capacity);
    report_if_due(queue);
}

pub(crate) fn record_dequeued(queue: &'static str, current_depth: usize) {
    update_depth(current_depth);
    report_if_due(queue);
}

static STARTED_AT: OnceLock<Instant> = OnceLock::new();
static LAST_REPORT_MS: AtomicU64 = AtomicU64::new(0);
static ENQUEUED: AtomicEventCounts = AtomicEventCounts::new();
static FULL_DROPS: AtomicEventCounts = AtomicEventCounts::new();
static QUEUE_CURRENT: AtomicUsize = AtomicUsize::new(0);
static QUEUE_MAX: AtomicUsize = AtomicUsize::new(0);

fn update_depth(current_depth: usize) {
    QUEUE_CURRENT.store(current_depth, Ordering::Relaxed);
    QUEUE_MAX.fetch_max(current_depth, Ordering::Relaxed);
}

fn report_if_due(queue: &'static str) {
    let elapsed_ms = STARTED_AT
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    let last_report_ms = LAST_REPORT_MS.load(Ordering::Relaxed);
    if elapsed_ms.saturating_sub(last_report_ms) < REPORT_INTERVAL_SECONDS * 1000 {
        return;
    }
    if LAST_REPORT_MS
        .compare_exchange(
            last_report_ms,
            elapsed_ms,
            Ordering::Relaxed,
            Ordering::Relaxed,
        )
        .is_err()
    {
        return;
    }

    let queue_current = QUEUE_CURRENT.load(Ordering::Relaxed);
    let queue_max = QUEUE_MAX.swap(queue_current, Ordering::Relaxed);
    log::info!(
        target: "crossdesk::metrics",
        "capture_queue={} window_s={:.1} queue_current={} queue_max={} \
         enqueued={} full_drops={}",
        queue,
        elapsed_ms.saturating_sub(last_report_ms) as f64 / 1000.0,
        queue_current,
        queue_max,
        ENQUEUED.take(),
        FULL_DROPS.take(),
    );
}

struct AtomicEventCounts {
    begin: AtomicU64,
    motion: AtomicU64,
    button: AtomicU64,
    scroll: AtomicU64,
    key: AtomicU64,
    modifiers: AtomicU64,
}

impl AtomicEventCounts {
    const fn new() -> Self {
        Self {
            begin: AtomicU64::new(0),
            motion: AtomicU64::new(0),
            button: AtomicU64::new(0),
            scroll: AtomicU64::new(0),
            key: AtomicU64::new(0),
            modifiers: AtomicU64::new(0),
        }
    }

    fn record(&self, kind: CaptureEventKind) {
        let counter = match kind {
            CaptureEventKind::Begin => &self.begin,
            CaptureEventKind::Input(EventKind::Motion) => &self.motion,
            CaptureEventKind::Input(EventKind::Button) => &self.button,
            CaptureEventKind::Input(EventKind::Scroll) => &self.scroll,
            CaptureEventKind::Input(EventKind::Key) => &self.key,
            CaptureEventKind::Input(EventKind::Modifiers) => &self.modifiers,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn take(&self) -> EventCounts {
        EventCounts {
            begin: self.begin.swap(0, Ordering::Relaxed),
            motion: self.motion.swap(0, Ordering::Relaxed),
            button: self.button.swap(0, Ordering::Relaxed),
            scroll: self.scroll.swap(0, Ordering::Relaxed),
            key: self.key.swap(0, Ordering::Relaxed),
            modifiers: self.modifiers.swap(0, Ordering::Relaxed),
        }
    }
}

struct EventCounts {
    begin: u64,
    motion: u64,
    button: u64,
    scroll: u64,
    key: u64,
    modifiers: u64,
}

impl std::fmt::Display for EventCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "begin:{} motion:{} button:{} scroll:{} key:{} modifiers:{}",
            self.begin, self.motion, self.button, self.scroll, self.key, self.modifiers
        )
    }
}

use input_event::Event;
#[cfg(feature = "metrics")]
use input_event::{KeyboardEvent, PointerEvent};
use lan_mouse_proto::ProtoEvent;

#[cfg(feature = "metrics")]
use std::{
    cell::RefCell,
    collections::VecDeque,
    time::{Duration, Instant},
};

#[cfg(feature = "metrics")]
const SAMPLE_CAPACITY: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timestamp {
    #[cfg(feature = "metrics")]
    instant: Instant,
}

impl Timestamp {
    pub(crate) fn now() -> Self {
        Self {
            #[cfg(feature = "metrics")]
            instant: Instant::now(),
        }
    }
}

#[cfg(feature = "metrics")]
#[derive(Clone, Copy, Debug)]
enum EventKind {
    Motion,
    Button,
    Scroll,
    Key,
    Modifiers,
}

#[cfg(feature = "metrics")]
fn event_kind(event: &Event) -> EventKind {
    match event {
        Event::Pointer(PointerEvent::Motion { .. }) => EventKind::Motion,
        Event::Pointer(PointerEvent::Button { .. }) => EventKind::Button,
        Event::Pointer(PointerEvent::Axis { .. })
        | Event::Pointer(PointerEvent::AxisDiscrete120 { .. }) => EventKind::Scroll,
        Event::Keyboard(KeyboardEvent::Key { .. }) => EventKind::Key,
        Event::Keyboard(KeyboardEvent::Modifiers { .. }) => EventKind::Modifiers,
    }
}

pub(crate) fn record_serialization(_started_at: Timestamp) {
    #[cfg(feature = "metrics")]
    with_metrics(|metrics| metrics.serialization.observe(_started_at.instant.elapsed()));
}

pub(crate) fn record_sent(_event: &ProtoEvent) {
    #[cfg(feature = "metrics")]
    if let ProtoEvent::Input(event) = _event {
        with_metrics(|metrics| metrics.sent.record(event_kind(event)));
    }
}

pub(crate) fn record_received(_event: &ProtoEvent) {
    #[cfg(feature = "metrics")]
    if let ProtoEvent::Input(event) = _event {
        with_metrics(|metrics| metrics.received.record(event_kind(event)));
    }
}

#[cfg(feature = "metrics")]
pub(crate) fn record_rtt(elapsed: std::time::Duration) {
    with_metrics(|metrics| metrics.rtt.observe(elapsed));
}

pub(crate) fn record_capture_to_send(_started_at: Timestamp) {
    #[cfg(feature = "metrics")]
    with_metrics(|metrics| {
        metrics
            .capture_to_send
            .observe(_started_at.instant.elapsed())
    });
}

pub(crate) fn record_receive_to_inject(_received_at: Timestamp) {
    #[cfg(feature = "metrics")]
    with_metrics(|metrics| {
        metrics
            .receive_to_inject
            .observe(_received_at.instant.elapsed())
    });
}

pub(crate) fn record_switch_ack(_started_at: Timestamp) {
    #[cfg(feature = "metrics")]
    with_metrics(|metrics| metrics.switch_ack.observe(_started_at.instant.elapsed()));
}

pub(crate) fn injection_queue_push() {
    #[cfg(feature = "metrics")]
    with_metrics(|metrics| {
        metrics.injection_queue_current += 1;
        metrics.injection_queue_max = metrics
            .injection_queue_max
            .max(metrics.injection_queue_current);
    });
}

pub(crate) fn injection_queue_pop() {
    #[cfg(feature = "metrics")]
    with_metrics(|metrics| {
        metrics.injection_queue_current = metrics.injection_queue_current.saturating_sub(1);
    });
}

pub(crate) fn record_emulation_inactive_drop(_event: &Event) {
    #[cfg(feature = "metrics")]
    with_metrics(|metrics| metrics.emulation_inactive_drops.record(event_kind(_event)));
}

pub(crate) fn start_reporter() {
    #[cfg(feature = "metrics")]
    {
        with_metrics(|_| {});
        tokio::task::spawn_local(async {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.tick().await;
            loop {
                interval.tick().await;
                report();
            }
        });
    }
}

#[cfg(feature = "metrics")]
thread_local! {
    static METRICS: RefCell<Metrics> = RefCell::new(Metrics::new());
}

#[cfg(feature = "metrics")]
fn with_metrics(f: impl FnOnce(&mut Metrics)) {
    METRICS.with_borrow_mut(f);
}

#[cfg(feature = "metrics")]
fn report() {
    with_metrics(|metrics| {
        let elapsed = metrics.interval_started.elapsed().as_secs_f64().max(0.001);
        let sent = metrics.sent.total();
        let received = metrics.received.total();
        let dropped = metrics.emulation_inactive_drops.total();

        log::info!(
            target: "crossdesk::metrics",
            "window_s={elapsed:.1} sent_eps={:.1} received_eps={:.1} \
             rtt_us={} serialization_us={} capture_dispatch_to_send_us={} \
             receive_to_inject_us={} switch_ack_us={} injection_queue_current={} \
             injection_queue_max={} motion_merged=0 emulation_inactive_drops={} \
             ordering=unavailable_without_sequence",
            sent as f64 / elapsed,
            received as f64 / elapsed,
            metrics.rtt.summary(),
            metrics.serialization.summary(),
            metrics.capture_to_send.summary(),
            metrics.receive_to_inject.summary(),
            metrics.switch_ack.summary(),
            metrics.injection_queue_current,
            metrics.injection_queue_max,
            dropped,
        );
        log::debug!(
            target: "crossdesk::metrics",
            "sent={} received={} emulation_inactive_drops={}",
            metrics.sent,
            metrics.received,
            metrics.emulation_inactive_drops,
        );

        metrics.interval_started = Instant::now();
        metrics.sent = EventCounts::default();
        metrics.received = EventCounts::default();
        metrics.emulation_inactive_drops = EventCounts::default();
        metrics.injection_queue_max = metrics.injection_queue_current;
    });
}

#[cfg(feature = "metrics")]
struct Metrics {
    interval_started: Instant,
    rtt: SampleWindow,
    serialization: SampleWindow,
    capture_to_send: SampleWindow,
    receive_to_inject: SampleWindow,
    switch_ack: SampleWindow,
    sent: EventCounts,
    received: EventCounts,
    emulation_inactive_drops: EventCounts,
    injection_queue_current: usize,
    injection_queue_max: usize,
}

#[cfg(feature = "metrics")]
impl Metrics {
    fn new() -> Self {
        Self {
            interval_started: Instant::now(),
            rtt: SampleWindow::new(),
            serialization: SampleWindow::new(),
            capture_to_send: SampleWindow::new(),
            receive_to_inject: SampleWindow::new(),
            switch_ack: SampleWindow::new(),
            sent: EventCounts::default(),
            received: EventCounts::default(),
            emulation_inactive_drops: EventCounts::default(),
            injection_queue_current: 0,
            injection_queue_max: 0,
        }
    }
}

#[cfg(feature = "metrics")]
#[derive(Default)]
struct EventCounts {
    motion: u64,
    button: u64,
    scroll: u64,
    key: u64,
    modifiers: u64,
}

#[cfg(feature = "metrics")]
impl EventCounts {
    fn record(&mut self, kind: EventKind) {
        match kind {
            EventKind::Motion => self.motion += 1,
            EventKind::Button => self.button += 1,
            EventKind::Scroll => self.scroll += 1,
            EventKind::Key => self.key += 1,
            EventKind::Modifiers => self.modifiers += 1,
        }
    }

    fn total(&self) -> u64 {
        self.motion + self.button + self.scroll + self.key + self.modifiers
    }
}

#[cfg(feature = "metrics")]
impl std::fmt::Display for EventCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "motion:{} button:{} scroll:{} key:{} modifiers:{}",
            self.motion, self.button, self.scroll, self.key, self.modifiers
        )
    }
}

#[cfg(feature = "metrics")]
struct SampleWindow {
    samples_us: VecDeque<u64>,
}

#[cfg(feature = "metrics")]
impl SampleWindow {
    fn new() -> Self {
        Self {
            samples_us: VecDeque::with_capacity(SAMPLE_CAPACITY),
        }
    }

    fn observe(&mut self, duration: Duration) {
        if self.samples_us.len() == SAMPLE_CAPACITY {
            self.samples_us.pop_front();
        }
        self.samples_us
            .push_back(duration.as_micros().min(u64::MAX as u128) as u64);
    }

    fn percentiles(&self) -> Option<(u64, u64, u64)> {
        if self.samples_us.is_empty() {
            return None;
        }
        let mut sorted = self.samples_us.iter().copied().collect::<Vec<_>>();
        sorted.sort_unstable();
        Some((
            percentile(&sorted, 50),
            percentile(&sorted, 95),
            percentile(&sorted, 99),
        ))
    }

    fn summary(&self) -> String {
        match self.percentiles() {
            Some((p50, p95, p99)) => format!(
                "samples:{} p50:{} p95:{} p99:{}",
                self.samples_us.len(),
                p50,
                p95,
                p99
            ),
            None => "samples:0 p50:n/a p95:n/a p99:n/a".to_owned(),
        }
    }
}

#[cfg(feature = "metrics")]
fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (percentile * sorted.len()).div_ceil(100).saturating_sub(1);
    sorted[rank]
}

#[cfg(all(test, feature = "metrics"))]
mod tests {
    use super::*;

    #[test]
    fn sample_window_is_bounded() {
        let mut window = SampleWindow::new();
        for value in 0..SAMPLE_CAPACITY + 10 {
            window.observe(Duration::from_micros(value as u64));
        }

        assert_eq!(window.samples_us.len(), SAMPLE_CAPACITY);
        assert_eq!(window.samples_us.front(), Some(&10));
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let mut window = SampleWindow::new();
        for value in 1..=100 {
            window.observe(Duration::from_micros(value));
        }

        assert_eq!(window.percentiles(), Some((50, 95, 99)));
    }
}

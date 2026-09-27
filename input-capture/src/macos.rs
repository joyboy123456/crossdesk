use super::{Capture, CaptureError, CaptureEvent, Position, error::MacosCaptureCreationError};
use async_trait::async_trait;
use bitflags::bitflags;
use core_foundation::{
    base::{CFRelease, TCFType, kCFAllocatorDefault},
    number::{CFBooleanRef, kCFBooleanTrue},
    runloop::{CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes},
    string::{CFString, CFStringCreateWithCString, CFStringRef, kCFStringEncodingUTF8},
};
use core_foundation_sys::base::Boolean;
use core_foundation_sys::preferences::{
    CFPreferencesGetAppBooleanValue, kCFPreferencesAnyApplication,
};
use core_graphics::{
    base::{CGError, kCGErrorSuccess},
    display::{CGDisplay, CGPoint},
    event::{
        CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventTapProxy, CGEventType, CallbackResult, EventField,
    },
};
use futures_core::Stream;
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
    scancode,
};
use keycode::{KeyMap, KeyMapping};
use libc::c_void;
use once_cell::unsync::Lazy;
use std::{
    collections::{HashSet, VecDeque},
    ffi::{CString, c_char},
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll, ready},
    thread::{self},
    time::{Duration, Instant},
};
use tokio::sync::{
    Mutex,
    mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender, error::TrySendError},
    oneshot,
};

const CROSSDESK_ENTER_EVENT_TAG: i64 = 0x4352_4f53_5344_534b;

// The event tap sits in front of *every* local mouse and keyboard event of the
// login session. Whatever goes wrong behind it, the callback must neither
// block nor keep dropping events, or the whole machine loses its input. The
// limits below exist so that failures degrade to "CrossDesk stops capturing"
// instead of "the Mac is frozen".

/// capacity of the tap -> service queue. The callback never waits on it: a
/// full queue means the consumer stalled, and the capture is dropped.
const EVENT_CHANNEL_CAPACITY: usize = 256;
/// capacity of the tap -> producer-task queue (Grab, display changes, ...)
const NOTIFY_CHANNEL_CAPACITY: usize = 32;
/// more capture starts than this within [`CAPTURE_STORM_WINDOW`] is a
/// feedback loop (e.g. emulated motion re-crossing a barrier), not a user
const CAPTURE_STORM_LIMIT: usize = 8;
const CAPTURE_STORM_WINDOW: Duration = Duration::from_secs(2);
/// how long no new capture may start after a storm or a forced release
const CAPTURE_COOLDOWN: Duration = Duration::from_secs(3);
/// more `TapDisabledByTimeout` than this within [`TAP_TIMEOUT_WINDOW`] and
/// the tap is left disabled instead of being re-enabled over and over
const TAP_TIMEOUT_LIMIT: usize = 3;
const TAP_TIMEOUT_WINDOW: Duration = Duration::from_secs(30);

/// Occurrences inside a sliding time window.
#[derive(Debug, Default)]
struct SlidingWindow {
    hits: VecDeque<Instant>,
}

impl SlidingWindow {
    /// records a hit at `now`; returns how many hits fall inside `window`
    fn hit(&mut self, now: Instant, window: Duration) -> usize {
        while self
            .hits
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) > window)
        {
            self.hits.pop_front();
        }
        self.hits.push_back(now);
        self.hits.len()
    }

    fn clear(&mut self) {
        self.hits.clear();
    }
}

/// Decides whether a barrier crossing may start a capture.
#[derive(Debug, Default)]
struct CaptureGate {
    blocked_until: Option<Instant>,
    starts: SlidingWindow,
}

impl CaptureGate {
    fn try_start(&mut self, now: Instant) -> bool {
        if self.blocked_until.is_some_and(|until| now < until) {
            return false;
        }
        self.blocked_until = None;
        if self.starts.hit(now, CAPTURE_STORM_WINDOW) > CAPTURE_STORM_LIMIT {
            log::warn!(
                "capture storm: more than {CAPTURE_STORM_LIMIT} captures within \
                 {CAPTURE_STORM_WINDOW:?}, pausing new captures for {CAPTURE_COOLDOWN:?}"
            );
            self.starts.clear();
            self.block(now);
            return false;
        }
        true
    }

    fn block(&mut self, now: Instant) {
        self.blocked_until = Some(now + CAPTURE_COOLDOWN);
    }
}

/// Ctrl+Option+Shift+Command, the default release bind. Also handled inside
/// the tap itself so it releases local input even when the service is hung.
fn is_emergency_release_chord(flags: CGEventFlags) -> bool {
    let all = XMods::ShiftMask | XMods::ControlMask | XMods::Mod1Mask | XMods::Mod4Mask;
    modifier_masks(flags).0.contains(all)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SyntheticEnterAction {
    ProcessNormally,
    PassThrough,
    Drop,
}

/// Decide how the capture tap handles CrossDesk's absolute enter placement.
/// It must reach the window server while idle, but it must never start or feed
/// a capture. If capture is already active, dropping it also prevents a
/// conflicting peer from moving the hidden local cursor.
fn synthetic_enter_action(
    capture_active: bool,
    event_type: CGEventType,
    source_user_data: i64,
) -> SyntheticEnterAction {
    if !matches!(event_type, CGEventType::MouseMoved)
        || source_user_data != CROSSDESK_ENTER_EVENT_TAG
    {
        return SyntheticEnterAction::ProcessNormally;
    }

    if capture_active {
        SyntheticEnterAction::Drop
    } else {
        SyntheticEnterAction::PassThrough
    }
}

#[derive(Debug, Default)]
struct Bounds {
    xmin: f64,
    xmax: f64,
    ymin: f64,
    ymax: f64,
}

/// Recover the wire scroll value from a session-tap line delta. The tap
/// observes deltas *after* macOS applied the natural-scrolling flip to real
/// wheels, so undo it here; the receiving end applies its own preference.
fn cg_line_scroll_to_wire(delta: i32, natural_scrolling: bool) -> i32 {
    if natural_scrolling { delta } else { -delta }
}

/// normalized position of `location` along the barrier edge `pos`, relative
/// to the desktop bounding box (top/left = 0.0)
fn edge_ratio(bounds: &Bounds, pos: Position, location: CGPoint) -> f64 {
    let (coord, min, max) = match pos {
        Position::Left | Position::Right => (location.y, bounds.ymin, bounds.ymax),
        Position::Top | Position::Bottom => (location.x, bounds.xmin, bounds.xmax),
    };
    if max <= min {
        return 0.0;
    }
    ((coord - min) / (max - min)).clamp(0.0, 1.0)
}

/// the point at `ratio` along the barrier edge `pos` of the desktop bounding
/// box, moved 1pt inward from the edge so it cannot immediately re-cross the
/// barrier
fn point_on_edge(bounds: &Bounds, pos: Position, ratio: f64) -> CGPoint {
    let ratio = ratio.clamp(0.0, 1.0);
    let edge_offset = 1.0;
    let along = |min: f64, max: f64| min + (max - min) * ratio;
    let (x, y) = match pos {
        Position::Left => (bounds.xmin + edge_offset, along(bounds.ymin, bounds.ymax)),
        Position::Right => (bounds.xmax - edge_offset, along(bounds.ymin, bounds.ymax)),
        Position::Top => (along(bounds.xmin, bounds.xmax), bounds.ymin + edge_offset),
        Position::Bottom => (along(bounds.xmin, bounds.xmax), bounds.ymax - edge_offset),
    };
    CGPoint { x, y }
}

/// Reads the system natural-scrolling preference
/// (`com.apple.swipescrolldirection` in the Apple Global Domain).
/// The key is absent on a freshly set-up account; natural scrolling
/// defaults to ON in that case.
fn read_natural_scrolling() -> bool {
    let key = CFString::from_static_string("com.apple.swipescrolldirection");
    let mut exists: Boolean = 0;
    let value = unsafe {
        CFPreferencesGetAppBooleanValue(
            key.as_concrete_TypeRef(),
            kCFPreferencesAnyApplication,
            &mut exists,
        )
    };
    if exists != 0 { value != 0 } else { true }
}

const NATURAL_SCROLL_TTL: Duration = Duration::from_secs(1);

/// TTL cache so momentum scrolling doesn't hit CFPreferences per event.
#[derive(Debug)]
struct NaturalScrollCache {
    cached: Option<(Instant, bool)>,
}

impl NaturalScrollCache {
    fn new() -> Self {
        Self { cached: None }
    }

    fn get(&mut self) -> bool {
        match self.cached {
            Some((at, v)) if at.elapsed() < NATURAL_SCROLL_TTL => v,
            _ => {
                let v = read_natural_scrolling();
                self.cached = Some((Instant::now(), v));
                v
            }
        }
    }
}

#[derive(Debug)]
struct InputCaptureState {
    /// active capture positions
    active_clients: Lazy<HashSet<Position>>,
    /// the currently entered capture position, if any
    current_pos: Option<Position>,
    /// position where the cursor was captured
    enter_position: Option<CGPoint>,
    /// bounds of the input capture area
    bounds: Bounds,
    /// current state of modifier keys
    modifier_state: XMods,
    /// cached natural-scrolling preference of this host
    natural_scroll: NaturalScrollCache,
    /// capture handed to the producer via Grab but not applied yet; a forced
    /// release clears it so a still-queued Grab cannot re-arm the capture
    pending_grab: Option<Position>,
    /// rate limit for starting captures
    gate: CaptureGate,
    /// recent `TapDisabledByTimeout` events
    tap_timeouts: SlidingWindow,
}

#[derive(Debug)]
enum ProducerEvent {
    Release {
        edge_ratio: Option<f64>,
        completed: Option<oneshot::Sender<()>>,
    },
    Create(Position),
    Destroy(Position),
    Grab(Position),
    DisplayReconfigured,
}

impl InputCaptureState {
    fn new() -> Result<Self, MacosCaptureCreationError> {
        let mut res = Self {
            active_clients: Lazy::new(HashSet::new),
            current_pos: None,
            enter_position: None,
            bounds: Bounds::default(),
            modifier_state: Default::default(),
            natural_scroll: NaturalScrollCache::new(),
            pending_grab: None,
            gate: CaptureGate::default(),
            tap_timeouts: SlidingWindow::default(),
        };
        res.update_bounds()?;
        Ok(res)
    }

    fn crossed(&mut self, event: &CGEvent) -> Option<Position> {
        let location = event.location();
        let relative_x = event.get_double_value_field(EventField::MOUSE_EVENT_DELTA_X);
        let relative_y = event.get_double_value_field(EventField::MOUSE_EVENT_DELTA_Y);

        for &position in self.active_clients.iter() {
            if (position == Position::Left && (location.x + relative_x) <= self.bounds.xmin)
                || (position == Position::Right && (location.x + relative_x) >= self.bounds.xmax)
                || (position == Position::Top && (location.y + relative_y) <= self.bounds.ymin)
                || (position == Position::Bottom && (location.y + relative_y) >= self.bounds.ymax)
            {
                log::debug!("Crossed barrier into position: {position:?}");
                return Some(position);
            }
        }
        None
    }

    // Get the max bounds of all displays
    fn update_bounds(&mut self) -> Result<(), MacosCaptureCreationError> {
        let active_ids =
            CGDisplay::active_displays().map_err(MacosCaptureCreationError::ActiveDisplays)?;
        active_ids.iter().for_each(|d| {
            let bounds = CGDisplay::new(*d).bounds();
            self.bounds.xmin = self.bounds.xmin.min(bounds.origin.x);
            self.bounds.xmax = self.bounds.xmax.max(bounds.origin.x + bounds.size.width);
            self.bounds.ymin = self.bounds.ymin.min(bounds.origin.y);
            self.bounds.ymax = self.bounds.ymax.max(bounds.origin.y + bounds.size.height);
        });

        log::debug!("Updated displays bounds: {0:?}", self.bounds);
        Ok(())
    }

    /// start the input capture; returns where along the crossed edge the
    /// cursor left the screen, normalized to [0, 1]
    fn start_capture(&mut self, event: &CGEvent, position: Position) -> Result<f64, CaptureError> {
        let mut location = event.location();
        let ratio = edge_ratio(&self.bounds, position, location);
        let edge_offset = 1.0;
        // move cursor location to display bounds
        match position {
            Position::Left => location.x = self.bounds.xmin + edge_offset,
            Position::Right => location.x = self.bounds.xmax - edge_offset,
            Position::Top => location.y = self.bounds.ymin + edge_offset,
            Position::Bottom => location.y = self.bounds.ymax - edge_offset,
        };
        self.enter_position = Some(location);
        self.reset_cursor()?;
        Ok(ratio)
    }

    /// resets the cursor to the position, where the capture started
    fn reset_cursor(&mut self) -> Result<(), CaptureError> {
        let pos = self.enter_position.expect("capture active");
        log::trace!("Resetting cursor position to: {}, {}", pos.x, pos.y);
        CGDisplay::warp_mouse_cursor_position(pos).map_err(CaptureError::WarpCursor)
    }

    fn hide_cursor(&self) -> Result<(), CaptureError> {
        CGDisplay::hide_cursor(&CGDisplay::main()).map_err(CaptureError::CoreGraphics)
    }

    fn show_cursor(&self) -> Result<(), CaptureError> {
        CGDisplay::show_cursor(&CGDisplay::main()).map_err(CaptureError::CoreGraphics)
    }

    /// Hand local input back right now, from the tap thread, without waiting
    /// for the async side - which is exactly the part that may be stuck.
    fn force_local_release(&mut self) {
        self.pending_grab = None;
        if self.current_pos.take().is_some() {
            let _ = CGDisplay::show_cursor(&CGDisplay::main());
        }
    }

    async fn handle_producer_event(
        &mut self,
        producer_event: ProducerEvent,
    ) -> Result<(), CaptureError> {
        log::debug!("handling event: {producer_event:?}");
        match producer_event {
            ProducerEvent::Release {
                edge_ratio,
                completed,
            } => {
                self.pending_grab = None;
                // Clear the capture before anything that can fail: a cursor
                // glitch is cosmetic, a capture left armed eats all input.
                if let Some(pos) = self.current_pos.take() {
                    // place the cursor where the remote cursor crossed back
                    // over the barrier; point_on_edge keeps the warp target
                    // 1pt inside rather than exactly on the barrier
                    if let Some(ratio) = edge_ratio {
                        let target = point_on_edge(&self.bounds, pos, ratio);
                        if let Err(e) = CGDisplay::warp_mouse_cursor_position(target) {
                            log::warn!("failed to place cursor on release: {e}");
                        }
                    }
                    self.show_cursor()
                        .unwrap_or_else(|e| log::warn!("failed to show cursor: {e}"));
                }
                if let Some(completed) = completed {
                    let _ = completed.send(());
                }
            }
            ProducerEvent::Grab(pos) => {
                // ignore a Grab the tap has since revoked (forced release)
                if self.pending_grab.take_if(|p| *p == pos).is_some() && self.current_pos.is_none()
                {
                    self.hide_cursor()?;
                    self.current_pos = Some(pos);
                }
            }
            ProducerEvent::Create(p) => {
                self.active_clients.insert(p);
            }
            ProducerEvent::Destroy(p) => {
                if self.pending_grab == Some(p) {
                    self.pending_grab = None;
                }
                self.active_clients.remove(&p);
                if self.current_pos.take_if(|current| *current == p).is_some() {
                    self.show_cursor()
                        .unwrap_or_else(|e| log::warn!("failed to show cursor: {e}"));
                }
            }
            ProducerEvent::DisplayReconfigured => {
                // The macOS display configuration changed — a monitor
                // was plugged in/out, the resolution changed, the
                // arrangement was rearranged, etc. Re-fetch the
                // active-display bounds so barrier crossings and the
                // cursor-warp on capture-start use the current
                // geometry instead of whatever was true at process
                // start.
                if let Err(e) = self.update_bounds() {
                    log::warn!("failed to refresh display bounds: {e}");
                } else {
                    log::info!("display reconfigured: {:?}", self.bounds);
                }
            }
        };
        Ok(())
    }
}

fn get_events(
    ev_type: &CGEventType,
    ev: &CGEvent,
    result: &mut Vec<CaptureEvent>,
    modifier_state: &mut XMods,
    natural_scroll: &mut NaturalScrollCache,
) -> Result<(), CaptureError> {
    fn map_pointer_event(ev: &CGEvent) -> PointerEvent {
        PointerEvent::Motion {
            time: 0,
            dx: ev.get_double_value_field(EventField::MOUSE_EVENT_DELTA_X),
            dy: ev.get_double_value_field(EventField::MOUSE_EVENT_DELTA_Y),
        }
    }

    fn map_key(ev: &CGEvent) -> Result<u32, CaptureError> {
        let code = ev.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
        match KeyMap::from_key_mapping(KeyMapping::Mac(code as u16)) {
            Ok(k) => Ok(k.evdev as u32),
            Err(()) => Err(CaptureError::KeyMapError(code)),
        }
    }

    match ev_type {
        CGEventType::KeyDown => {
            let k = map_key(ev)?;
            result.push(CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                time: 0,
                key: k,
                state: 1,
            })));
        }
        CGEventType::KeyUp => {
            let k = map_key(ev)?;
            result.push(CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                time: 0,
                key: k,
                state: 0,
            })));
        }
        CGEventType::FlagsChanged => {
            let (depressed, mods_locked) = modifier_masks(ev.get_flags());
            *modifier_state = depressed;

            if let Ok(key) = map_key(ev) {
                result.extend(
                    modifier_key_events(key, depressed)
                        .into_iter()
                        .map(|event| CaptureEvent::Input(Event::Keyboard(event))),
                );
            }

            let modifier_event = KeyboardEvent::Modifiers {
                depressed: depressed.bits(),
                latched: 0,
                locked: mods_locked.bits(),
                group: 0,
            };

            result.push(CaptureEvent::Input(Event::Keyboard(modifier_event)));
        }
        CGEventType::MouseMoved => {
            result.push(CaptureEvent::Input(Event::Pointer(map_pointer_event(ev))))
        }
        CGEventType::LeftMouseDragged => {
            result.push(CaptureEvent::Input(Event::Pointer(map_pointer_event(ev))))
        }
        CGEventType::RightMouseDragged => {
            result.push(CaptureEvent::Input(Event::Pointer(map_pointer_event(ev))))
        }
        CGEventType::OtherMouseDragged => {
            result.push(CaptureEvent::Input(Event::Pointer(map_pointer_event(ev))))
        }
        CGEventType::LeftMouseDown => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_LEFT,
                state: 1,
            })))
        }
        CGEventType::LeftMouseUp => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_LEFT,
                state: 0,
            })))
        }
        CGEventType::RightMouseDown => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_RIGHT,
                state: 1,
            })))
        }
        CGEventType::RightMouseUp => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_RIGHT,
                state: 0,
            })))
        }
        CGEventType::OtherMouseDown => {
            let btn_num = ev.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            let button = match btn_num {
                3 => BTN_BACK,
                4 => BTN_FORWARD,
                _ => BTN_MIDDLE,
            };
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button,
                state: 1,
            })))
        }
        CGEventType::OtherMouseUp => {
            let btn_num = ev.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            let button = match btn_num {
                3 => BTN_BACK,
                4 => BTN_FORWARD,
                _ => BTN_MIDDLE,
            };
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button,
                state: 0,
            })))
        }
        CGEventType::ScrollWheel => {
            // CoreGraphics uses the opposite scroll sign from the protocol's
            // wl_pointer/libei convention (positive = down/right).
            //
            // Continuous (trackpad) deltas are intentionally NOT compensated
            // for natural scrolling: the session tap already observes the
            // content-follows-finger direction the user intends.
            if ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0 {
                let v =
                    ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
                let h =
                    ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
                if v != 0 {
                    result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Axis {
                        time: 0,
                        axis: 0, // Vertical
                        value: -v as f64,
                    })));
                }
                if h != 0 {
                    result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Axis {
                        time: 0,
                        axis: 1, // Horizontal
                        value: -h as f64,
                    })));
                }
            } else {
                // line based scrolling
                const LINES_PER_STEP: i32 = 3;
                const V120_STEPS_PER_LINE: i32 = 120 / LINES_PER_STEP;
                let v = ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
                let h = ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2);
                let natural = natural_scroll.get();
                if v != 0 {
                    result.push(CaptureEvent::Input(Event::Pointer(
                        PointerEvent::AxisDiscrete120 {
                            axis: 0, // Vertical
                            value: V120_STEPS_PER_LINE * cg_line_scroll_to_wire(v as i32, natural),
                        },
                    )));
                }
                if h != 0 {
                    result.push(CaptureEvent::Input(Event::Pointer(
                        PointerEvent::AxisDiscrete120 {
                            axis: 1, // Horizontal
                            value: V120_STEPS_PER_LINE * cg_line_scroll_to_wire(h as i32, natural),
                        },
                    )));
                }
            }
        }
        _ => (),
    }
    Ok(())
}

fn modifier_masks(flags: CGEventFlags) -> (XMods, XMods) {
    let mut depressed = XMods::empty();
    let mut locked = XMods::empty();
    if flags.contains(CGEventFlags::CGEventFlagShift) {
        depressed |= XMods::ShiftMask;
    }
    if flags.contains(CGEventFlags::CGEventFlagControl) {
        depressed |= XMods::ControlMask;
    }
    if flags.contains(CGEventFlags::CGEventFlagAlternate) {
        depressed |= XMods::Mod1Mask;
    }
    if flags.contains(CGEventFlags::CGEventFlagCommand) {
        depressed |= XMods::Mod4Mask;
    }
    if flags.contains(CGEventFlags::CGEventFlagAlphaShift) {
        locked |= XMods::LockMask;
    }
    (depressed, locked)
}

fn modifier_key_events(key: u32, depressed: XMods) -> Vec<KeyboardEvent> {
    if scancode::Linux::try_from(key) == Ok(scancode::Linux::KeyCapsLock) {
        // Quartz exposes Caps Lock's logical lock transition, not its physical
        // press/release pair. Forward a pulse so it can never remain in the
        // cross-device pressed-key set or enter key repeat.
        return vec![
            KeyboardEvent::Key {
                time: 0,
                key,
                state: 1,
            },
            KeyboardEvent::Key {
                time: 0,
                key,
                state: 0,
            },
        ];
    }

    let mask = scancode::Linux::try_from(key)
        .map(|key| match key {
            scancode::Linux::KeyLeftShift | scancode::Linux::KeyRightShift => XMods::ShiftMask,
            scancode::Linux::KeyLeftCtrl | scancode::Linux::KeyRightCtrl => XMods::ControlMask,
            scancode::Linux::KeyLeftAlt | scancode::Linux::KeyRightalt => XMods::Mod1Mask,
            scancode::Linux::KeyLeftMeta | scancode::Linux::KeyRightmeta => XMods::Mod4Mask,
            _ => XMods::empty(),
        })
        .unwrap_or_default();
    let state = u8::from(!mask.is_empty() && depressed.contains(mask));
    vec![KeyboardEvent::Key {
        time: 0,
        key,
        state,
    }]
}

fn create_event_tap<'a>(
    client_state: Arc<Mutex<InputCaptureState>>,
    notify_tx: Sender<ProducerEvent>,
    event_tx: Sender<(Position, CaptureEvent)>,
    fault_tx: UnboundedSender<CaptureError>,
) -> Result<CGEventTap<'a>, MacosCaptureCreationError> {
    // Shared slot for the tap's mach port pointer. Stored as `usize`
    // because raw pointers aren't `Send`, but the integer
    // representation is — and CGEventTapEnable is documented as
    // thread-safe. Set immediately after CGEventTap::new returns;
    // read by the callback to recover from a TapDisabledByTimeout.
    let tap_mach_port: Arc<OnceLock<usize>> = Arc::new(OnceLock::new());
    let tap_mach_port_cb = Arc::clone(&tap_mach_port);

    let cg_events_of_interest: Vec<CGEventType> = vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
        CGEventType::ScrollWheel,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ];

    // Invariant: nothing in this callback may wait on the async side. The
    // window server holds every local input event until the callback returns,
    // so a callback blocked on a full channel (or on a lock whose holder is
    // blocked on one) freezes mouse and keyboard for the whole session. Every
    // send is a `try_send`; when the pipeline cannot keep up, local input is
    // handed back first and the failure is reported afterwards.
    let event_tap_callback = move |_proxy: CGEventTapProxy,
                                   event_type: CGEventType,
                                   cg_ev: &CGEvent| {
        log::trace!("Got event from tap: {event_type:?}");
        let mut state = client_state.blocking_lock();
        let now = Instant::now();
        let mut capture_position = None;
        let mut res_events = vec![];

        if matches!(event_type, CGEventType::TapDisabledByTimeout) {
            // The window server disables the tap when the callback does not
            // answer in time (a stalled pipeline, heavy load, App Nap). Any
            // capture that was active is suspect by now: give input back
            // first, then re-enable as Apple documents - unless the tap keeps
            // timing out, in which case staying disabled is the safe state.
            state.force_local_release();
            state.gate.block(now);
            let timeouts = state.tap_timeouts.hit(now, TAP_TIMEOUT_WINDOW);
            if timeouts > TAP_TIMEOUT_LIMIT {
                log::error!(
                    "CGEventTap timed out {timeouts} times within {TAP_TIMEOUT_WINDOW:?}, \
                     leaving it disabled"
                );
                let _ = fault_tx.send(CaptureError::EventTapDisabled);
                return CallbackResult::Keep;
            }
            if let Some(&port) = tap_mach_port_cb.get() {
                log::warn!("CGEventTap disabled by timeout — local input released, re-enabling");
                unsafe {
                    CGEventTapEnable(port as *mut c_void, true);
                }
            } else {
                log::error!(
                    "CGEventTap disabled by timeout, but mach port not yet stored — cannot re-enable"
                );
                let _ = fault_tx.send(CaptureError::EventTapDisabled);
            }
            return CallbackResult::Keep;
        }

        if matches!(event_type, CGEventType::TapDisabledByUserInput) {
            // Deliberate kill — secure-input mode (e.g. password field), TCC
            // Accessibility revoked mid-session, or the user disabling
            // event-monitoring. We can't recover from this: drop the capture
            // synchronously so no racing callback keeps eating input, and let
            // the service tear the session down.
            log::error!("CGEventTap disabled by user input, releasing capture state");
            state.force_local_release();
            let _ = fault_tx.send(CaptureError::EventTapDisabled);
            return CallbackResult::Keep;
        }

        match synthetic_enter_action(
            state.current_pos.is_some(),
            event_type,
            cg_ev.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
        ) {
            SyntheticEnterAction::PassThrough => {
                log::trace!("passing through CrossDesk synthetic enter event");
                return CallbackResult::Keep;
            }
            SyntheticEnterAction::Drop => {
                log::debug!("dropping CrossDesk synthetic enter event while capture is active");
                cg_ev.set_type(CGEventType::Null);
                return CallbackResult::Drop;
            }
            SyntheticEnterAction::ProcessNormally => {}
        }

        let mut emergency_release = false;

        // Are we in a client?
        if let Some(current_pos) = state.current_pos {
            capture_position = Some(current_pos);
            emergency_release = matches!(event_type, CGEventType::FlagsChanged)
                && is_emergency_release_chord(cg_ev.get_flags());
            // reborrow through the guard so the field borrows can split
            let state = &mut *state;
            get_events(
                &event_type,
                cg_ev,
                &mut res_events,
                &mut state.modifier_state,
                &mut state.natural_scroll,
            )
            .unwrap_or_else(|e| {
                log::error!("Failed to get events: {e}");
            });

            // Keep (hidden) cursor at the edge of the screen
            if matches!(
                event_type,
                CGEventType::MouseMoved
                    | CGEventType::LeftMouseDragged
                    | CGEventType::RightMouseDragged
                    | CGEventType::OtherMouseDragged
            ) {
                state.reset_cursor().unwrap_or_else(|e| log::warn!("{e}"));
            }
        } else if matches!(event_type, CGEventType::MouseMoved) && state.pending_grab.is_none() {
            // Did we cross a barrier?
            if let Some(new_pos) = state.crossed(cg_ev) {
                if state.gate.try_start(now) {
                    match notify_tx.try_send(ProducerEvent::Grab(new_pos)) {
                        Ok(()) => {
                            state.pending_grab = Some(new_pos);
                            capture_position = Some(new_pos);
                            let ratio = match state.start_capture(cg_ev, new_pos) {
                                Ok(ratio) => Some(ratio),
                                Err(e) => {
                                    log::warn!("{e}");
                                    None
                                }
                            };
                            res_events.push(CaptureEvent::Begin { ratio });
                            let (depressed, locked) = modifier_masks(cg_ev.get_flags());
                            state.modifier_state = depressed;
                            res_events.push(CaptureEvent::Input(Event::Keyboard(
                                KeyboardEvent::Modifiers {
                                    depressed: depressed.bits(),
                                    latched: 0,
                                    locked: locked.bits(),
                                    group: 0,
                                },
                            )));
                        }
                        Err(e) => {
                            // the producer task is not keeping up; a capture
                            // started now could not be released in time
                            log::warn!("not starting capture, producer queue unavailable: {e}");
                            state.gate.block(now);
                        }
                    }
                }
            }
        }

        let Some(pos) = capture_position else {
            return CallbackResult::Keep;
        };

        for e in &res_events {
            #[cfg(feature = "metrics")]
            let kind = crate::observability::event_kind(e);
            match event_tx.try_send((pos, *e)) {
                Ok(()) => {
                    #[cfg(feature = "metrics")]
                    crate::observability::record_enqueued(
                        "macos_capture",
                        kind,
                        EVENT_CHANNEL_CAPACITY.saturating_sub(event_tx.capacity()),
                    );
                }
                Err(TrySendError::Full(_)) => {
                    // Nobody is consuming captured input any more. Holding on
                    // would turn this Mac into an input black hole.
                    log::error!(
                        "input capture consumer stalled ({EVENT_CHANNEL_CAPACITY} events queued), \
                         releasing local input"
                    );
                    state.force_local_release();
                    state.gate.block(now);
                    let _ = fault_tx.send(CaptureError::EventTapDisabled);
                    return CallbackResult::Keep;
                }
                Err(TrySendError::Closed(_)) => {
                    // the InputCapture instance is being dropped
                    state.force_local_release();
                    return CallbackResult::Keep;
                }
            }
        }

        if emergency_release {
            // The chord was forwarded above, so a healthy service releases
            // the session normally; this makes it work for a hung one too.
            log::warn!("release chord pressed, releasing local input");
            state.force_local_release();
            return CallbackResult::Keep;
        }

        // Returning Drop should stop the event from being processed
        // but core fundation still returns the event
        cg_ev.set_type(CGEventType::Null);
        CallbackResult::Drop
    };

    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        cg_events_of_interest,
        event_tap_callback,
    )
    .map_err(|_| MacosCaptureCreationError::EventTapCreation)?;

    // Hand the mach port pointer to the callback so it can re-enable
    // the tap on TapDisabledByTimeout. The pointer is valid for the
    // lifetime of `tap` (which lives on the event-tap thread until
    // the run loop exits).
    let port_ptr = tap.mach_port().as_concrete_TypeRef() as usize;
    let _ = tap_mach_port.set(port_ptr);

    let tap_source: CFRunLoopSource = tap
        .mach_port()
        .create_runloop_source(0)
        .expect("Failed creating loop source");

    unsafe {
        CFRunLoop::get_current().add_source(&tap_source, kCFRunLoopCommonModes);
    }

    Ok(tap)
}

fn event_tap_thread(
    client_state: Arc<Mutex<InputCaptureState>>,
    event_tx: Sender<(Position, CaptureEvent)>,
    notify_tx: Sender<ProducerEvent>,
    fault_tx: UnboundedSender<CaptureError>,
    ready: std::sync::mpsc::Sender<Result<CFRunLoop, MacosCaptureCreationError>>,
    exit: oneshot::Sender<()>,
) {
    // Clone now: create_event_tap consumes notify_tx into its closure.
    let display_notify_tx = notify_tx.clone();

    let _tap = match create_event_tap(client_state, notify_tx, event_tx, fault_tx) {
        Err(e) => {
            ready.send(Err(e)).expect("channel closed");
            return;
        }
        Ok(tap) => {
            let run_loop = CFRunLoop::get_current();
            ready.send(Ok(run_loop)).expect("channel closed");
            tap
        }
    };

    // Register a Quartz display-reconfiguration callback so the
    // capture state's bounds get refreshed when the user plugs in a
    // monitor, changes resolution, or rearranges displays. The
    // callback runs on this thread's CFRunLoop. Box-leak the sender
    // so the C side has a stable user_info pointer; reclaim it after
    // the run loop exits.
    let display_user_info = Box::into_raw(Box::new(display_notify_tx)) as *mut c_void;
    unsafe {
        CGDisplayRegisterReconfigurationCallback(
            display_reconfiguration_callback,
            display_user_info,
        );
    }

    log::debug!("running CFRunLoop...");
    CFRunLoop::run_current();
    log::debug!("event tap thread exiting!...");

    unsafe {
        CGDisplayRemoveReconfigurationCallback(display_reconfiguration_callback, display_user_info);
        // Reclaim the leaked sender Box so we don't leak a tokio
        // channel sender on every capture create/destroy cycle.
        drop(Box::from_raw(
            display_user_info as *mut Sender<ProducerEvent>,
        ));
    }

    let _ = exit.send(());
}

/// Quartz display-reconfiguration callback. Fires twice per change:
/// once with `kCGDisplayBeginConfigurationFlag` set (BEFORE the
/// change is applied — the bounds are still stale at this point),
/// then again afterwards with the actual change flags (Add, Remove,
/// Mode, DesktopShapeChanged, etc.). Skip the begin phase; on the
/// real notification, kick the producer task to refresh bounds.
extern "C" fn display_reconfiguration_callback(_display: u32, flags: u32, user_info: *mut c_void) {
    const K_CG_DISPLAY_BEGIN_CONFIGURATION_FLAG: u32 = 1 << 0;
    if flags & K_CG_DISPLAY_BEGIN_CONFIGURATION_FLAG != 0 {
        return;
    }
    if user_info.is_null() {
        return;
    }
    // SAFETY: user_info is a Box::into_raw of Sender<ProducerEvent>
    // owned by `event_tap_thread`. It's valid for the lifetime of
    // that thread; the registration is removed before the box is
    // freed. The callback only fires while the run loop is running
    // on that thread, so we know the box is live here.
    let sender = unsafe { &*(user_info as *const Sender<ProducerEvent>) };
    // This runs on the event tap's run loop: blocking here would stall the
    // tap and with it all local input. A dropped notification only delays
    // the bounds refresh.
    if let Err(e) = sender.try_send(ProducerEvent::DisplayReconfigured) {
        log::warn!("failed to notify display reconfiguration: {e}");
    }
}

pub struct MacOSInputCapture {
    event_rx: Receiver<(Position, CaptureEvent)>,
    /// failures detected on the tap thread; the tap has already given local
    /// input back when one arrives, the service still has to end the session
    fault_rx: UnboundedReceiver<CaptureError>,
    notify_tx: Sender<ProducerEvent>,
    run_loop: CFRunLoop,
}

impl MacOSInputCapture {
    pub async fn new() -> Result<Self, MacosCaptureCreationError> {
        request_macos_capture_permissions()?;

        let state = Arc::new(Mutex::new(InputCaptureState::new()?));
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (notify_tx, mut notify_rx) = mpsc::channel(NOTIFY_CHANNEL_CAPACITY);
        let (fault_tx, fault_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (tap_exit_tx, mut tap_exit_rx) = oneshot::channel();

        unsafe { configure_cursor_settings()? };

        log::info!("Enabling CGEvent tap");
        let event_tap_thread_state = state.clone();
        let event_tap_notify = notify_tx.clone();
        thread::spawn(move || {
            event_tap_thread(
                event_tap_thread_state,
                event_tx,
                event_tap_notify,
                fault_tx,
                ready_tx,
                tap_exit_tx,
            )
        });

        // wait for event tap creation result
        let run_loop = ready_rx.recv().expect("channel closed")?;

        let _tap_task: tokio::task::JoinHandle<()> = tokio::task::spawn_local(async move {
            loop {
                tokio::select! {
                    producer_event = notify_rx.recv() => {
                        let Some(producer_event) = producer_event else {
                            break;
                        };
                        let mut state = state.lock().await;
                        state.handle_producer_event(producer_event).await.unwrap_or_else(|e| {
                            log::error!("Failed to handle producer event: {e}");
                        })
                    }
                    _ = &mut tap_exit_rx => break,
                }
            }
            // show cursor
            let _ = CGDisplay::show_cursor(&CGDisplay::main());
        });

        Ok(Self {
            event_rx,
            fault_rx,
            notify_tx,
            run_loop,
        })
    }
}

fn request_macos_capture_permissions() -> Result<(), MacosCaptureCreationError> {
    // Call both request functions unconditionally so macOS surfaces both
    // TCC prompts on the very first launch. TCC always returns `false` the
    // first time a permission is requested (the grant only becomes visible
    // on the next process launch), so returning early on the first failure
    // would skip the second prompt and force the user through an extra
    // relaunch just to see it.
    let accessibility = request_accessibility_permission();
    let input_monitoring = request_input_monitoring_permission();

    if !accessibility {
        return Err(MacosCaptureCreationError::AccessibilityPermission);
    }
    if !input_monitoring {
        return Err(MacosCaptureCreationError::InputMonitoringPermission);
    }
    Ok(())
}

fn request_accessibility_permission() -> bool {
    // The GUI owns the user-visible prompt at startup (see
    // crossdesk_ui::macos_privacy), so a silent check is enough while it is
    // running. A headless daemon has no such owner: unless it asks, macOS
    // never registers it with TCC and the permission cannot be granted at
    // all. `prompt_allowed` limits this to one dialog per process, so
    // clicking "Reenable" repeatedly does not pop a fresh alert every time.
    if unsafe { AXIsProcessTrusted() } {
        return true;
    }
    if crate::macos_permissions::accessibility_prompt_allowed() {
        crate::macos_permissions::prompt_for_accessibility()
    } else {
        false
    }
}

fn request_input_monitoring_permission() -> bool {
    if unsafe { CGPreflightListenEventAccess() } {
        return true;
    }
    if crate::macos_permissions::event_access_prompt_allowed() {
        unsafe { CGRequestListenEventAccess() }
    } else {
        false
    }
}

impl Drop for MacOSInputCapture {
    fn drop(&mut self) {
        self.run_loop.stop();
    }
}

#[async_trait]
impl Capture for MacOSInputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        log::debug!("creating capture, {pos}");
        self.notify_tx
            .send(ProducerEvent::Create(pos))
            .await
            .map_err(|_| CaptureError::EventTapDisabled)?;
        log::debug!("done !");
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        log::debug!("destroying capture {pos}");
        self.notify_tx
            .send(ProducerEvent::Destroy(pos))
            .await
            .map_err(|_| CaptureError::EventTapDisabled)?;
        log::debug!("done !");
        Ok(())
    }

    async fn release(&mut self, edge_ratio: Option<f64>) -> Result<(), CaptureError> {
        log::debug!("notifying Release");
        let (completed_tx, completed_rx) = oneshot::channel();
        self.notify_tx
            .send(ProducerEvent::Release {
                edge_ratio,
                completed: Some(completed_tx),
            })
            .await
            .map_err(|_| CaptureError::EventTapDisabled)?;
        completed_rx
            .await
            .map_err(|_| CaptureError::EventTapDisabled)
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

impl Stream for MacOSInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // faults first: events queued before one are stale
        if let Poll::Ready(Some(error)) = self.fault_rx.poll_recv(cx) {
            return Poll::Ready(Some(Err(error)));
        }
        match ready!(self.event_rx.poll_recv(cx)) {
            None => Poll::Ready(None),
            Some(e) => {
                #[cfg(feature = "metrics")]
                crate::observability::record_dequeued("macos_capture", self.event_rx.len());
                Poll::Ready(Some(Ok(e)))
            }
        }
    }
}

type CGSConnectionID = u32;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGSSetConnectionProperty(
        cid: CGSConnectionID,
        targetCID: CGSConnectionID,
        key: CFStringRef,
        value: CFBooleanRef,
    ) -> CGError;
    fn _CGSDefaultConnection() -> CGSConnectionID;
}

extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    /// Re-enable an event tap that was disabled by a
    /// `kCGEventTapDisabledByTimeout` event. The Apple-documented
    /// recovery path: see Quartz Event Services Reference. The `tap`
    /// argument is a `CFMachPortRef`; we pass the raw pointer so we
    /// can store it as `usize` for cross-thread sharing.
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);

    /// Register a callback invoked when the display configuration
    /// changes (monitor add/remove, resolution change, mirror,
    /// rearrange, etc). See Quartz Display Services Reference.
    fn CGDisplayRegisterReconfigurationCallback(
        callback: extern "C" fn(u32, u32, *mut c_void),
        user_info: *mut c_void,
    ) -> CGError;
    fn CGDisplayRemoveReconfigurationCallback(
        callback: extern "C" fn(u32, u32, *mut c_void),
        user_info: *mut c_void,
    ) -> CGError;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

unsafe fn configure_cursor_settings() -> Result<(), MacosCaptureCreationError> {
    // This is a private settings that allows the cursor to be hidden while in the background.
    // It is used by Barrier and other apps.
    let key = CString::new("SetsCursorInBackground").unwrap();
    let cf_key = CFStringCreateWithCString(
        kCFAllocatorDefault,
        key.as_ptr() as *const c_char,
        kCFStringEncodingUTF8,
    );
    if CGSSetConnectionProperty(
        _CGSDefaultConnection(),
        _CGSDefaultConnection(),
        cf_key,
        kCFBooleanTrue,
    ) != kCGErrorSuccess
    {
        return Err(MacosCaptureCreationError::CGCursorProperty);
    }
    CFRelease(cf_key as *const c_void);
    Ok(())
}

// From X11/X.h
bitflags! {
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
    struct XMods: u32 {
        const ShiftMask = (1<<0);
        const LockMask = (1<<1);
        const ControlMask = (1<<2);
        const Mod1Mask = (1<<3);
        const Mod2Mask = (1<<4);
        const Mod3Mask = (1<<5);
        const Mod4Mask = (1<<6);
        const Mod5Mask = (1<<7);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Bounds, CAPTURE_COOLDOWN, CAPTURE_STORM_LIMIT, CROSSDESK_ENTER_EVENT_TAG, CaptureGate,
        Position, SlidingWindow, SyntheticEnterAction, XMods, cg_line_scroll_to_wire, edge_ratio,
        is_emergency_release_chord, modifier_key_events, modifier_masks, point_on_edge,
        synthetic_enter_action,
    };
    use core_graphics::event::CGEventFlags;
    use input_event::{KeyboardEvent, scancode};
    use std::time::{Duration, Instant};

    const BOUNDS: Bounds = Bounds {
        xmin: 0.0,
        xmax: 1512.0,
        ymin: 0.0,
        ymax: 982.0,
    };

    #[test]
    fn caps_lock_is_locked_not_depressed() {
        let (depressed, locked) = modifier_masks(CGEventFlags::CGEventFlagAlphaShift);
        assert!(depressed.is_empty());
        assert_eq!(locked, XMods::LockMask);
    }

    #[test]
    fn caps_lock_transition_is_forwarded_as_a_complete_pulse() {
        let key = scancode::Linux::KeyCapsLock as u32;
        assert_eq!(
            modifier_key_events(key, XMods::empty()),
            vec![
                KeyboardEvent::Key {
                    time: 0,
                    key,
                    state: 1,
                },
                KeyboardEvent::Key {
                    time: 0,
                    key,
                    state: 0,
                },
            ]
        );
    }

    #[test]
    fn edge_ratio_spans_the_bounding_box() {
        let top = super::CGPoint { x: 0.0, y: 0.0 };
        let mid = super::CGPoint { x: 756.0, y: 491.0 };
        let bottom = super::CGPoint {
            x: 1512.0,
            y: 982.0,
        };
        assert_eq!(edge_ratio(&BOUNDS, Position::Left, top), 0.0);
        assert_eq!(edge_ratio(&BOUNDS, Position::Right, mid), 0.5);
        assert_eq!(edge_ratio(&BOUNDS, Position::Right, bottom), 1.0);
        assert_eq!(edge_ratio(&BOUNDS, Position::Bottom, mid), 0.5);
    }

    #[test]
    fn point_on_edge_round_trips_and_stays_inside() {
        for pos in [
            Position::Left,
            Position::Right,
            Position::Top,
            Position::Bottom,
        ] {
            for ratio in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let point = point_on_edge(&BOUNDS, pos, ratio);
                assert!(point.x > BOUNDS.xmin - 0.5 && point.x < BOUNDS.xmax + 0.5);
                assert!(point.y > BOUNDS.ymin - 0.5 && point.y < BOUNDS.ymax + 0.5);
                let recovered = edge_ratio(&BOUNDS, pos, point);
                assert!(
                    (recovered - ratio).abs() < 0.01,
                    "{pos:?}: {ratio} -> {recovered}"
                );
            }
        }
    }

    #[test]
    fn out_of_range_ratio_is_clamped() {
        let low = point_on_edge(&BOUNDS, Position::Right, -1.0);
        let zero = point_on_edge(&BOUNDS, Position::Right, 0.0);
        assert_eq!((low.x, low.y), (zero.x, zero.y));
    }

    #[test]
    fn passes_line_scroll_through_when_natural_scrolling_on() {
        assert_eq!(cg_line_scroll_to_wire(1, true), 1);
        assert_eq!(cg_line_scroll_to_wire(-1, true), -1);
        assert_eq!(cg_line_scroll_to_wire(0, true), 0);
    }

    #[test]
    fn inverts_line_scroll_when_natural_scrolling_off() {
        assert_eq!(cg_line_scroll_to_wire(1, false), -1);
        assert_eq!(cg_line_scroll_to_wire(-1, false), 1);
        assert_eq!(cg_line_scroll_to_wire(0, false), 0);
    }

    #[test]
    fn sliding_window_forgets_old_hits() {
        let start = Instant::now();
        let window = Duration::from_secs(2);
        let mut hits = SlidingWindow::default();
        assert_eq!(hits.hit(start, window), 1);
        assert_eq!(hits.hit(start + Duration::from_secs(1), window), 2);
        assert_eq!(hits.hit(start + Duration::from_secs(4), window), 1);
    }

    #[test]
    fn capture_storm_pauses_new_captures() {
        let start = Instant::now();
        let mut gate = CaptureGate::default();
        for i in 0..CAPTURE_STORM_LIMIT {
            assert!(gate.try_start(start + Duration::from_millis(i as u64)));
        }
        assert!(!gate.try_start(start + Duration::from_millis(100)));
        assert!(!gate.try_start(start + CAPTURE_COOLDOWN));
        assert!(gate.try_start(start + CAPTURE_COOLDOWN + Duration::from_millis(200)));
    }

    #[test]
    fn human_paced_crossings_are_never_throttled() {
        let start = Instant::now();
        let mut gate = CaptureGate::default();
        for i in 0..100 {
            assert!(gate.try_start(start + Duration::from_millis(300 * i)));
        }
    }

    #[test]
    fn blocked_gate_reopens_after_cooldown() {
        let start = Instant::now();
        let mut gate = CaptureGate::default();
        gate.block(start);
        assert!(!gate.try_start(start + Duration::from_secs(1)));
        assert!(gate.try_start(start + CAPTURE_COOLDOWN));
    }

    #[test]
    fn emergency_chord_needs_all_four_modifiers() {
        let all = CGEventFlags::CGEventFlagControl
            | CGEventFlags::CGEventFlagShift
            | CGEventFlags::CGEventFlagAlternate
            | CGEventFlags::CGEventFlagCommand;
        assert!(is_emergency_release_chord(all));
        assert!(is_emergency_release_chord(
            all | CGEventFlags::CGEventFlagAlphaShift
        ));
        assert!(!is_emergency_release_chord(
            CGEventFlags::CGEventFlagControl
                | CGEventFlags::CGEventFlagShift
                | CGEventFlags::CGEventFlagCommand
        ));
    }

    #[test]
    fn synthetic_enter_passes_through_without_starting_idle_capture() {
        assert_eq!(
            synthetic_enter_action(
                false,
                super::CGEventType::MouseMoved,
                CROSSDESK_ENTER_EVENT_TAG,
            ),
            SyntheticEnterAction::PassThrough
        );
    }

    #[test]
    fn synthetic_enter_is_dropped_when_capture_is_already_active() {
        assert_eq!(
            synthetic_enter_action(
                true,
                super::CGEventType::MouseMoved,
                CROSSDESK_ENTER_EVENT_TAG,
            ),
            SyntheticEnterAction::Drop
        );
    }

    #[test]
    fn real_motion_and_buttons_keep_the_normal_capture_path() {
        assert_eq!(
            synthetic_enter_action(false, super::CGEventType::MouseMoved, 0),
            SyntheticEnterAction::ProcessNormally
        );
        assert_eq!(
            synthetic_enter_action(
                false,
                super::CGEventType::LeftMouseDown,
                CROSSDESK_ENTER_EVENT_TAG,
            ),
            SyntheticEnterAction::ProcessNormally
        );
    }
}

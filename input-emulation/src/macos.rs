use super::{Emulation, EmulationHandle, Position, error::EmulationError};
use async_trait::async_trait;
use bitflags::bitflags;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::base::Boolean;
use core_foundation_sys::preferences::{
    CFPreferencesGetAppBooleanValue, kCFPreferencesAnyApplication,
};
use core_graphics::base::CGFloat;
use core_graphics::display::{
    CGDirectDisplayID, CGDisplay, CGDisplayBounds, CGGetDisplaysWithRect, CGPoint, CGRect, CGSize,
};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
    scancode,
};
use keycode::{KeyMap, KeyMapping};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::{sync::Notify, task::JoinHandle};

use super::error::MacOSEmulationCreationError;

const DEFAULT_REPEAT_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_REPEAT_INTERVAL: Duration = Duration::from_millis(32);
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

/// Convert the protocol's scroll convention (positive = down/right) to a
/// CoreGraphics scroll delta. CG's base convention is the opposite sign,
/// *but* HID-posted synthetic events bypass the natural-scrolling flip the
/// system applies to real devices, so that flip is applied here as well:
/// with natural scrolling ON the two negations cancel (identity), with it
/// OFF the net effect is a single negation.
fn wire_scroll_to_cgevent(value: i32, natural_scrolling: bool) -> i32 {
    if natural_scrolling {
        value
    } else {
        value.saturating_neg()
    }
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

pub(crate) struct MacOSEmulation {
    /// global event source for all events
    event_source: CGEventSource,
    /// task handle for key repeats
    repeat_task: Option<RepeatTask>,
    /// current state of the mouse buttons (tracked by evdev button code)
    pressed_buttons: HashSet<u32>,
    /// button previously pressed (evdev button code)
    previous_button: Option<u32>,
    /// timestamp of previous click (button down)
    previous_button_click: Option<Instant>,
    /// click state, i.e. number of clicks in quick succession
    button_click_state: i64,
    /// current modifier state
    modifier_state: Rc<Cell<XMods>>,
    /// notify to cancel key repeats
    notify_repeat_task: Arc<Notify>,
    /// cached natural-scrolling preference of this host
    natural_scroll: NaturalScrollCache,
}

struct RepeatTask {
    key: CGKeyCode,
    task: JoinHandle<()>,
}

/// Maps an evdev button code to the CGEventType used for drag events.
fn drag_event_type(button: u32) -> CGEventType {
    match button {
        BTN_LEFT => CGEventType::LeftMouseDragged,
        BTN_RIGHT => CGEventType::RightMouseDragged,
        // middle, back, forward, and any other button all use OtherMouseDragged
        _ => CGEventType::OtherMouseDragged,
    }
}

unsafe impl Send for MacOSEmulation {}

impl MacOSEmulation {
    pub(crate) fn new() -> Result<Self, MacOSEmulationCreationError> {
        request_macos_emulation_permissions()?;

        let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| MacOSEmulationCreationError::EventSourceCreation)?;
        Ok(Self {
            event_source,
            pressed_buttons: HashSet::new(),
            previous_button: None,
            previous_button_click: None,
            button_click_state: 0,
            repeat_task: None,
            notify_repeat_task: Arc::new(Notify::new()),
            modifier_state: Rc::new(Cell::new(XMods::empty())),
            natural_scroll: NaturalScrollCache::new(),
        })
    }

    fn get_mouse_location(&self) -> Option<CGPoint> {
        let event: CGEvent = CGEvent::new(self.event_source.clone()).ok()?;
        Some(event.location())
    }

    async fn spawn_repeat_task(&mut self, key: u16) {
        // there can only be one repeating key and it's
        // always the last to be pressed
        self.cancel_repeat_task().await;
        // initial key event
        key_event(self.event_source.clone(), key, 1, self.modifier_state.get());
        // repeat task
        let event_source = self.event_source.clone();
        let notify = self.notify_repeat_task.clone();
        let modifiers = self.modifier_state.clone();
        let repeat_task = tokio::task::spawn_local(async move {
            let stop = tokio::select! {
                _ = tokio::time::sleep(DEFAULT_REPEAT_DELAY) => false,
                _ = notify.notified() => true,
            };
            if !stop {
                loop {
                    key_event(event_source.clone(), key, 1, modifiers.get());
                    tokio::select! {
                        _ = tokio::time::sleep(DEFAULT_REPEAT_INTERVAL) => {},
                        _ = notify.notified() => break,
                    }
                }
            }
        });
        self.repeat_task = Some(RepeatTask {
            key,
            task: repeat_task,
        });
    }

    async fn cancel_repeat_task(&mut self) {
        if let Some(repeat) = self.repeat_task.take() {
            self.notify_repeat_task.notify_waiters();
            let _ = repeat.task.await;
        }
    }

    async fn release_key(&mut self, key: CGKeyCode) {
        if self
            .repeat_task
            .as_ref()
            .is_some_and(|repeat| repeat.key == key)
        {
            self.cancel_repeat_task().await;
        }
        key_event(self.event_source.clone(), key, 0, self.modifier_state.get());
    }
}

fn request_macos_emulation_permissions() -> Result<(), MacOSEmulationCreationError> {
    // Request both permissions up front so the user sees both TCC prompts
    // on the first launch. See the matching comment in input-capture/src/
    // macos.rs::request_macos_capture_permissions for the rationale.
    let accessibility = request_accessibility_permission();
    let input_control = request_input_control_permission();

    if !accessibility {
        return Err(MacOSEmulationCreationError::AccessibilityPermission);
    }
    if !input_control {
        return Err(MacOSEmulationCreationError::InputControlPermission);
    }
    Ok(())
}

fn request_accessibility_permission() -> bool {
    // The GUI owns the user-visible prompt (see crossdesk_ui::macos_privacy),
    // so a silent check is enough when it is running. A headless daemon has
    // no such owner: without asking, macOS never registers this process with
    // TCC and the permission can never be granted, so prompt there.
    if unsafe { AXIsProcessTrusted() } {
        return true;
    }
    if crate::macos_permissions::accessibility_prompt_allowed() {
        crate::macos_permissions::prompt_for_accessibility()
    } else {
        false
    }
}

fn request_input_control_permission() -> bool {
    if unsafe { CGPreflightPostEventAccess() } {
        return true;
    }
    if crate::macos_permissions::event_access_prompt_allowed() {
        unsafe { CGRequestPostEventAccess() }
    } else {
        false
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Mac virtual key codes for the four arrow keys.
const MAC_KEY_LEFT: u16 = 0x7B;
const MAC_KEY_RIGHT: u16 = 0x7C;
const MAC_KEY_DOWN: u16 = 0x7D;
const MAC_KEY_UP: u16 = 0x7E;

fn is_arrow_key(key: u16) -> bool {
    matches!(
        key,
        MAC_KEY_LEFT | MAC_KEY_RIGHT | MAC_KEY_DOWN | MAC_KEY_UP
    )
}

fn key_event(event_source: CGEventSource, key: u16, state: u8, modifiers: XMods) {
    let event = match CGEvent::new_keyboard_event(event_source, key, state != 0) {
        Ok(e) => e,
        Err(_) => {
            log::warn!("unable to create key event");
            return;
        }
    };
    let mut flags = to_cgevent_flags(modifiers);
    // Hardware-generated arrow keys on macOS carry NumericPad + SecondaryFn.
    // CGEventTap-based hotkey matchers (e.g. tiling window managers) check
    // these flags to recognize navigation keys; without them synthesized
    // arrow chords fall through to the focused app.
    if is_arrow_key(key) {
        flags |= CGEventFlags::CGEventFlagNumericPad | CGEventFlags::CGEventFlagSecondaryFn;
    }
    event.set_flags(flags);
    event.post(CGEventTapLocation::HID);
    log::trace!("key event: {key} {state}");
}

fn modifier_event(event_source: CGEventSource, depressed: XMods) {
    let Ok(event) = CGEvent::new(event_source) else {
        log::warn!("could not create CGEvent");
        return;
    };
    let flags = to_cgevent_flags(depressed);
    event.set_type(CGEventType::FlagsChanged);
    event.set_flags(flags);
    event.post(CGEventTapLocation::HID);
    log::trace!("modifiers updated: {depressed:?}");
}

fn get_display_at_point(x: CGFloat, y: CGFloat) -> Option<CGDirectDisplayID> {
    let mut displays: [CGDirectDisplayID; 16] = [0; 16];
    let mut display_count: u32 = 0;
    let rect = CGRect::new(&CGPoint::new(x, y), &CGSize::new(0.0, 0.0));

    let error = unsafe {
        CGGetDisplaysWithRect(
            rect,
            1,
            displays.as_mut_ptr(),
            &mut display_count as *mut u32,
        )
    };

    if error != 0 {
        log::warn!("error getting displays at point ({x}, {y}): {error}");
        return Option::None;
    }

    if display_count == 0 {
        log::debug!("no displays found at point ({x}, {y})");
        return Option::None;
    }

    displays.first().copied()
}

fn get_display_bounds(display: CGDirectDisplayID) -> (CGFloat, CGFloat, CGFloat, CGFloat) {
    unsafe {
        let bounds = CGDisplayBounds(display);
        let min_x = bounds.origin.x;
        let max_x = bounds.origin.x + bounds.size.width;
        let min_y = bounds.origin.y;
        let max_y = bounds.origin.y + bounds.size.height;
        (min_x as f64, min_y as f64, max_x as f64, max_y as f64)
    }
}

fn clamp_to_screen_space(
    current_x: CGFloat,
    current_y: CGFloat,
    dx: CGFloat,
    dy: CGFloat,
) -> (CGFloat, CGFloat) {
    // Check which display the mouse is currently on
    // Determine what the location of the mouse would be after applying the move
    // Get the display at the new location
    // If the point is not on a display
    //   Clamp the mouse to the current display
    // Else If the point is on a display
    //   Clamp the mouse to the new display
    let current_display = match get_display_at_point(current_x, current_y) {
        Some(display) => display,
        None => {
            log::warn!("could not get current display!");
            return (current_x, current_y);
        }
    };

    let new_x = current_x + dx;
    let new_y = current_y + dy;

    let final_display = get_display_at_point(new_x, new_y).unwrap_or(current_display);
    let (min_x, min_y, max_x, max_y) = get_display_bounds(final_display);

    (
        new_x.clamp(min_x, max_x - 1.),
        new_y.clamp(min_y, max_y - 1.),
    )
}

#[async_trait]
impl Emulation for MacOSEmulation {
    async fn consume(
        &mut self,
        event: Event,
        _handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        log::trace!("{event:?}");
        match event {
            Event::Pointer(pointer_event) => {
                match pointer_event {
                    PointerEvent::Motion { time: _, dx, dy } => {
                        let mut mouse_location = match self.get_mouse_location() {
                            Some(l) => l,
                            None => {
                                log::warn!("could not get mouse location!");
                                return Ok(());
                            }
                        };

                        let (new_mouse_x, new_mouse_y) =
                            clamp_to_screen_space(mouse_location.x, mouse_location.y, dx, dy);

                        mouse_location.x = new_mouse_x;
                        mouse_location.y = new_mouse_y;

                        // If any button is held, emit a drag event for it;
                        // otherwise emit a normal mouse-moved event.
                        let event_type = self
                            .pressed_buttons
                            .iter()
                            .next()
                            .map(|&btn| drag_event_type(btn))
                            .unwrap_or(CGEventType::MouseMoved);
                        let event = match CGEvent::new_mouse_event(
                            self.event_source.clone(),
                            event_type,
                            mouse_location,
                            CGMouseButton::Left,
                        ) {
                            Ok(e) => e,
                            Err(_) => {
                                log::warn!("mouse event creation failed!");
                                return Ok(());
                            }
                        };
                        event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, dx as i64);
                        event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, dy as i64);
                        // Set modifier flags from our tracked state rather than
                        // inheriting the system's CombinedSessionState. If a
                        // modifier key-up was lost over UDP the system can
                        // think a modifier (e.g. Control) is still held, which
                        // would silently turn mouse events into modified
                        // variants (Control+click = right-click on macOS).
                        event.set_flags(to_cgevent_flags(self.modifier_state.get()));
                        event.post(CGEventTapLocation::HID);
                    }
                    PointerEvent::Button {
                        time: _,
                        button,
                        state,
                    } => {
                        // button number for OtherMouse events (3 = back, 4 = forward, etc.)
                        let cg_button_number: Option<i64> = match button {
                            BTN_BACK => Some(3),
                            BTN_FORWARD => Some(4),
                            _ => None,
                        };
                        let (event_type, mouse_button) = match (button, state) {
                            (BTN_LEFT, 1) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
                            (BTN_LEFT, 0) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
                            (BTN_RIGHT, 1) => (CGEventType::RightMouseDown, CGMouseButton::Right),
                            (BTN_RIGHT, 0) => (CGEventType::RightMouseUp, CGMouseButton::Right),
                            (BTN_MIDDLE, 1) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
                            (BTN_MIDDLE, 0) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
                            (BTN_BACK, 1) | (BTN_FORWARD, 1) => {
                                (CGEventType::OtherMouseDown, CGMouseButton::Center)
                            }
                            (BTN_BACK, 0) | (BTN_FORWARD, 0) => {
                                (CGEventType::OtherMouseUp, CGMouseButton::Center)
                            }
                            _ => {
                                log::warn!("invalid button event: {button},{state}");
                                return Ok(());
                            }
                        };
                        // store button state using the evdev button code so
                        // back, forward, and middle are tracked independently
                        if state == 1 {
                            self.pressed_buttons.insert(button);
                        } else {
                            self.pressed_buttons.remove(&button);
                        }

                        // update double-click tracking using the evdev button
                        // code so that back/forward don't alias with middle
                        if state == 1 {
                            if self.previous_button == Some(button)
                                && self
                                    .previous_button_click
                                    .is_some_and(|i| i.elapsed() < DOUBLE_CLICK_INTERVAL)
                            {
                                self.button_click_state += 1;
                            } else {
                                self.button_click_state = 1;
                            }
                            self.previous_button = Some(button);
                            self.previous_button_click = Some(Instant::now());
                        }

                        log::debug!("click_state: {}", self.button_click_state);
                        let location = self.get_mouse_location().unwrap();
                        let event = match CGEvent::new_mouse_event(
                            self.event_source.clone(),
                            event_type,
                            location,
                            mouse_button,
                        ) {
                            Ok(e) => e,
                            Err(()) => {
                                log::warn!("mouse event creation failed!");
                                return Ok(());
                            }
                        };
                        event.set_integer_value_field(
                            EventField::MOUSE_EVENT_CLICK_STATE,
                            self.button_click_state,
                        );
                        // Set the button number for extra buttons (back=3, forward=4)
                        if let Some(btn_num) = cg_button_number {
                            event.set_integer_value_field(
                                EventField::MOUSE_EVENT_BUTTON_NUMBER,
                                btn_num,
                            );
                        }
                        // Set modifier flags from our tracked state rather than
                        // inheriting the system's CombinedSessionState. See the
                        // matching comment in the Motion branch above.
                        event.set_flags(to_cgevent_flags(self.modifier_state.get()));
                        event.post(CGEventTapLocation::HID);
                    }
                    PointerEvent::Axis {
                        time: _,
                        axis,
                        value,
                    } => {
                        let value = wire_scroll_to_cgevent(value as i32, self.natural_scroll.get());
                        let (count, wheel1, wheel2, wheel3) = match axis {
                            0 => (1, value, 0, 0), // 0 = vertical => 1 scroll wheel device (y axis)
                            1 => (2, 0, value, 0), // 1 = horizontal => 2 scroll wheel devices (y, x) -> (0, x)
                            _ => {
                                log::warn!("invalid scroll event: {axis}, {value}");
                                return Ok(());
                            }
                        };
                        let event = match CGEvent::new_scroll_event(
                            self.event_source.clone(),
                            ScrollEventUnit::PIXEL,
                            count,
                            wheel1,
                            wheel2,
                            wheel3,
                        ) {
                            Ok(e) => e,
                            Err(()) => {
                                log::warn!("scroll event creation failed!");
                                return Ok(());
                            }
                        };
                        event.post(CGEventTapLocation::HID);
                    }
                    PointerEvent::AxisDiscrete120 { axis, value } => {
                        const LINES_PER_STEP: i32 = 3;
                        let value = wire_scroll_to_cgevent(value, self.natural_scroll.get());
                        let (count, wheel1, wheel2, wheel3) = match axis {
                            0 => (1, value / (120 / LINES_PER_STEP), 0, 0), // 0 = vertical => 1 scroll wheel device (y axis)
                            1 => (2, 0, value / (120 / LINES_PER_STEP), 0), // 1 = horizontal => 2 scroll wheel devices (y, x) -> (0, x)
                            _ => {
                                log::warn!("invalid scroll event: {axis}, {value}");
                                return Ok(());
                            }
                        };
                        let event = match CGEvent::new_scroll_event(
                            self.event_source.clone(),
                            ScrollEventUnit::LINE,
                            count,
                            wheel1,
                            wheel2,
                            wheel3,
                        ) {
                            Ok(e) => e,
                            Err(()) => {
                                log::warn!("scroll event creation failed!");
                                return Ok(());
                            }
                        };
                        event.post(CGEventTapLocation::HID);
                    }
                }

                // reset button click state in case it's not a button event
                if !matches!(pointer_event, PointerEvent::Button { .. }) {
                    self.button_click_state = 0;
                }
            }
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    let code = match KeyMap::from_key_mapping(KeyMapping::Evdev(key as u16)) {
                        Ok(k) => k.mac as CGKeyCode,
                        Err(_) => {
                            log::warn!("unable to map key event");
                            return Ok(());
                        }
                    };
                    let is_modifier = update_modifiers(&self.modifier_state, key, state);
                    if is_modifier {
                        modifier_event(self.event_source.clone(), self.modifier_state.get());
                    }
                    match state {
                        // pressed
                        1 => self.spawn_repeat_task(code).await,
                        _ => self.release_key(code).await,
                    }
                }
                KeyboardEvent::Modifiers {
                    depressed,
                    latched,
                    locked,
                    group,
                } => {
                    set_modifiers(&self.modifier_state, depressed, latched, locked, group);
                    modifier_event(self.event_source.clone(), self.modifier_state.get());
                }
            },
        }
        // FIXME
        Ok(())
    }

    async fn create(&mut self, _handle: EmulationHandle) {}

    async fn destroy(&mut self, _handle: EmulationHandle) {}

    async fn terminate(&mut self) {
        self.cancel_repeat_task().await;
    }

    async fn enter(&mut self, _handle: EmulationHandle, pos: Position, ratio: f64) {
        let Some(bounds) = desktop_bounds() else {
            log::warn!("could not determine desktop bounds, not placing cursor");
            return;
        };
        let location = point_on_edge(bounds, pos, ratio);

        /* the mapped point may fall on a stretch of the bounding-box edge
         * where no display exists; clamp into the current display then */
        let location = match get_display_at_point(location.x, location.y) {
            Some(_) => location,
            None => {
                let (min_x, min_y, max_x, max_y) = match self
                    .get_mouse_location()
                    .and_then(|l| get_display_at_point(l.x, l.y))
                {
                    Some(display) => get_display_bounds(display),
                    None => return,
                };
                CGPoint::new(
                    location.x.clamp(min_x, max_x - 1.),
                    location.y.clamp(min_y, max_y - 1.),
                )
            }
        };

        /* post an absolute mouse-moved event rather than warping: it takes
         * the same path as regular Motion injection and is not affected by
         * the warp suppression interval */
        let event = match CGEvent::new_mouse_event(
            self.event_source.clone(),
            CGEventType::MouseMoved,
            location,
            CGMouseButton::Left,
        ) {
            Ok(e) => e,
            Err(()) => {
                log::warn!("mouse event creation failed!");
                return;
            }
        };
        event.post(CGEventTapLocation::HID);
    }
}

/// bounding box `(min_x, min_y, max_x, max_y)` of all active displays
fn desktop_bounds() -> Option<(CGFloat, CGFloat, CGFloat, CGFloat)> {
    let displays = CGDisplay::active_displays().ok()?;
    let mut bounds: Option<(CGFloat, CGFloat, CGFloat, CGFloat)> = None;
    for display in displays {
        let (min_x, min_y, max_x, max_y) = get_display_bounds(display);
        bounds = Some(match bounds {
            Some((x0, y0, x1, y1)) => (x0.min(min_x), y0.min(min_y), x1.max(max_x), y1.max(max_y)),
            None => (min_x, min_y, max_x, max_y),
        });
    }
    bounds
}

/// the point at `ratio` along the edge `pos` of the desktop bounding box,
/// moved 1pt inward from the edge
fn point_on_edge(
    bounds: (CGFloat, CGFloat, CGFloat, CGFloat),
    pos: Position,
    ratio: f64,
) -> CGPoint {
    let (min_x, min_y, max_x, max_y) = bounds;
    let ratio = if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let edge_offset = 1.0;
    let along = |min: CGFloat, max: CGFloat| min + (max - min) * ratio;
    let (x, y) = match pos {
        Position::Left => (min_x + edge_offset, along(min_y, max_y)),
        Position::Right => (max_x - edge_offset, along(min_y, max_y)),
        Position::Top => (along(min_x, max_x), min_y + edge_offset),
        Position::Bottom => (along(min_x, max_x), max_y - edge_offset),
    };
    CGPoint::new(x, y)
}

fn update_modifiers(modifiers: &Cell<XMods>, key: u32, state: u8) -> bool {
    if let Ok(key) = scancode::Linux::try_from(key) {
        let mask = match key {
            scancode::Linux::KeyLeftShift | scancode::Linux::KeyRightShift => XMods::ShiftMask,
            scancode::Linux::KeyCapsLock => XMods::LockMask,
            scancode::Linux::KeyLeftCtrl | scancode::Linux::KeyRightCtrl => XMods::ControlMask,
            scancode::Linux::KeyLeftAlt | scancode::Linux::KeyRightalt => XMods::Mod1Mask,
            scancode::Linux::KeyLeftMeta | scancode::Linux::KeyRightmeta => XMods::Mod4Mask,
            _ => XMods::empty(),
        };
        // unchanged
        if mask.is_empty() {
            return false;
        }
        let mut mods = modifiers.get();
        match state {
            1 => mods.insert(mask),
            _ => mods.remove(mask),
        }
        modifiers.set(mods);
        true
    } else {
        false
    }
}

fn set_modifiers(
    active_modifiers: &Cell<XMods>,
    depressed: u32,
    latched: u32,
    locked: u32,
    group: u32,
) {
    let depressed = XMods::from_bits(depressed).unwrap_or_default();
    let _latched = XMods::from_bits(latched).unwrap_or_default();
    let _locked = XMods::from_bits(locked).unwrap_or_default();
    let _group = XMods::from_bits(group).unwrap_or_default();

    // we only care about the depressed modifiers for now
    active_modifiers.replace(depressed);
}

fn to_cgevent_flags(depressed: XMods) -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    if depressed.contains(XMods::ShiftMask) {
        flags |= CGEventFlags::CGEventFlagShift;
    }
    if depressed.contains(XMods::LockMask) {
        flags |= CGEventFlags::CGEventFlagAlphaShift;
    }
    if depressed.contains(XMods::ControlMask) {
        flags |= CGEventFlags::CGEventFlagControl;
    }
    if depressed.contains(XMods::Mod1Mask) {
        flags |= CGEventFlags::CGEventFlagAlternate;
    }
    if depressed.contains(XMods::Mod4Mask) {
        flags |= CGEventFlags::CGEventFlagCommand;
    }
    flags
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
    use super::{Position, point_on_edge, wire_scroll_to_cgevent};

    const BOUNDS: (f64, f64, f64, f64) = (0.0, 0.0, 1512.0, 982.0);

    #[test]
    fn point_on_edge_maps_ratio_along_the_edge() {
        let p = point_on_edge(BOUNDS, Position::Left, 0.0);
        assert_eq!((p.x, p.y), (1.0, 0.0));
        let p = point_on_edge(BOUNDS, Position::Right, 1.0);
        assert_eq!((p.x, p.y), (1511.0, 982.0));
        let p = point_on_edge(BOUNDS, Position::Bottom, 0.5);
        assert_eq!((p.x, p.y), (756.0, 981.0));
    }

    #[test]
    fn abnormal_ratio_falls_back_to_center() {
        let nan = point_on_edge(BOUNDS, Position::Right, f64::NAN);
        let mid = point_on_edge(BOUNDS, Position::Right, 0.5);
        assert_eq!((nan.x, nan.y), (mid.x, mid.y));
    }

    #[test]
    fn inverts_wire_scroll_when_natural_scrolling_off() {
        assert_eq!(wire_scroll_to_cgevent(120, false), -120);
        assert_eq!(wire_scroll_to_cgevent(-120, false), 120);
        assert_eq!(wire_scroll_to_cgevent(0, false), 0);
        assert_eq!(wire_scroll_to_cgevent(i32::MIN, false), i32::MAX);
    }

    #[test]
    fn passes_wire_scroll_through_when_natural_scrolling_on() {
        assert_eq!(wire_scroll_to_cgevent(120, true), 120);
        assert_eq!(wire_scroll_to_cgevent(-120, true), -120);
        assert_eq!(wire_scroll_to_cgevent(0, true), 0);
        assert_eq!(wire_scroll_to_cgevent(i32::MIN, true), i32::MIN);
    }
}

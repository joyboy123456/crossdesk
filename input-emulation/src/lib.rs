use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use input_event::{Event, KeyboardEvent, PointerEvent, scancode};

pub use self::error::{EmulationCreationError, EmulationError, InputEmulationError};

#[cfg(windows)]
mod windows;

#[cfg(x11)]
mod x11;

#[cfg(wlroots)]
mod wlroots;

#[cfg(rdp)]
mod xdg_desktop_portal;

#[cfg(libei)]
mod libei;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
mod macos_permissions;

/// fallback input emulation (logs events)
mod dummy;
mod error;

pub type EmulationHandle = u64;

/// Edge of the local desktop through which a remote cursor enters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

impl Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pos = match self {
            Position::Left => "left",
            Position::Right => "right",
            Position::Top => "top",
            Position::Bottom => "bottom",
        };
        write!(f, "{pos}")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(wlroots)]
    Wlroots,
    #[cfg(libei)]
    Libei,
    #[cfg(rdp)]
    Xdp,
    #[cfg(x11)]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl Backend {
    /// Whether this backend can actually move the local cursor.
    ///
    /// [`Backend::Dummy`] discards everything it is given. Treating it as a
    /// working emulation lets a peer take control of a machine it cannot
    /// actually move, which strands the peer's own cursor: it never reaches
    /// the barrier that would hand control back.
    pub fn can_emulate(&self) -> bool {
        *self != Backend::Dummy
    }
}

impl Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(wlroots)]
            Backend::Wlroots => write!(f, "wlroots"),
            #[cfg(libei)]
            Backend::Libei => write!(f, "libei"),
            #[cfg(rdp)]
            Backend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(x11)]
            Backend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            Backend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            Backend::MacOs => write!(f, "macos"),
            Backend::Dummy => write!(f, "dummy"),
        }
    }
}

pub struct InputEmulation {
    emulation: Box<dyn Emulation>,
    backend: Backend,
    handles: HashSet<EmulationHandle>,
    pressed_keys: HashMap<EmulationHandle, HashSet<u32>>,
    pressed_buttons: HashMap<EmulationHandle, HashSet<u32>>,
}

impl InputEmulation {
    async fn with_backend(backend: Backend) -> Result<InputEmulation, EmulationCreationError> {
        let emulation: Box<dyn Emulation> = match backend {
            #[cfg(wlroots)]
            Backend::Wlroots => Box::new(wlroots::WlrootsEmulation::new()?),
            #[cfg(libei)]
            Backend::Libei => Box::new(libei::LibeiEmulation::new().await?),
            #[cfg(x11)]
            Backend::X11 => Box::new(x11::X11Emulation::new()?),
            #[cfg(rdp)]
            Backend::Xdp => Box::new(xdg_desktop_portal::DesktopPortalEmulation::new().await?),
            #[cfg(windows)]
            Backend::Windows => Box::new(windows::WindowsEmulation::new()?),
            #[cfg(target_os = "macos")]
            Backend::MacOs => Box::new(macos::MacOSEmulation::new()?),
            Backend::Dummy => Box::new(dummy::DummyEmulation::new()),
        };
        Ok(Self {
            emulation,
            backend,
            handles: HashSet::new(),
            pressed_keys: HashMap::new(),
            pressed_buttons: HashMap::new(),
        })
    }

    /// The backend in use. [`Backend::Dummy`] means every event is discarded,
    /// which happens when no real backend could be created (on macOS:
    /// missing accessibility permissions).
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// whether this emulation can actually move the local cursor
    pub fn can_emulate(&self) -> bool {
        self.backend.can_emulate()
    }

    pub async fn new(backend: Option<Backend>) -> Result<InputEmulation, EmulationCreationError> {
        if let Some(backend) = backend {
            let b = Self::with_backend(backend).await;
            if b.is_ok() {
                log::info!("using emulation backend: {backend}");
            }
            return b;
        }

        for backend in [
            #[cfg(wlroots)]
            Backend::Wlroots,
            #[cfg(libei)]
            Backend::Libei,
            #[cfg(rdp)]
            Backend::Xdp,
            #[cfg(x11)]
            Backend::X11,
            #[cfg(windows)]
            Backend::Windows,
            #[cfg(target_os = "macos")]
            Backend::MacOs,
            Backend::Dummy,
        ] {
            match Self::with_backend(backend).await {
                Ok(b) => {
                    log::info!("using emulation backend: {backend}");
                    return Ok(b);
                }
                Err(e) if e.cancelled_by_user() => return Err(e),
                Err(e) => log::warn!("{e}"),
            }
        }

        Err(EmulationCreationError::NoAvailableBackend)
    }

    pub async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        match event {
            Event::Keyboard(KeyboardEvent::Key { key, state, .. }) => {
                if !tracks_pressed_key(key) {
                    return self.emulation.consume(event, handle).await;
                }
                // prevent double pressed / released keys
                if self.update_pressed_keys(handle, key, state) {
                    self.emulation.consume(event, handle).await?;
                }
                Ok(())
            }
            Event::Pointer(PointerEvent::Button { button, state, .. }) => {
                if self.update_pressed_buttons(handle, button, state) {
                    self.emulation.consume(event, handle).await?;
                }
                Ok(())
            }
            Event::Keyboard(KeyboardEvent::Modifiers {
                depressed,
                latched,
                locked,
                ..
            }) => {
                if let Some(keys) = self.pressed_keys.get_mut(&handle) {
                    reconcile_pressed_modifiers(keys, depressed | latched | locked);
                }
                self.emulation.consume(event, handle).await
            }
            _ => self.emulation.consume(event, handle).await,
        }
    }

    /// place the local cursor where the remote cursor entered: at `ratio`
    /// (normalized, top/left = 0.0) along the edge `pos`
    pub async fn enter(&mut self, handle: EmulationHandle, pos: Position, ratio: f64) {
        self.emulation.enter(handle, pos, ratio).await
    }

    pub async fn create(&mut self, handle: EmulationHandle) -> bool {
        if self.handles.insert(handle) {
            self.pressed_keys.insert(handle, HashSet::new());
            self.pressed_buttons.insert(handle, HashSet::new());
            self.emulation.create(handle).await;
            true
        } else {
            false
        }
    }

    pub async fn destroy(&mut self, handle: EmulationHandle) {
        let _ = self.release_buttons(handle).await;
        let _ = self.release_keys(handle).await;
        if self.handles.remove(&handle) {
            self.pressed_keys.remove(&handle);
            self.pressed_buttons.remove(&handle);
            self.emulation.destroy(handle).await
        }
    }

    pub async fn terminate(&mut self) {
        for handle in self.handles.iter().cloned().collect::<Vec<_>>() {
            self.destroy(handle).await
        }
        self.emulation.terminate().await
    }

    pub async fn release_keys(&mut self, handle: EmulationHandle) -> Result<(), EmulationError> {
        if let Some(keys) = self.pressed_keys.get_mut(&handle) {
            let keys = keys.drain().collect::<Vec<_>>();
            for key in keys {
                let event = Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key,
                    state: 0,
                });
                self.emulation.consume(event, handle).await?;
                if let Ok(key) = input_event::scancode::Linux::try_from(key) {
                    log::warn!("releasing stuck key: {key:?}");
                }
            }
        }

        let event = Event::Keyboard(KeyboardEvent::Modifiers {
            depressed: 0,
            latched: 0,
            locked: 0,
            group: 0,
        });
        self.emulation.consume(event, handle).await?;
        Ok(())
    }

    pub async fn release_buttons(&mut self, handle: EmulationHandle) -> Result<(), EmulationError> {
        if let Some(buttons) = self.pressed_buttons.get_mut(&handle) {
            let buttons = buttons.drain().collect::<Vec<_>>();
            for button in buttons {
                let event = Event::Pointer(PointerEvent::Button {
                    time: 0,
                    button,
                    state: 0,
                });
                self.emulation.consume(event, handle).await?;
                log::warn!("releasing stuck mouse button: {button}");
            }
        }
        Ok(())
    }

    pub fn has_pressed_keys(&self, handle: EmulationHandle) -> bool {
        self.pressed_keys
            .get(&handle)
            .is_some_and(|p| !p.is_empty())
    }

    /// update the pressed_keys for the given handle
    /// returns whether the event should be processed
    fn update_pressed_keys(&mut self, handle: EmulationHandle, key: u32, state: u8) -> bool {
        let Some(pressed_keys) = self.pressed_keys.get_mut(&handle) else {
            return false;
        };

        if state == 0 {
            // currently pressed => can release
            pressed_keys.remove(&key)
        } else {
            // currently not pressed => can press
            pressed_keys.insert(key)
        }
    }

    fn update_pressed_buttons(&mut self, handle: EmulationHandle, button: u32, state: u32) -> bool {
        let Some(pressed_buttons) = self.pressed_buttons.get_mut(&handle) else {
            return false;
        };

        if state == 0 {
            pressed_buttons.remove(&button)
        } else {
            pressed_buttons.insert(button)
        }
    }
}

fn tracks_pressed_key(key: u32) -> bool {
    #[cfg(target_os = "macos")]
    if scancode::Linux::try_from(key) == Ok(scancode::Linux::KeyCapsLock) {
        // macOS models Caps Lock as a locking edge, not a held key. Keeping it
        // in the generic pressed-key set makes a lost pulse release suppress
        // every later toggle until the control session is destroyed.
        return false;
    }
    true
}

fn reconcile_pressed_modifiers(pressed_keys: &mut HashSet<u32>, active: u32) {
    pressed_keys.retain(|key| {
        let Ok(key) = scancode::Linux::try_from(*key) else {
            return true;
        };
        let mask = match key {
            scancode::Linux::KeyLeftShift | scancode::Linux::KeyRightShift => 1 << 0,
            scancode::Linux::KeyCapsLock => 1 << 1,
            scancode::Linux::KeyLeftCtrl | scancode::Linux::KeyRightCtrl => 1 << 2,
            scancode::Linux::KeyLeftAlt => 1 << 3,
            scancode::Linux::KeyRightalt => (1 << 3) | (1 << 7),
            scancode::Linux::KeyLeftMeta | scancode::Linux::KeyRightmeta => 1 << 6,
            _ => return true,
        };
        active & mask != 0
    });
}

#[async_trait]
trait Emulation: Send {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError>;
    async fn create(&mut self, handle: EmulationHandle);
    async fn destroy(&mut self, handle: EmulationHandle);
    async fn terminate(&mut self);
    /// place the local cursor where the remote cursor entered: at `ratio`
    /// (normalized, top/left = 0.0) along the edge `pos`. Backends that
    /// cannot position the cursor keep the default no-op.
    async fn enter(&mut self, _handle: EmulationHandle, _pos: Position, _ratio: f64) {}
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    struct RecordingEmulation {
        buttons: Arc<Mutex<Vec<(u32, u32)>>>,
    }

    #[async_trait]
    impl Emulation for RecordingEmulation {
        async fn consume(
            &mut self,
            event: Event,
            _handle: EmulationHandle,
        ) -> Result<(), EmulationError> {
            if let Event::Pointer(PointerEvent::Button { button, state, .. }) = event {
                self.buttons
                    .lock()
                    .expect("button event recorder")
                    .push((button, state));
            }
            Ok(())
        }

        async fn create(&mut self, _handle: EmulationHandle) {}
        async fn destroy(&mut self, _handle: EmulationHandle) {}
        async fn terminate(&mut self) {}
    }

    /// The service decides whether to let a peer take control from this, so a
    /// backend that silently drops events must never claim it can emulate.
    #[test]
    fn dummy_backend_cannot_emulate() {
        assert!(!Backend::Dummy.can_emulate());
        #[cfg(windows)]
        assert!(Backend::Windows.can_emulate());
        #[cfg(target_os = "macos")]
        assert!(Backend::MacOs.can_emulate());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn caps_lock_is_not_retained_as_a_pressed_key() {
        assert!(!tracks_pressed_key(scancode::Linux::KeyCapsLock as u32));
        assert!(tracks_pressed_key(scancode::Linux::KeyA as u32));
    }

    #[test]
    fn modifier_snapshot_clears_stale_pressed_modifier_keys_only() {
        let mut pressed = HashSet::from([
            scancode::Linux::KeyLeftShift as u32,
            scancode::Linux::KeyLeftCtrl as u32,
            scancode::Linux::KeyA as u32,
        ]);
        reconcile_pressed_modifiers(&mut pressed, 0);
        assert_eq!(pressed, HashSet::from([scancode::Linux::KeyA as u32]));
    }

    #[tokio::test]
    async fn destroying_a_handle_releases_its_pressed_mouse_buttons() {
        let buttons = Arc::new(Mutex::new(Vec::new()));
        let mut emulation = InputEmulation {
            emulation: Box::new(RecordingEmulation {
                buttons: buttons.clone(),
            }),
            backend: Backend::Dummy,
            handles: HashSet::new(),
            pressed_keys: HashMap::new(),
            pressed_buttons: HashMap::new(),
        };

        emulation.create(9).await;
        emulation
            .consume(
                Event::Pointer(PointerEvent::Button {
                    time: 0,
                    button: 0x110,
                    state: 1,
                }),
                9,
            )
            .await
            .expect("button down");
        emulation.destroy(9).await;

        assert_eq!(
            *buttons.lock().expect("button event recorder"),
            vec![(0x110, 1), (0x110, 0)]
        );
    }
}

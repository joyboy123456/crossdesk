use std::{
    ffi::{c_uchar, c_void},
    path::{Path, PathBuf},
    process::{self, Command},
    sync::Once,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PermissionState {
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub post_events: bool,
}

impl PermissionState {
    pub fn any_granted(self) -> bool {
        self.accessibility || self.input_monitoring || self.post_events
    }

    pub fn any_granted_from(self, previous: Self) -> bool {
        (!previous.accessibility && self.accessibility)
            || (!previous.input_monitoring && self.input_monitoring)
            || (!previous.post_events && self.post_events)
    }

    pub fn any_revoked_from(self, previous: Self) -> bool {
        (previous.accessibility && !self.accessibility)
            || (previous.input_monitoring && !self.input_monitoring)
            || (previous.post_events && !self.post_events)
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> c_uchar;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> c_uchar;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: *const c_void;
    static kCFTypeDictionaryValueCallBacks: *const c_void;
    static kCFBooleanTrue: *const c_void;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(value: *const c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> c_uchar;
    fn CGPreflightPostEventAccess() -> c_uchar;
    fn CGRequestListenEventAccess() -> c_uchar;
    fn CGRequestPostEventAccess() -> c_uchar;
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: *const c_void,
        user_info: *const c_void,
    ) -> *const c_void;
}

pub fn permission_state() -> PermissionState {
    PermissionState {
        accessibility: unsafe { AXIsProcessTrusted() != 0 },
        input_monitoring: unsafe { CGPreflightListenEventAccess() != 0 },
        post_events: unsafe { CGPreflightPostEventAccess() != 0 },
    }
}

pub fn fire_initial_prompts() {
    static FIRED: Once = Once::new();
    FIRED.call_once(|| {
        if !permission_state().accessibility {
            request_accessibility();
            return;
        }

        unsafe {
            ensure_listed_in_input_monitoring();
            CGRequestPostEventAccess();
        }
    });
}

fn request_accessibility() {
    unsafe {
        let key = kAXTrustedCheckOptionPrompt;
        let value = kCFBooleanTrue;
        let options = CFDictionaryCreate(
            kCFAllocatorDefault,
            &key,
            &value,
            1,
            kCFTypeDictionaryKeyCallBacks,
            kCFTypeDictionaryValueCallBacks,
        );
        if !options.is_null() {
            AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
        }
    }
}

unsafe fn ensure_listed_in_input_monitoring() {
    CGRequestListenEventAccess();
    let callback = input_monitoring_noop_callback as *const c_void;
    let tap = CGEventTapCreate(1, 0, 1, 1 << 10, callback, std::ptr::null());
    if !tap.is_null() {
        CFRelease(tap);
    }
}

extern "C" fn input_monitoring_noop_callback(
    _proxy: *const c_void,
    _event_type: u32,
    event: *const c_void,
    _user_info: *const c_void,
) -> *const c_void {
    event
}

pub fn open_accessibility_settings() {
    open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
}

pub fn open_input_monitoring_settings() {
    open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent");
}

fn open_url(url: &str) {
    if let Err(error) = Command::new("open").arg(url).spawn() {
        log::warn!("failed to open macOS privacy settings: {error}");
    }
}

pub fn relaunch_bundle() {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let (script, target) = match enclosing_app_bundle(&executable) {
        Some(bundle) => (
            "while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; open \"$2\"",
            bundle,
        ),
        None => (
            "while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; exec \"$2\"",
            executable,
        ),
    };

    if let Err(error) = Command::new("sh")
        .args(["-c", script, "crossdesk-relaunch"])
        .arg(process::id().to_string())
        .arg(target)
        .spawn()
    {
        log::warn!("failed to schedule CrossDesk relaunch: {error}");
    }
}

fn enclosing_app_bundle(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }

    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }

    let bundle = contents.parent()?;
    (bundle.extension()? == "app").then(|| bundle.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_only_real_app_bundle_layouts() {
        assert_eq!(
            enclosing_app_bundle(Path::new(
                "/Applications/CrossDesk.app/Contents/MacOS/crossdesk"
            )),
            Some(PathBuf::from("/Applications/CrossDesk.app"))
        );
        assert_eq!(
            enclosing_app_bundle(Path::new("/workspace/target/debug/crossdesk")),
            None
        );
    }
}

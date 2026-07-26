//! Asking macOS for the TCC permissions this backend needs.
//!
//! TCC only registers a process that actually asks: a binary that merely
//! preflights never shows up in System Settings with a usable identity, so
//! adding it there by hand does not grant anything. A headless daemon has no
//! GUI to own that prompt, so it has to ask for itself.

use std::sync::atomic::{AtomicBool, Ordering};

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::number::kCFBooleanTrue;
use core_foundation_sys::string::CFStringRef;

static ACCESSIBILITY_PROMPTED: AtomicBool = AtomicBool::new(false);
static EVENT_ACCESS_PROMPTED: AtomicBool = AtomicBool::new(false);

/// Whether the accessibility prompt may be shown now. Tracked separately
/// from the event-access prompt so both are asked for on the first run, but
/// at most once each per process - repeatedly re-enabling emulation must not
/// spam dialogs.
pub(crate) fn accessibility_prompt_allowed() -> bool {
    !ACCESSIBILITY_PROMPTED.swap(true, Ordering::SeqCst)
}

/// Whether the input monitoring / post event prompt may be shown now.
pub(crate) fn event_access_prompt_allowed() -> bool {
    !EVENT_ACCESS_PROMPTED.swap(true, Ordering::SeqCst)
}

/// Ask for accessibility access, showing the system dialog. Returns whether
/// access is already granted - a fresh grant only takes effect on the next
/// launch, so `false` here is the normal first-run answer.
pub(crate) fn prompt_for_accessibility() -> bool {
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let value = CFBoolean::wrap_under_get_rule(kCFBooleanTrue);
        let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef())
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

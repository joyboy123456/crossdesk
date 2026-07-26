//! Decides what happens to clipboard text, without touching the clipboard.
//!
//! Text arrives from two directions - the local clipboard poller, and peers
//! over the network - and both feed back into each other. Applying a peer's
//! text sets the local clipboard, whose next poll reports that same text as a
//! local change, which would be sent straight back. The cached last-seen text
//! is what stops that loop, so the decision lives here as plain logic that can
//! be tested rather than being spread across the service's event handlers.

use lan_mouse_proto::MAX_CLIPBOARD_TEXT_SIZE;

/// What the service should do with a clipboard text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClipboardAction {
    /// nothing to do
    Ignore,
    /// remember the text; `broadcast` tells whether peers should be told
    Cache {
        text: Option<String>,
        broadcast: bool,
    },
}

#[derive(Default)]
pub(crate) struct ClipboardState {
    enabled: bool,
    available: bool,
    /// the text last seen, in either direction
    text: Option<String>,
}

impl ClipboardState {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            available: false,
            text: None,
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn available(&self) -> bool {
        self.available
    }

    pub(crate) fn set_available(&mut self, available: bool) {
        self.available = available;
    }

    /// The local clipboard changed.
    pub(crate) fn on_local_text(&mut self, text: String) -> ClipboardAction {
        if !self.enabled {
            return ClipboardAction::Ignore;
        }
        if text.len() > MAX_CLIPBOARD_TEXT_SIZE {
            log::warn!(
                "clipboard text was not synchronized: {} bytes exceeds the {} byte limit",
                text.len(),
                MAX_CLIPBOARD_TEXT_SIZE,
            );
            // Forget the cached text: it no longer describes the clipboard, and
            // keeping it would suppress a later, smaller copy of the same text.
            return self.cache(None, false);
        }
        self.cache(Some(text), true)
    }

    /// A peer sent us clipboard text.
    ///
    /// `false` means it must not be applied - either synchronization is off, or
    /// this is our own text coming back.
    pub(crate) fn accepts_remote_text(&self, text: &str) -> bool {
        self.enabled && self.text.as_deref() != Some(text)
    }

    /// Remote text was applied to the local clipboard successfully.
    ///
    /// Not broadcast: the peers already have it, and echoing would loop.
    pub(crate) fn on_remote_text_applied(&mut self, text: String) -> ClipboardAction {
        self.cache(Some(text), false)
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) -> ClipboardAction {
        self.enabled = enabled;
        if enabled {
            ClipboardAction::Ignore
        } else {
            // Drop the cache so re-enabling synchronization picks the
            // clipboard up again instead of treating it as already sent.
            self.cache(None, false)
        }
    }

    fn cache(&mut self, text: Option<String>, broadcast: bool) -> ClipboardAction {
        self.text.clone_from(&text);
        ClipboardAction::Cache { text, broadcast }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(text: &str) -> ClipboardAction {
        ClipboardAction::Cache {
            text: Some(text.to_owned()),
            broadcast: true,
        }
    }

    #[test]
    fn local_text_is_cached_and_broadcast() {
        let mut state = ClipboardState::new(true);
        assert_eq!(state.on_local_text("hello".into()), cached("hello"));
    }

    #[test]
    fn nothing_happens_while_synchronization_is_off() {
        let mut state = ClipboardState::new(false);
        assert_eq!(state.on_local_text("hello".into()), ClipboardAction::Ignore);
        assert!(!state.accepts_remote_text("hello"));
    }

    #[test]
    fn oversized_text_is_dropped_rather_than_truncated() {
        let mut state = ClipboardState::new(true);
        let huge = "x".repeat(MAX_CLIPBOARD_TEXT_SIZE + 1);

        assert_eq!(
            state.on_local_text(huge),
            ClipboardAction::Cache {
                text: None,
                broadcast: false
            }
        );
        // a later copy of ordinary text still goes out
        assert_eq!(state.on_local_text("hello".into()), cached("hello"));
    }

    /// The loop this guards against: peer text is applied locally, the poller
    /// reports it as a local change, and without the cache it would be sent
    /// straight back to the peer that just sent it.
    #[test]
    fn applied_remote_text_is_not_echoed_back() {
        let mut state = ClipboardState::new(true);

        assert!(state.accepts_remote_text("from peer"));
        assert_eq!(
            state.on_remote_text_applied("from peer".into()),
            ClipboardAction::Cache {
                text: Some("from peer".into()),
                broadcast: false
            }
        );

        assert!(
            !state.accepts_remote_text("from peer"),
            "the same text arriving again must be ignored"
        );
        assert_eq!(
            state.on_local_text("from peer".into()),
            ClipboardAction::Cache {
                text: Some("from peer".into()),
                broadcast: true
            },
            "the poller reporting it is cached again; peers already have it"
        );
    }

    #[test]
    fn different_remote_text_is_still_accepted() {
        let mut state = ClipboardState::new(true);
        state.on_remote_text_applied("first".into());

        assert!(state.accepts_remote_text("second"));
    }

    #[test]
    fn disabling_clears_the_cache_so_re_enabling_resynchronizes() {
        let mut state = ClipboardState::new(true);
        state.on_local_text("hello".into());

        assert_eq!(
            state.set_enabled(false),
            ClipboardAction::Cache {
                text: None,
                broadcast: false
            }
        );
        assert_eq!(state.set_enabled(true), ClipboardAction::Ignore);
        assert!(
            state.accepts_remote_text("hello"),
            "the same text must be accepted again after a round trip through \
             disabled"
        );
    }

    #[test]
    fn availability_is_tracked_independently_of_the_setting() {
        let mut state = ClipboardState::new(true);
        assert!(!state.available(), "unknown until the worker reports in");

        state.set_available(true);
        assert!(state.available());
        assert!(state.enabled());

        state.set_enabled(false);
        assert!(state.available(), "the platform did not change");
        assert!(!state.enabled());
    }
}

//! The fingerprints of peers allowed to connect.

use std::{
    collections::HashMap,
    sync::{Arc, PoisonError, RwLock},
};

/// SHA-256 certificate fingerprints authorized for incoming connections,
/// mapped to the description the user gave them.
///
/// Shared with the DTLS listener's certificate verification callback, which
/// runs on a different thread - hence the lock. This type is the only place
/// that takes it, so the locking discipline lives in one file.
#[derive(Clone, Default)]
pub(crate) struct AuthorizedKeys {
    keys: Arc<RwLock<HashMap<String, String>>>,
}

impl AuthorizedKeys {
    pub(crate) fn new(keys: HashMap<String, String>) -> Self {
        Self {
            keys: Arc::new(RwLock::new(keys)),
        }
    }

    /// The shared map, for the DTLS listener's verification callback.
    pub(crate) fn shared(&self) -> Arc<RwLock<HashMap<String, String>>> {
        self.keys.clone()
    }

    pub(crate) fn snapshot(&self) -> HashMap<String, String> {
        self.read().clone()
    }

    pub(crate) fn authorize(&self, fingerprint: String, description: String) {
        self.write().insert(fingerprint, description);
    }

    pub(crate) fn revoke(&self, fingerprint: &str) {
        self.write().remove(fingerprint);
    }

    pub(crate) fn replace(&self, keys: &HashMap<String, String>) {
        self.write().clone_from(keys);
    }

    /// A poisoned lock means some other thread panicked while holding it. The
    /// map is a plain collection with no invariant to break, so carrying on
    /// with it beats taking the service down.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, String>> {
        self.keys.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, String>> {
        self.keys.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_and_revoke_round_trip() {
        let keys = AuthorizedKeys::default();
        keys.authorize("aa:bb".into(), "laptop".into());

        assert_eq!(
            keys.snapshot().get("aa:bb").map(String::as_str),
            Some("laptop")
        );

        keys.revoke("aa:bb");
        assert!(keys.snapshot().is_empty());
        // revoking twice must not panic: the frontend can replay a request
        keys.revoke("aa:bb");
    }

    #[test]
    fn the_listener_sees_later_changes() {
        let keys = AuthorizedKeys::default();
        let shared = keys.shared();
        keys.authorize("aa:bb".into(), "laptop".into());

        assert!(
            shared
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key("aa:bb"),
            "a key authorized after the listener started must let it connect"
        );
    }

    #[test]
    fn replace_swaps_the_whole_set() {
        let keys = AuthorizedKeys::new(HashMap::from([("aa:bb".into(), "old".into())]));

        keys.replace(&HashMap::from([("cc:dd".into(), "new".into())]));

        let snapshot = keys.snapshot();
        assert!(
            !snapshot.contains_key("aa:bb"),
            "a key removed from the config file must stop working"
        );
        assert!(snapshot.contains_key("cc:dd"));
    }
}

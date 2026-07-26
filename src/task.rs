//! Building blocks shared by the crate's background tasks.

use local_channel::mpsc::Sender;

/// Sends `on_new` on construction and `on_drop` when dropped.
///
/// Used by the capture and emulation tasks to bracket a backend session with
/// matching "enabled" / "disabled" notifications, so the disabled event is
/// emitted on every exit path (including an error returning out of the
/// session loop).
pub(crate) struct DropGuard<T> {
    tx: Sender<T>,
    on_drop: Option<T>,
}

impl<T> DropGuard<T> {
    pub(crate) fn new(tx: Sender<T>, on_new: T, on_drop: T) -> Self {
        if tx.send(on_new).is_err() {
            log::warn!("dropping enter notification: receiver is gone");
        }
        Self {
            tx,
            on_drop: Some(on_drop),
        }
    }
}

impl<T> Drop for DropGuard<T> {
    fn drop(&mut self) {
        // Never panic here: a panic while unwinding aborts the process, and
        // the receiver being gone simply means nobody is listening anymore.
        if let Some(on_drop) = self.on_drop.take() {
            if self.tx.send(on_drop).is_err() {
                log::warn!("dropping exit notification: receiver is gone");
            }
        }
    }
}

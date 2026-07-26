//! Building blocks shared by the crate's background tasks.
//!
//! Every subsystem follows the same shape: a handle held by
//! [`crate::service::Service`], a task spawned onto the `LocalSet`, and two
//! channels between them (requests in, events out). This module owns the
//! pieces that shape has in common, so the subsystems agree on one channel
//! type, one way to stop a task and one way to treat a closed channel.

use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The channel type used between handles and their tasks.
///
/// Unbounded, because the input path must never block the capture backend's
/// callback, and single-threaded, because everything runs on one `LocalSet`.
pub(crate) use tokio::sync::mpsc::{
    UnboundedReceiver as Receiver, UnboundedSender as Sender, unbounded_channel as channel,
};

/// How long a task may take to wind down after being cancelled.
///
/// Applies per task. A task that owns further tasks winds those down as part
/// of its own shutdown, so its children spend their parent's budget; if that
/// runs out the parent is aborted, which is the correct outcome for something
/// that has already been unresponsive for this long.
const TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Hand `message` to a task, or log if that task is already gone.
///
/// A closed channel means the receiving task exited - during shutdown, or
/// because it failed. Neither is worth a panic: with `panic = "abort"` that
/// would take down the whole service, and the service already learns about a
/// dead subsystem when its event channel ends.
pub(crate) fn send<T>(tx: &Sender<T>, what: &str, message: T) {
    if tx.send(message).is_err() {
        log::debug!("dropping {what}: the receiving task has exited");
    }
}

/// A spawned task and the token that stops it.
pub(crate) struct TaskHandle {
    token: CancellationToken,
    join: Option<JoinHandle<()>>,
}

impl TaskHandle {
    pub(crate) fn new(token: CancellationToken, join: JoinHandle<()>) -> Self {
        Self {
            token,
            join: Some(join),
        }
    }

    /// Cancel the task and wait for it to finish.
    ///
    /// The wait is bounded: a task that does not observe its token is aborted
    /// instead of holding up shutdown indefinitely.
    pub(crate) async fn terminate(&mut self, what: &str) {
        self.token.cancel();
        let Some(mut join) = self.join.take() else {
            return;
        };
        match tokio::time::timeout(TERMINATION_TIMEOUT, &mut join).await {
            Ok(Ok(())) => log::debug!("{what} stopped"),
            Ok(Err(e)) => log::warn!("{what} did not stop cleanly: {e}"),
            Err(_) => {
                log::warn!("{what} ignored the stop request; aborting it");
                join.abort();
            }
        }
    }
}

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
        send(&tx, "enter notification", on_new);
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
            send(&self.tx, "exit notification", on_drop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    use tokio::task::{LocalSet, spawn_local};

    /// Run `f` the way the service runs: a current-thread runtime with a
    /// `LocalSet`, so `spawn_local` and the timers behave as in production.
    fn run_local<F: std::future::Future<Output = ()> + 'static>(f: F) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build test runtime");
        runtime.block_on(LocalSet::new().run_until(f));
    }

    #[test]
    fn send_to_a_gone_task_is_not_fatal() {
        let (tx, rx) = channel::<u8>();
        drop(rx);
        // must not panic: with panic = "abort" this would kill the process
        send(&tx, "test message", 1);
    }

    #[test]
    fn terminate_waits_for_a_task_that_observes_cancellation() {
        run_local(async {
            let token = CancellationToken::new();
            let stopped = Rc::new(Cell::new(false));
            let task = {
                let token = token.clone();
                let stopped = stopped.clone();
                spawn_local(async move {
                    token.cancelled().await;
                    stopped.set(true);
                })
            };

            TaskHandle::new(token, task).terminate("test task").await;

            assert!(stopped.get(), "the task should have run to completion");
        });
    }

    #[test]
    fn terminate_aborts_a_task_that_ignores_cancellation() {
        run_local(async {
            // A task that never observes its token would otherwise hang
            // shutdown forever.
            let token = CancellationToken::new();
            let task = spawn_local(std::future::pending::<()>());
            let mut handle = TaskHandle::new(token, task);

            tokio::time::pause();
            let terminate = handle.terminate("stuck task");
            tokio::pin!(terminate);
            // not finished before the backstop elapses
            assert!(futures::poll!(terminate.as_mut()).is_pending());
            tokio::time::advance(TERMINATION_TIMEOUT + Duration::from_millis(1)).await;
            terminate.await;
        });
    }

    #[test]
    fn terminate_is_idempotent() {
        run_local(async {
            let token = CancellationToken::new();
            let task = spawn_local(async {});
            let mut handle = TaskHandle::new(token, task);

            handle.terminate("test task").await;
            // the service terminates on both the shutdown path and on drop
            handle.terminate("test task").await;
        });
    }

    #[test]
    fn drop_guard_brackets_a_session() {
        let (tx, mut rx) = channel::<&'static str>();
        {
            let _guard = DropGuard::new(tx, "enabled", "disabled");
            assert_eq!(rx.try_recv().expect("enter event"), "enabled");
        }
        assert_eq!(rx.try_recv().expect("exit event"), "disabled");
    }

    #[test]
    fn drop_guard_does_not_panic_without_a_receiver() {
        let (tx, rx) = channel::<&'static str>();
        let guard = DropGuard::new(tx, "enabled", "disabled");
        drop(rx);
        drop(guard);
    }
}

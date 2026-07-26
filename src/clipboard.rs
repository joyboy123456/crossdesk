use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel},
    thread::{self, JoinHandle},
    time::Duration,
};

use arboard::Clipboard;
use tokio::sync::mpsc;

const POLL_INTERVAL: Duration = Duration::from_millis(350);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const REQUEST_CAPACITY: usize = 8;
const EVENT_CAPACITY: usize = 8;

pub(crate) enum ClipboardEvent {
    LocalText(String),
    Availability(bool),
}

enum ClipboardRequest {
    Apply(String),
    SetEnabled(bool),
    Terminate,
}

pub(crate) struct ClipboardSync {
    request_tx: SyncSender<ClipboardRequest>,
    event_rx: mpsc::Receiver<ClipboardEvent>,
    worker: Option<JoinHandle<()>>,
}

impl ClipboardSync {
    pub(crate) fn new(enabled: bool) -> Self {
        let (request_tx, request_rx) = sync_channel(REQUEST_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
        let worker = thread::Builder::new()
            .name("crossdesk-clipboard".into())
            .spawn(move || run_worker(request_rx, event_tx, enabled))
            .expect("spawn clipboard worker");

        Self {
            request_tx,
            event_rx,
            worker: Some(worker),
        }
    }

    pub(crate) async fn event(&mut self) -> Option<ClipboardEvent> {
        self.event_rx.recv().await
    }

    pub(crate) fn apply(&self, text: String) -> bool {
        match self.request_tx.try_send(ClipboardRequest::Apply(text)) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("failed to queue remote clipboard text: {error}");
                false
            }
        }
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        if let Err(error) = self
            .request_tx
            .try_send(ClipboardRequest::SetEnabled(enabled))
        {
            log::warn!("failed to update clipboard sync state: {error}");
        }
    }

    /// Stop the worker and wait for it to exit.
    ///
    /// `arboard` is a blocking API, so the worker is an OS thread; joining it
    /// happens on the blocking pool to keep the async runtime thread free.
    pub(crate) async fn terminate(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        if self
            .request_tx
            .try_send(ClipboardRequest::Terminate)
            .is_err()
        {
            // Nothing is reading the request queue. Dropping this
            // ClipboardSync closes the channel, which stops the worker; a
            // join here could block forever.
            log::warn!("clipboard worker did not accept the stop request");
            return;
        }
        match tokio::task::spawn_blocking(move || worker.join()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => log::warn!("clipboard worker panicked"),
            Err(e) => log::warn!("failed to join clipboard worker: {e}"),
        }
    }
}

impl Drop for ClipboardSync {
    fn drop(&mut self) {
        // Detached on purpose: dropping the sender makes the worker's
        // recv_timeout return Disconnected and exit on its own, and Drop must
        // not block whichever thread happens to run it.
        let _ = self.request_tx.try_send(ClipboardRequest::Terminate);
    }
}

fn run_worker(
    request_rx: Receiver<ClipboardRequest>,
    event_tx: mpsc::Sender<ClipboardEvent>,
    mut enabled: bool,
) {
    let mut clipboard = create_clipboard(&event_tx);
    let mut last_text = None;
    if enabled {
        poll_clipboard(&mut clipboard, &event_tx, &mut last_text, true);
    }

    loop {
        let wait = if clipboard.is_some() {
            POLL_INTERVAL
        } else {
            RETRY_INTERVAL
        };
        match request_rx.recv_timeout(wait) {
            Ok(ClipboardRequest::Apply(text)) => {
                if !enabled || last_text.as_deref() == Some(text.as_str()) {
                    continue;
                }
                let Some(clipboard) = clipboard.as_mut() else {
                    continue;
                };
                match clipboard.set_text(text.clone()) {
                    Ok(()) => last_text = Some(text),
                    Err(error) => log::warn!("failed to apply remote clipboard text: {error}"),
                }
            }
            Ok(ClipboardRequest::SetEnabled(new_enabled)) => {
                enabled = new_enabled;
                if enabled {
                    poll_clipboard(&mut clipboard, &event_tx, &mut last_text, true);
                }
            }
            Ok(ClipboardRequest::Terminate) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if clipboard.is_none() {
                    clipboard = create_clipboard(&event_tx);
                    if clipboard.is_some() && enabled {
                        poll_clipboard(&mut clipboard, &event_tx, &mut last_text, true);
                    }
                } else if enabled {
                    poll_clipboard(&mut clipboard, &event_tx, &mut last_text, false);
                }
            }
        }
    }
}

fn create_clipboard(event_tx: &mpsc::Sender<ClipboardEvent>) -> Option<Clipboard> {
    match Clipboard::new() {
        Ok(clipboard) => {
            let _ = event_tx.try_send(ClipboardEvent::Availability(true));
            Some(clipboard)
        }
        Err(error) => {
            log::warn!("clipboard is unavailable: {error}");
            let _ = event_tx.try_send(ClipboardEvent::Availability(false));
            None
        }
    }
}

fn poll_clipboard(
    clipboard: &mut Option<Clipboard>,
    event_tx: &mpsc::Sender<ClipboardEvent>,
    last_text: &mut Option<String>,
    force: bool,
) {
    let Some(clipboard) = clipboard.as_mut() else {
        return;
    };
    let Ok(text) = clipboard.get_text() else {
        return;
    };
    if !force && last_text.as_deref() == Some(text.as_str()) {
        return;
    }
    if event_tx
        .try_send(ClipboardEvent::LocalText(text.clone()))
        .is_ok()
    {
        *last_text = Some(text);
    }
}

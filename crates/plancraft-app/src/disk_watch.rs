//! Watching open plans for changes on disk (see `plancraft_core::watch`): a background thread
//! checks the files every second, without the workspace lock, and hands what it found to the
//! window, which reloads, warns or refreshes links on its next frame.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use plancraft_core::watch::{self, Change, Watcher};
use plancraft_core::workspace::SharedWorkspace;

/// How often the files are checked. A change is acted on after two checks (it must settle).
const POLL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Inbox {
    changes: Vec<Change>,
    stopped: bool,
}

/// The changes found by the watcher thread, waiting for the next frame.
#[derive(Default)]
pub struct DiskWatch {
    inbox: Arc<Mutex<Inbox>>,
    /// Polled on the window's thread by tests (no thread is started under test).
    #[cfg(test)]
    local: Watcher,
}

fn lock(inbox: &Mutex<Inbox>) -> MutexGuard<'_, Inbox> {
    inbox.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DiskWatch {
    /// Starts the watcher thread. It stops with [`DiskWatch::stop`] or when the window closes.
    pub fn spawn(&self, ws: &SharedWorkspace, ctx: egui::Context) {
        let inbox: Weak<Mutex<Inbox>> = Arc::downgrade(&self.inbox);
        let ws = Arc::downgrade(ws);
        let spawned = std::thread::Builder::new().name("plancraft-disk-watch".into()).spawn(move || {
            let mut watcher = Watcher::new();
            loop {
                std::thread::sleep(POLL);
                let (Some(inbox), Some(ws)) = (inbox.upgrade(), ws.upgrade()) else { return };
                if lock(&inbox).stopped {
                    return;
                }
                // Copied under the lock; the files are checked and read without it.
                let targets = watch::targets(&ws.lock().unwrap_or_else(PoisonError::into_inner));
                drop(ws);
                let changes = match plancraft_core::guard::catch("watching files", || watcher.poll(&targets)) {
                    Ok(changes) => changes,
                    // Start afresh: a bug while watching must not stop the watching.
                    Err(_) => {
                        watcher = Watcher::new();
                        continue;
                    }
                };
                if !changes.is_empty() {
                    lock(&inbox).changes.extend(changes);
                    ctx.request_repaint();
                }
            }
        });
        if let Err(e) = spawned {
            eprintln!("plancraft: could not start watching files ({e}); changes on disk are noticed when saving");
        }
    }

    pub fn stop(&self) {
        lock(&self.inbox).stopped = true;
    }

    /// What the watcher found since the last call.
    pub fn take(&self) -> Vec<Change> {
        std::mem::take(&mut lock(&self.inbox).changes)
    }

    /// Checks the files now, twice (a change must settle), as the thread would over two seconds.
    #[cfg(test)]
    pub fn poll_now(&mut self, ws: &SharedWorkspace) {
        for _ in 0..2 {
            let targets = watch::targets(&ws.lock().unwrap_or_else(PoisonError::into_inner));
            let changes = self.local.poll(&targets);
            lock(&self.inbox).changes.extend(changes);
        }
    }
}

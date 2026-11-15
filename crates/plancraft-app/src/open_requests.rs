//! Files the operating system asks the running app to open. On macOS, double-clicking a plan in
//! Finder (or dropping it on PlanCraft's Dock icon) sends an "open documents" event to the app,
//! whether it is starting or already running, instead of passing the file on the command line
//! (`mac_open.rs`). The event handler queues the paths here and wakes the app, which opens them
//! at the start of its next frame, the same way as File ▸ Open.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// At most this many files wait at once; a request beyond it is dropped (the system sends
/// requests one at a time as the user acts, so only a runaway sender reaches it).
pub const MAX_PENDING: usize = 256;

/// Paths waiting to be opened, and the window to wake when one arrives.
#[derive(Default)]
pub struct Inbox {
    paths: Mutex<Vec<PathBuf>>,
    wake: Mutex<Option<egui::Context>>,
}

impl Inbox {
    /// Queues files to open and asks the app to draw a frame.
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    pub fn push(&self, paths: impl IntoIterator<Item = PathBuf>) {
        {
            let mut queue = self.paths.lock().unwrap_or_else(PoisonError::into_inner);
            let room = MAX_PENDING.saturating_sub(queue.len());
            queue.extend(paths.into_iter().take(room));
        }
        if let Some(ctx) = self.wake.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            ctx.request_repaint();
        }
    }

    /// The window to wake when files arrive (set once the window exists; earlier requests wait).
    pub fn wake_with(&self, ctx: &egui::Context) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(ctx.clone());
    }

    /// Takes every waiting path, oldest first.
    pub fn take(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.paths.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The app's inbox, which the system's event handler fills.
pub fn global() -> Arc<Inbox> {
    static GLOBAL: OnceLock<Arc<Inbox>> = OnceLock::new();
    GLOBAL.get_or_init(Default::default).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_wait_in_order_and_are_taken_once() {
        let inbox = Inbox::default();
        inbox.push([PathBuf::from("a.plancraft"), PathBuf::from("b.plancraft")]);
        inbox.push([PathBuf::from("c.plancraft")]);
        assert_eq!(inbox.take(), ["a.plancraft", "b.plancraft", "c.plancraft"].map(PathBuf::from));
        assert!(inbox.take().is_empty());
    }

    #[test]
    fn a_flood_of_requests_is_capped() {
        let inbox = Inbox::default();
        inbox.push((0..MAX_PENDING + 50).map(|i| PathBuf::from(format!("{i}.plancraft"))));
        inbox.push([PathBuf::from("late.plancraft")]);
        let taken = inbox.take();
        assert_eq!(taken.len(), MAX_PENDING);
        assert_eq!(taken.last(), Some(&PathBuf::from(format!("{}.plancraft", MAX_PENDING - 1))));
    }
}

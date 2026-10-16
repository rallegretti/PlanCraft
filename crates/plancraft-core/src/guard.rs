//! The last line of defence: a panic inside an edit, a recalculation or a tool call becomes an
//! error, and the caller rolls the project back. It exists for bugs and doesn't replace `Result`.

use std::panic::{AssertUnwindSafe, catch_unwind};

/// A panic that was caught and turned into an error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("internal error in {what}: {message}")]
pub struct Panicked {
    /// What was running, e.g. `"edit 'Insert Task'"`.
    pub what: String,
    pub message: String,
}

/// Runs `f`, converting a panic into [`Panicked`].
///
/// `f` may leave the data it was mutating half-changed when it panics: callers restore a snapshot.
pub fn catch<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, Panicked> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        eprintln!("plancraft: {what} panicked: {message}");
        Panicked { what: what.to_string(), message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panics_become_errors() {
        assert_eq!(catch("ok", || 7), Ok(7));
        let e = catch("boom", || -> i32 { panic!("kaput") }).unwrap_err();
        assert_eq!(e.what, "boom");
        assert_eq!(e.message, "kaput");
        let v: Vec<u8> = Vec::new();
        #[allow(clippy::indexing_slicing)]
        let e = catch("index", || v[3]).unwrap_err();
        assert!(e.message.contains("index out of bounds"), "{}", e.message);
    }
}

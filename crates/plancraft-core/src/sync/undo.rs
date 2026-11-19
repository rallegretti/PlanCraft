//! Undo in a shared plan: taking back one's own change without taking back other people's
//! changes made since (docs/COLLABORATION.md › Undo).

use super::patch::{apply, diff, rebase};
use super::{Conflict, SyncError};
use crate::model::Project;
use crate::{guard, limits, schedule};

/// Why a change could not be taken back.
#[derive(Clone, Debug, PartialEq)]
pub enum Reversal {
    /// Someone changed what the change touched since; nothing was undone.
    ChangedSince(Vec<Conflict>),
    /// The plan without the change does not pass the checks, or the patch does not fit.
    Failed(String),
}

impl Reversal {
    /// A sentence for the person, e.g. "Someone changed the duration of task 'Pour slab' since".
    pub fn describe(&self, plan: &Project) -> String {
        match self {
            Reversal::ChangedSince(conflicts) => {
                let mut subjects: Vec<String> = conflicts.iter().map(|c| c.subject(plan)).collect();
                subjects.dedup();
                let more = subjects.len().saturating_sub(3);
                subjects.truncate(3);
                let list = subjects.join(", ");
                let by = conflicts.iter().find_map(|c| c.by.clone()).unwrap_or_else(|| "Someone".into());
                if more > 0 { format!("{by} changed {list} and {more} more since") } else { format!("{by} changed {list} since") }
            }
            Reversal::Failed(why) => why.clone(),
        }
    }
}

impl From<SyncError> for Reversal {
    fn from(e: SyncError) -> Self {
        Reversal::Failed(e.to_string())
    }
}

/// Takes back the change that turned `before` into `after`, on the plan as it is now (`current`,
/// which may hold other people's changes made since): the change's inverse, rebased over what
/// changed since. If someone changed any of the same values since, nothing is undone.
///
/// The result is checked and recalculated; it keeps `current`'s id counters (a replica's cursor
/// never goes back).
pub fn reverse(current: &Project, before: &Project, after: &Project) -> Result<Project, Reversal> {
    let inverse = diff(after, before)?;
    let since = diff(after, current)?;
    let r = rebase(&inverse, &since, None);
    if !r.conflicts.is_empty() {
        return Err(Reversal::ChangedSince(r.conflicts));
    }
    let mut p = apply(current, &r.patch)?;
    let checked = guard::catch("taking back a shared change", || -> Result<Project, String> {
        limits::check_edit(&mut p).map_err(|e| e.to_string())?;
        schedule::try_recalculate(&mut p).map_err(|e| e.message)?;
        Ok(p)
    });
    match checked {
        Ok(Ok(mut p)) => {
            p.ids = current.ids.clone();
            Ok(p)
        }
        Ok(Err(why)) => Err(Reversal::Failed(why)),
        Err(panicked) => Err(Reversal::Failed(panicked.to_string())),
    }
}

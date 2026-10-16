//! Edits whose closures report problems as plain messages (what the UI shows the user).

use plancraft_core::model::Project;
use plancraft_core::ops::PlanError;
use plancraft_core::workspace::{ChangeSource, Document};

/// A user-facing error message; any [`PlanError`] (limits, internal errors) converts into one.
struct Msg(String);

impl From<PlanError> for Msg {
    fn from(e: PlanError) -> Self {
        Self(e.to_string())
    }
}

pub trait EditExt {
    /// [`Document::try_edit`] by the user, with errors as messages for the status bar or a dialog.
    fn edit_msg<R>(&mut self, label: &str, f: impl FnOnce(&mut Project) -> Result<R, String>) -> Result<R, String>;
}

impl EditExt for Document {
    fn edit_msg<R>(&mut self, label: &str, f: impl FnOnce(&mut Project) -> Result<R, String>) -> Result<R, String> {
        self.try_edit(label, ChangeSource::User, |p| f(p).map_err(Msg)).map_err(|m| m.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plancraft_core::workspace::Workspace;

    #[test]
    fn messages_and_plan_errors_both_reach_the_caller() {
        let mut ws = Workspace::default();
        ws.add(Project::new("P", chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
        let doc = ws.active_doc_mut().unwrap();
        assert_eq!(doc.edit_msg("Bad", |_| Err::<(), _>("nope".to_string())), Err("nope".to_string()));
        let r = doc.edit_msg("Boom", |_| -> Result<(), String> { panic!("kaput") });
        assert!(r.is_err_and(|e| e.contains("kaput")));
        assert_eq!(doc.edit_msg("Ok", |p| Ok(p.add_task("A", None))).map(|_| ()), Ok(()));
    }
}

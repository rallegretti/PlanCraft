//! The Task Board: tasks as cards in status columns (Microsoft Project's Task Board view).
//!
//! Microsoft's published description of the board:
//! - Each task's Board Status names its column.
//! - New tasks start in Not Started.
//! - Columns can be renamed, and new ones are added to the right of the last.
//! - Moving a card to another column changes its Board Status.
//! - A column can carry a "Set % Complete" value that a task moved into it takes.
//! - Show on Board (Yes by default) hides a task from the board.
//! - A project has at most 100 columns.
//!
//! Choices where the pages say nothing:
//! - A task with no Board Status (or one naming a deleted column) sits in the first column.
//! - Summary tasks don't show; their subtasks do.
//! - Deleting a column moves its tasks to the first column.

use serde::{Deserialize, Serialize};

use crate::ids::{BoardColumnId, IdCounters, TaskId};
use crate::limits::{MAX_NAME_CHARS, MAX_TASKS};
use crate::model::Project;
use crate::ops::{PlanError, PlanResult};

/// Most columns a board can have.
pub const MAX_BOARD_COLUMNS: usize = 100;

/// A Task Board column (a Board Status value).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardColumn {
    pub id: BoardColumnId,
    pub name: String,
    /// Set % Complete: a task moved into this column takes this % complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_complete: Option<u8>,
}

/// The board a new plan starts with: Not Started, Next up, In progress and Done (Done sets
/// 100% complete).
pub fn default_columns(ids: &mut IdCounters) -> Vec<BoardColumn> {
    [("Not Started", None), ("Next up", None), ("In progress", None), ("Done", Some(100))]
        .into_iter()
        .map(|(name, percent_complete)| BoardColumn { id: ids.next_board_column(), name: name.into(), percent_complete })
        .collect()
}

fn invalid<T>(msg: impl Into<String>) -> PlanResult<T> {
    Err(PlanError::Invalid(msg.into()))
}

/// A trimmed column name, or why it can't be one.
fn column_name(name: &str) -> PlanResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return invalid("a column needs a name");
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return invalid(format!("a column name can be at most {MAX_NAME_CHARS} characters"));
    }
    Ok(name.to_string())
}

impl Project {
    pub fn board_column(&self, id: BoardColumnId) -> Option<&crate::board::BoardColumn> {
        self.board_columns.iter().find(|c| c.id == id)
    }

    /// A column by name (case ignored) or by id ("B3" or "3").
    pub fn find_board_column(&self, key: &str) -> Option<BoardColumnId> {
        let k = key.trim();
        if let Some(c) = self.board_columns.iter().find(|c| c.name.eq_ignore_ascii_case(k)) {
            return Some(c.id);
        }
        let n: u32 = k.trim_start_matches(['B', 'b']).parse().ok()?;
        self.board_column(BoardColumnId(n)).map(|c| c.id)
    }

    /// The column a task's card sits in: its Board Status, or the first column.
    pub fn board_column_of(&self, idx: usize) -> Option<BoardColumnId> {
        let t = self.tasks.get(idx)?;
        t.board_status.filter(|s| self.board_column(*s).is_some()).or_else(|| self.board_columns.first().map(|c| c.id))
    }

    /// Whether the task at `idx` has a card: shown on the board and not a summary task.
    pub fn on_board(&self, idx: usize) -> bool {
        self.tasks.get(idx).is_some_and(|t| t.show_on_board) && !self.has_children(idx)
    }

    /// The board: each column with the rows of its cards, in outline order.
    pub fn board(&self) -> Vec<(&crate::board::BoardColumn, Vec<usize>)> {
        let mut cols: Vec<(&crate::board::BoardColumn, Vec<usize>)> = self.board_columns.iter().map(|c| (c, Vec::new())).collect();
        let first = self.board_columns.first().map(|c| c.id);
        for (i, t) in self.tasks.iter().enumerate() {
            if !self.on_board(i) {
                continue;
            }
            let col = t.board_status.filter(|s| self.board_column(*s).is_some()).or(first);
            if let Some((_, rows)) = cols.iter_mut().find(|(c, _)| Some(c.id) == col) {
                rows.push(i);
            }
        }
        cols
    }

    fn require_column(&self, id: BoardColumnId) -> PlanResult<usize> {
        self.board_columns.iter().position(|c| c.id == id).ok_or_else(|| PlanError::Invalid(format!("there is no board column {id}")))
    }

    /// Adds a column at position `at` (to the right of the last when `None`).
    pub fn add_board_column(&mut self, name: &str, at: Option<usize>, percent_complete: Option<u8>) -> PlanResult<BoardColumnId> {
        let name = column_name(name)?;
        if self.board_columns.len() >= MAX_BOARD_COLUMNS {
            return invalid(format!("a board can have at most {MAX_BOARD_COLUMNS} columns"));
        }
        if self.board_columns.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
            return invalid(format!("there is already a column named '{name}'"));
        }
        if percent_complete.is_some_and(|p| p > 100) {
            return invalid("% complete is at most 100");
        }
        let id = self.ids.next_board_column();
        let at = at.unwrap_or(self.board_columns.len()).min(self.board_columns.len());
        self.board_columns.insert(at, BoardColumn { id, name, percent_complete });
        Ok(id)
    }

    pub fn rename_board_column(&mut self, id: BoardColumnId, name: &str) -> PlanResult<()> {
        let k = self.require_column(id)?;
        let name = column_name(name)?;
        if self.board_columns.iter().any(|c| c.id != id && c.name.eq_ignore_ascii_case(&name)) {
            return invalid(format!("there is already a column named '{name}'"));
        }
        if let Some(c) = self.board_columns.get_mut(k) {
            c.name = name;
        }
        Ok(())
    }

    /// Sets (or with `None` clears) the % complete a task moved into the column takes.
    pub fn set_board_column_percent(&mut self, id: BoardColumnId, percent_complete: Option<u8>) -> PlanResult<()> {
        let k = self.require_column(id)?;
        if percent_complete.is_some_and(|p| p > 100) {
            return invalid("% complete is at most 100");
        }
        if let Some(c) = self.board_columns.get_mut(k) {
            c.percent_complete = percent_complete;
        }
        Ok(())
    }

    /// Moves a column to position `to` (clamped).
    pub fn move_board_column(&mut self, id: BoardColumnId, to: usize) -> PlanResult<()> {
        let k = self.require_column(id)?;
        let col = self.board_columns.remove(k);
        let to = to.min(self.board_columns.len());
        self.board_columns.insert(to, col);
        Ok(())
    }

    /// Deletes a column; its tasks move to the first remaining column. The last column can't be
    /// deleted. Returns the number of tasks moved.
    pub fn delete_board_column(&mut self, id: BoardColumnId) -> PlanResult<usize> {
        let k = self.require_column(id)?;
        if self.board_columns.len() <= 1 {
            return invalid("the board needs at least one column");
        }
        self.board_columns.remove(k);
        let mut moved = 0;
        for t in &mut self.tasks {
            if t.board_status == Some(id) {
                t.board_status = None;
                moved += 1;
            }
        }
        Ok(moved)
    }

    /// Moves tasks' cards to a column (their Board Status). A column with a Set % Complete value
    /// gives them that % complete. Summary tasks are refused (their subtasks have the cards).
    pub fn set_board_status(&mut self, ids: &[TaskId], column: BoardColumnId) -> PlanResult<()> {
        let k = self.require_column(column)?;
        let percent = self.board_columns.get(k).and_then(|c| c.percent_complete);
        if ids.len() > MAX_TASKS {
            return invalid("too many tasks");
        }
        for &id in ids {
            let idx = self.task_index(id).ok_or(PlanError::TaskNotFound(id))?;
            if self.has_children(idx) {
                let name = self.tasks.get(idx).map(|t| t.name.clone()).unwrap_or_default();
                return invalid(format!("'{name}' is a summary task; its subtasks have the cards"));
            }
        }
        for &id in ids {
            if let Some(t) = self.task_mut(id) {
                t.board_status = Some(column);
            }
            if let Some(p) = percent {
                self.set_percent_complete(id, p)?;
            }
        }
        Ok(())
    }

    /// Shows or hides tasks on the board (Show on Board).
    pub fn set_show_on_board(&mut self, ids: &[TaskId], show: bool) -> PlanResult<()> {
        for &id in ids {
            self.task_mut(id).ok_or(PlanError::TaskNotFound(id))?.show_on_board = show;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use crate::prelude::*;

    fn plan() -> (Project, TaskId, TaskId) {
        let mut p = Project::new("B", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let a = p.add_task("A", None);
        let b = p.add_task("B", None);
        crate::schedule::recalculate(&mut p);
        (p, a, b)
    }

    #[test]
    fn columns_add_rename_move_and_delete() {
        let (mut p, a, _) = plan();
        let names = |p: &Project| p.board_columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&p), ["Not Started", "Next up", "In progress", "Done"]);
        let review = p.add_board_column("Review", None, None).unwrap();
        assert!(p.add_board_column(" review ", None, None).is_err(), "names are unique");
        assert!(p.add_board_column("", None, None).is_err());
        p.move_board_column(review, 3).unwrap();
        assert_eq!(names(&p)[3], "Review");
        p.rename_board_column(review, "QA").unwrap();
        assert_eq!(p.find_board_column("qa"), Some(review));
        assert_eq!(p.find_board_column(&review.to_string()), Some(review));
        p.set_board_status(&[a], review).unwrap();
        assert_eq!(p.delete_board_column(review).unwrap(), 1);
        assert_eq!(p.task(a).unwrap().board_status, None);
        assert_eq!(p.board_column_of(p.task_index(a).unwrap()), p.board_columns.first().map(|c| c.id));
        while p.board_columns.len() > 1 {
            let id = p.board_columns[0].id;
            p.delete_board_column(id).unwrap();
        }
        assert!(p.delete_board_column(p.board_columns[0].id).is_err(), "one column stays");
        for k in 0..99 {
            p.add_board_column(&format!("C{k}"), None, None).unwrap();
        }
        assert!(p.add_board_column("One too many", None, None).is_err());
    }

    #[test]
    fn summaries_have_no_card_and_hidden_tasks_leave_the_board() {
        let (mut p, a, b) = plan();
        p.indent(b).unwrap();
        let done = p.board_columns[3].id;
        assert!(p.set_board_status(&[a], done).is_err(), "A is now a summary");
        p.set_board_status(&[b], done).unwrap();
        assert_eq!(p.task(b).unwrap().percent_complete, 100, "Done sets 100%");
        let board = p.board();
        assert!(board.iter().all(|(_, rows)| !rows.contains(&0)));
        assert_eq!(board[3].1, [1]);
        p.set_show_on_board(&[b], false).unwrap();
        assert!(p.board().iter().all(|(_, rows)| rows.is_empty()));
    }
}

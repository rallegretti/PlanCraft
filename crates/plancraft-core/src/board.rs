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
//! - A new plan's columns are Not Started, Next up, In progress and Done (the pages name Not
//!   Started for new tasks and the other three for a sprint board), and Done sets 100%.
//! - A task with no Board Status (or one naming a deleted column) sits in the first column.
//! - Summary tasks don't show; their subtasks do.
//! - Deleting a column moves its tasks to the first column.

use serde::{Deserialize, Serialize};

use crate::ids::{BoardColumnId, IdCounters, TaskId};
use crate::limits::MAX_NAME_CHARS;
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

/// What makes two column names the same: case and surrounding spaces don't count. Used by the
/// ops and by the file checks alike, so they always agree.
pub fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// A column id written as "B3" or "3" (digits only).
fn parse_id(key: &str) -> Option<BoardColumnId> {
    let digits = key.strip_prefix(['B', 'b']).unwrap_or(key);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().map(BoardColumnId)
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

    /// A column by name (case ignored) or by id ("B3" or "3"); a name wins.
    pub fn find_board_column(&self, key: &str) -> Option<BoardColumnId> {
        let k = name_key(key);
        if let Some(c) = self.board_columns.iter().find(|c| name_key(&c.name) == k) {
            return Some(c.id);
        }
        self.board_column_by_id(key.trim())
    }

    /// A column by id only ("B3" or "3").
    pub fn board_column_by_id(&self, key: &str) -> Option<BoardColumnId> {
        parse_id(key).and_then(|id| self.board_column(id)).map(|c| c.id)
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
        let at: std::collections::HashMap<BoardColumnId, usize> = self.board_columns.iter().enumerate().map(|(k, c)| (c.id, k)).collect();
        for (i, t) in self.tasks.iter().enumerate() {
            if !self.on_board(i) {
                continue;
            }
            let k = t.board_status.and_then(|s| at.get(&s).copied()).unwrap_or(0);
            if let Some((_, rows)) = cols.get_mut(k) {
                rows.push(i);
            }
        }
        cols
    }

    /// Rows of `ids`, looked up in one pass (an unknown id is an error).
    fn rows_of(&self, ids: &[TaskId]) -> PlanResult<Vec<usize>> {
        let at: std::collections::HashMap<TaskId, usize> = self.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        ids.iter().map(|id| at.get(id).copied().ok_or(PlanError::TaskNotFound(*id))).collect()
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
        if self.board_columns.iter().any(|c| name_key(&c.name) == name_key(&name)) {
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
        if self.board_columns.iter().any(|c| c.id != id && name_key(&c.name) == name_key(&name)) {
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
    /// deleted. Returns the number of cards moved.
    pub fn delete_board_column(&mut self, id: BoardColumnId) -> PlanResult<usize> {
        let k = self.require_column(id)?;
        if self.board_columns.len() <= 1 {
            return invalid("the board needs at least one column");
        }
        let moved = self.board().get(k).map_or(0, |(_, rows)| rows.len());
        self.board_columns.remove(k);
        for t in &mut self.tasks {
            if t.board_status == Some(id) {
                t.board_status = None;
            }
        }
        Ok(moved)
    }

    /// Moves tasks' cards to a column (their Board Status). A column with a Set % Complete value
    /// gives them that % complete. Summary tasks are refused (their subtasks have the cards).
    pub fn set_board_status(&mut self, ids: &[TaskId], column: BoardColumnId) -> PlanResult<()> {
        let k = self.require_column(column)?;
        let percent = self.board_columns.get(k).and_then(|c| c.percent_complete);
        let rows = self.rows_of(ids)?;
        if let Some(&i) = rows.iter().find(|&&i| self.has_children(i)) {
            let name = self.tasks.get(i).map(|t| t.name.clone()).unwrap_or_default();
            return invalid(format!("'{name}' is a summary task; its subtasks have the cards"));
        }
        for i in rows {
            if let Some(t) = self.tasks.get_mut(i) {
                t.board_status = Some(column);
            }
            if let Some(p) = percent {
                self.set_percent_complete_at(i, p)?;
            }
        }
        Ok(())
    }

    /// Shows or hides tasks on the board (Show on Board).
    pub fn set_show_on_board(&mut self, ids: &[TaskId], show: bool) -> PlanResult<()> {
        for i in self.rows_of(ids)? {
            if let Some(t) = self.tasks.get_mut(i) {
                t.show_on_board = show;
            }
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

    /// Regressions: the ops compared names with ASCII-only case folding while the file check
    /// lowercased fully, so some names passed one and were renamed by the other (the edit rolled
    /// back); loose ids like "BB3" or "+3" resolved; deleting the first column under-counted the
    /// cards that moved (tasks with no Board Status already sat there).
    #[test]
    fn names_ids_and_counts_are_consistent() {
        let (mut p, a, b) = plan();
        p.add_board_column("ÉTÉ", None, None).unwrap();
        assert!(p.add_board_column("été", None, None).is_err());
        assert!(crate::limits::sanitize(&mut p.clone()).is_empty());
        let third = p.board_columns[2].id;
        for loose in ["BB3", "+3", "B", "b+3", " 3x"] {
            assert_eq!(p.find_board_column(loose), None, "{loose}");
        }
        assert_eq!(p.find_board_column("b3"), Some(third));
        let named = p.add_board_column("3", None, None).unwrap();
        assert_eq!(p.find_board_column("3"), Some(named), "a name wins over an id");
        assert_eq!(p.board_column_by_id("3"), Some(third));
        // A and B sit in the first column with no Board Status; one more was put there.
        let first = p.board_columns[0].id;
        p.set_board_status(&[b], first).unwrap();
        assert_eq!(p.delete_board_column(first).unwrap(), 2);
        assert!(p.task(a).unwrap().board_status.is_none() && p.task(b).unwrap().board_status.is_none());
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

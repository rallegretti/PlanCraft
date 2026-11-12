//! Main views.

pub mod calendar;
pub mod gantt;
pub mod gantt_chart;
pub mod milestones;
pub mod resource_sheet;
pub mod resource_usage;
pub mod task_usage;
pub mod team_planner;
pub mod timeline;
pub mod timesheet;
pub mod workload;

use std::collections::{HashMap, HashSet};

use plancraft_core::prelude::*;

use crate::columns;
use crate::fields::{Field, SortKey};
use crate::state::{Selection, TaskFilter, UiState};

/// One row of a task view: a task, or the header of a group when tasks are grouped by a field.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Task(usize),
    Group(GroupRow),
}

/// A group header: the shared value and the tasks under it (task indices, in display order).
#[derive(Clone, Debug, PartialEq)]
pub struct GroupRow {
    pub label: String,
    pub tasks: Vec<usize>,
    pub collapsed: bool,
}

impl Row {
    /// The task index of a task row.
    pub fn task(&self) -> Option<usize> {
        match self {
            Row::Task(i) => Some(*i),
            Row::Group(_) => None,
        }
    }
}

/// The task shown on display row `row` (`None` for group headers and blank rows).
pub fn task_at<'a>(p: &'a Project, rows: &[Row], row: usize) -> Option<&'a Task> {
    rows.get(row).and_then(Row::task).and_then(|i| p.tasks.get(i))
}

/// The display row of the task at index `idx`, if it is shown.
pub fn row_of_task(rows: &[Row], idx: usize) -> Option<usize> {
    rows.iter().position(|r| r.task() == Some(idx))
}

fn matches_filter(p: &Project, t: &Task, f: TaskFilter) -> bool {
    match f {
        TaskFilter::All => true,
        TaskFilter::Critical => t.sched.critical,
        TaskFilter::Incomplete => !t.is_complete(),
        TaskFilter::Completed => t.is_complete(),
        TaskFilter::Milestones => t.is_milestone(),
        TaskFilter::Late => columns::is_late(p, t),
        TaskFilter::Summary => t.sched.is_summary,
        TaskFilter::Unassigned => !t.sched.is_summary && !t.is_milestone() && p.assignments_for_task(t.id).next().is_none(),
    }
}

/// Orders task indices by the sort field (ties keep outline order; empty values stay last in
/// either direction).
fn sort_indices(p: &Project, items: &mut [usize], field: &Field, descending: bool) {
    let keys: HashMap<usize, SortKey> = items.iter().map(|&i| (i, field.sort_key(p, i))).collect();
    items.sort_by(|a, b| {
        let (ka, kb) = (keys.get(a).unwrap_or(&SortKey::Missing), keys.get(b).unwrap_or(&SortKey::Missing));
        let missing = (matches!(ka, SortKey::Missing), matches!(kb, SortKey::Missing));
        let ord = match missing {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ if descending => kb.cmp(ka),
            _ => ka.cmp(kb),
        };
        ord.then(a.cmp(b))
    });
}

/// Rows shown in task views, honoring the filter and search text, the sort (within the outline:
/// subtasks stay under their summary), grouping (subtasks under group headers) and collapsed
/// summaries and groups.
pub fn visible_rows(p: &Project, st: &UiState) -> Vec<Row> {
    let search = st.search.trim().to_lowercase();
    let filtering = st.filter != TaskFilter::All || !search.is_empty();
    let matches = |t: &Task| matches_filter(p, t, st.filter) && (search.is_empty() || t.name.to_lowercase().contains(&search));
    let sort = st.sort.as_ref().and_then(|(k, desc)| Field::from_key(p, k).map(|f| (f, *desc)));

    if let Some(group) = st.group.as_ref().and_then(|k| Field::from_key(p, k)) {
        // Grouped: subtasks only, under one header per value.
        let mut tasks: Vec<usize> =
            p.tasks.iter().enumerate().filter(|(_, t)| !t.sched.is_summary && (!filtering || matches(t))).map(|(i, _)| i).collect();
        if let Some((field, desc)) = &sort {
            sort_indices(p, &mut tasks, field, *desc);
        }
        let mut groups: Vec<(SortKey, GroupRow)> = Vec::new();
        let mut by_label: HashMap<String, usize> = HashMap::new();
        for i in tasks {
            let label = group.group_label(p, i);
            let key = group.sort_key(p, i);
            let slot = *by_label.entry(label.clone()).or_insert_with(|| {
                groups.push((
                    key.clone(),
                    GroupRow { label: label.clone(), tasks: Vec::new(), collapsed: st.collapsed_groups.contains(&label) },
                ));
                groups.len().saturating_sub(1)
            });
            if let Some((k, g)) = groups.get_mut(slot) {
                if key.cmp(k) == std::cmp::Ordering::Less {
                    *k = key;
                }
                g.tasks.push(i);
            }
        }
        groups.sort_by(|(ka, a), (kb, b)| ka.cmp(kb).then_with(|| a.label.cmp(&b.label)));
        let mut rows = Vec::new();
        for (_, g) in groups {
            let shown = if g.collapsed { Vec::new() } else { g.tasks.clone() };
            rows.push(Row::Group(g));
            rows.extend(shown.into_iter().map(Row::Task));
        }
        return rows;
    }

    let mut keep: Vec<bool> = vec![!filtering; p.tasks.len()];
    if filtering {
        for (i, t) in p.tasks.iter().enumerate() {
            if !matches(t) {
                continue;
            }
            // Keep summary rows above matches as context, like "show related summary rows".
            let mut a = Some(i);
            while let Some(ai) = a {
                if let Some(k) = keep.get_mut(ai) {
                    *k = true;
                }
                a = p.parent_index(ai);
            }
        }
    }
    // Children of each row (and the top level), from one pass over outline levels.
    let mut roots: Vec<usize> = Vec::new();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); p.tasks.len()];
    let mut stack: Vec<(usize, u16)> = Vec::new();
    for (i, t) in p.tasks.iter().enumerate() {
        while stack.last().is_some_and(|&(_, lvl)| lvl >= t.outline_level) {
            stack.pop();
        }
        match stack.last().and_then(|&(pi, _)| children.get_mut(pi)) {
            Some(list) => list.push(i),
            None => roots.push(i),
        }
        stack.push((i, t.outline_level));
    }
    if let Some((field, desc)) = &sort {
        sort_indices(p, &mut roots, field, *desc);
        for list in &mut children {
            sort_indices(p, list, field, *desc);
        }
    }
    let mut rows = Vec::with_capacity(p.tasks.len());
    let mut todo: Vec<usize> = roots.into_iter().rev().collect();
    while let Some(i) = todo.pop() {
        if !keep.get(i).copied().unwrap_or(false) {
            continue;
        }
        rows.push(Row::Task(i));
        let collapsed = p.tasks.get(i).is_some_and(|t| st.collapsed.contains(&t.id));
        if !collapsed && let Some(kids) = children.get(i) {
            todo.extend(kids.iter().rev());
        }
    }
    rows
}

/// Applies a click on `row` to the task selection. A group header selects the tasks under it.
pub fn select_row(sel: &mut Selection, p: &Project, rows: &[Row], row: usize, shift: bool, ctrl: bool) {
    if let Some(Row::Group(g)) = rows.get(row) {
        if !ctrl {
            sel.tasks.clear();
        }
        sel.tasks.extend(g.tasks.iter().filter_map(|&i| p.tasks.get(i)).map(|t| t.id));
        return;
    }
    let Some(id) = task_at(p, rows, row).map(|t| t.id) else { return };
    if shift && let Some(anchor) = sel.anchor {
        let a = rows.iter().position(|r| task_at(p, std::slice::from_ref(r), 0).is_some_and(|t| t.id == anchor)).unwrap_or(row);
        let (lo, hi) = if a <= row { (a, row) } else { (row, a) };
        if !ctrl {
            sel.tasks.clear();
        }
        for r in rows.iter().take(hi + 1).skip(lo) {
            if let Some(t) = r.task().and_then(|i| p.tasks.get(i)) {
                sel.tasks.insert(t.id);
            }
        }
    } else if ctrl {
        if !sel.tasks.remove(&id) {
            sel.tasks.insert(id);
        }
        sel.anchor = Some(id);
    } else {
        sel.select_task(id);
    }
}

/// Selected task ids in outline order.
pub fn selected_in_order(p: &Project, sel: &Selection) -> Vec<TaskId> {
    p.tasks.iter().filter(|t| sel.tasks.contains(&t.id)).map(|t| t.id).collect()
}

/// Task ids that have an assignment to an overallocated resource.
pub fn overallocated_tasks(p: &Project, over: &HashSet<ResourceId>) -> HashSet<TaskId> {
    if over.is_empty() {
        return HashSet::new();
    }
    p.assignments.iter().filter(|a| over.contains(&a.resource)).map(|a| a.task).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columns::{TaskCol, commit};

    fn demo() -> Project {
        plancraft_core::sample::demo_project(chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap())
    }

    #[test]
    fn collapse_and_filter_rows() {
        let p = demo();
        let mut st = UiState::default();
        assert_eq!(visible_rows(&p, &st).len(), p.tasks.len());
        let planning = p.tasks[0].id;
        st.collapsed.insert(planning);
        assert_eq!(visible_rows(&p, &st).len(), p.tasks.len() - 3);
        st.collapsed.clear();
        st.filter = TaskFilter::Milestones;
        let rows = visible_rows(&p, &st);
        // 4 milestones plus their 4 summary parents.
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().filter_map(Row::task).all(|i| p.tasks[i].is_milestone() || p.tasks[i].sched.is_summary));
    }

    #[test]
    fn sorting_keeps_subtasks_under_their_summary() {
        let p = demo();
        let mut st = UiState { sort: Some(("duration".into(), true)), ..UiState::default() };
        let rows = visible_rows(&p, &st);
        assert_eq!(rows.len(), p.tasks.len(), "sorting hides nothing");
        // Each subtask still follows its own summary, and siblings run longest first.
        let order: Vec<usize> = rows.iter().filter_map(Row::task).collect();
        for (pos, &i) in order.iter().enumerate() {
            if let Some(parent) = p.parent_index(i) {
                let ppos = order.iter().position(|&x| x == parent).unwrap();
                assert!(ppos < pos, "{} shown before its summary", p.tasks[i].name);
                assert!(order[ppos..pos].iter().all(|&x| x == parent || p.parent_index(x).is_some()), "subtasks stay inside their summary");
            }
        }
        for parent in (0..p.tasks.len()).filter(|&i| p.has_children(i)) {
            let kids: Vec<usize> = order.iter().copied().filter(|&x| p.parent_index(x) == Some(parent)).collect();
            assert!(kids.windows(2).all(|w| p.tasks[w[0]].sched.duration >= p.tasks[w[1]].sched.duration), "{}", p.tasks[parent].name);
        }
        st.sort = Some(("duration".into(), false));
        let asc: Vec<usize> = visible_rows(&p, &st).iter().filter_map(Row::task).collect();
        assert_ne!(asc, order);
    }

    #[test]
    fn grouping_by_a_custom_field_lists_subtasks_under_headers() {
        let p = demo();
        let status = p.find_custom_field("Status").unwrap().id;
        let mut st = UiState { group: Some(plancraft_core::custom::custom_column_key(status)), ..UiState::default() };
        let rows = visible_rows(&p, &st);
        let groups: Vec<&GroupRow> = rows.iter().filter_map(|r| if let Row::Group(g) = r { Some(g) } else { None }).collect();
        let labels: Vec<&str> = groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["Done", "In progress", "To do", "No value"], "groups in value order, blanks last");
        let mut seen = 0;
        for g in &groups {
            for &i in &g.tasks {
                let t = &p.tasks[i];
                assert!(!t.sched.is_summary, "summaries are not grouped");
                let v = t.custom.get(&status).map(|v| p.custom_value_text(status, v));
                assert_eq!(v.as_deref().unwrap_or("No value"), g.label);
                seen += 1;
            }
        }
        assert_eq!(seen, p.tasks.iter().filter(|t| !t.sched.is_summary).count());
        // A collapsed group shows its header only.
        st.collapsed_groups.insert("To do".into());
        let rows = visible_rows(&p, &st);
        let todo = rows.iter().position(|r| matches!(r, Row::Group(g) if g.label == "To do")).unwrap();
        assert!(matches!(rows.get(todo + 1), Some(Row::Group(_))), "no task rows under a collapsed group");
    }

    #[test]
    fn pick_list_cells_set_and_clear_values() {
        let mut p = demo();
        let risk = p.find_custom_field("Risk").unwrap().id;
        let id = p.tasks[1].id;
        let field = crate::fields::Field::Custom(risk);
        field.commit(&mut p, id, "High").unwrap();
        assert_eq!(p.task(id).unwrap().custom.get(&risk), Some(&CustomValue::Text("High".into())));
        assert!(field.commit(&mut p, id, "Severe").is_err());
        field.commit(&mut p, id, crate::fields::NO_VALUE).unwrap();
        assert!(!p.task(id).unwrap().custom.contains_key(&risk));
    }

    #[test]
    fn column_commits_follow_ms_project_rules() {
        let mut p = demo();
        let id = p.tasks[1].id;
        commit(TaskCol::Duration, &mut p, id, "4d").unwrap();
        assert_eq!(p.task(id).unwrap().duration, 4 * 480);
        // Typing a start date on an auto task sets a Start No Earlier Than constraint.
        commit(TaskCol::Start, &mut p, id, "2026-11-02").unwrap();
        assert_eq!(p.task(id).unwrap().constraint.kind, ConstraintKind::Snet);
        assert!(commit(TaskCol::Duration, &mut p, id, "abc").is_err());
        assert!(commit(TaskCol::Name, &mut p, id, "  ").is_err());
        // Row 3 already depends on this task, so linking back is rejected.
        assert!(commit(TaskCol::Predecessors, &mut p, id, "3").is_err());
        let event = p.tasks[16].id;
        commit(TaskCol::Predecessors, &mut p, event, "14FS+1d").unwrap();
        assert_eq!(p.format_predecessors(event), "14FS+1 day");
    }
}

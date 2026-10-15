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

use std::collections::HashSet;

use plancraft_core::prelude::*;

use crate::columns;
use crate::state::{Selection, TaskFilter, UiState};

fn matches_filter(p: &Project, idx: usize, f: TaskFilter) -> bool {
    let t = &p.tasks[idx];
    match f {
        TaskFilter::All => true,
        TaskFilter::Critical => t.sched.critical,
        TaskFilter::Incomplete => !t.is_complete(),
        TaskFilter::Completed => t.is_complete(),
        TaskFilter::Milestones => t.is_milestone(),
        TaskFilter::Late => columns::is_late(p, t),
        TaskFilter::Summary => t.sched.is_summary,
        TaskFilter::Unassigned => {
            !t.sched.is_summary
                && !t.is_milestone()
                && p.assignments_for_task(t.id).next().is_none()
        }
    }
}

/// Task indices shown in task views, honoring collapsed summaries, the filter and search text.
pub fn visible_rows(p: &Project, st: &UiState) -> Vec<usize> {
    let search = st.search.trim().to_lowercase();
    let filtering = st.filter != TaskFilter::All || !search.is_empty();
    let mut keep: Vec<bool> = vec![!filtering; p.tasks.len()];
    if filtering {
        for i in 0..p.tasks.len() {
            let ok = matches_filter(p, i, st.filter)
                && (search.is_empty() || p.tasks[i].name.to_lowercase().contains(&search));
            if !ok {
                continue;
            }
            keep[i] = true;
            // Keep summary rows above matches as context, like "show related summary rows".
            let mut a = p.parent_index(i);
            while let Some(ai) = a {
                keep[ai] = true;
                a = p.parent_index(ai);
            }
        }
    }
    let mut rows = Vec::with_capacity(p.tasks.len());
    let mut skip_deeper: Option<u16> = None;
    for (i, t) in p.tasks.iter().enumerate() {
        if let Some(lvl) = skip_deeper {
            if t.outline_level > lvl {
                continue;
            }
            skip_deeper = None;
        }
        if !keep[i] {
            continue;
        }
        rows.push(i);
        if st.collapsed.contains(&t.id) && p.has_children(i) {
            skip_deeper = Some(t.outline_level);
        }
    }
    rows
}

/// Applies a click on `row` to the task selection.
pub fn select_row(
    sel: &mut Selection,
    p: &Project,
    rows: &[usize],
    row: usize,
    shift: bool,
    ctrl: bool,
) {
    let Some(&idx) = rows.get(row) else { return };
    let id = p.tasks[idx].id;
    if shift && let Some(anchor) = sel.anchor {
        let a = rows
            .iter()
            .position(|&i| p.tasks[i].id == anchor)
            .unwrap_or(row);
        let (lo, hi) = if a <= row { (a, row) } else { (row, a) };
        if !ctrl {
            sel.tasks.clear();
        }
        for &i in &rows[lo..=hi] {
            sel.tasks.insert(p.tasks[i].id);
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
    p.tasks
        .iter()
        .filter(|t| sel.tasks.contains(&t.id))
        .map(|t| t.id)
        .collect()
}

/// Task ids that have an assignment to an overallocated resource.
pub fn overallocated_tasks(p: &Project, over: &HashSet<ResourceId>) -> HashSet<TaskId> {
    if over.is_empty() {
        return HashSet::new();
    }
    p.assignments
        .iter()
        .filter(|a| over.contains(&a.resource))
        .map(|a| a.task)
        .collect()
}

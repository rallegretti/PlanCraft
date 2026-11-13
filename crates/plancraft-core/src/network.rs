//! Network Diagram layout: one box per task, links between them.
//!
//! Boxes go in columns by dependency depth (a task sits right of all its predecessors) and in
//! rows by path, like Microsoft Project's default "Top Down From Left" arrangement: a path runs
//! along one row and each new path starts a new row below. With `critical_first`, critical paths
//! are laid out first ("Top Down - Critical First"). A summary box sits left of its subtasks.
//!
//! The layout is in diagram units (points at 100% zoom); the app and print scale it. Everything
//! here is linear in tasks and links, so large plans lay out quickly.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::duration::Lag;
use crate::ids::TaskId;
use crate::model::{LinkType, Project, TaskRef};

/// How the diagram is arranged and drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct NetworkOptions {
    /// Lay out critical paths first (top rows).
    pub critical_first: bool,
    /// Small boxes that show only the task ID.
    pub ids_only: bool,
}

/// Sizes of boxes and the gaps between them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub box_w: f32,
    pub box_h: f32,
    pub col_gap: f32,
    pub row_gap: f32,
    pub margin: f32,
}

impl Geometry {
    pub fn new(ids_only: bool) -> Self {
        if ids_only {
            Self { box_w: 44.0, box_h: 26.0, col_gap: 34.0, row_gap: 18.0, margin: 16.0 }
        } else {
            Self { box_w: 176.0, box_h: 76.0, col_gap: 48.0, row_gap: 26.0, margin: 16.0 }
        }
    }
}

/// A task's box: column and row in the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// Index of the task in `Project::tasks`.
    pub task: usize,
    pub col: u32,
    pub row: u32,
}

/// What a line between two boxes stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetLinkKind {
    /// A dependency (or, for a collapsed summary, its subtasks' dependencies).
    Dependency { kind: LinkType, lag: Lag },
    /// From a task to a milestone it is a member of (the milestone follows its members).
    Member,
    /// From a summary to a subtask that starts its part of the diagram.
    Outline,
}

/// A line between two boxes (indices into `Diagram::nodes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetLink {
    pub from: usize,
    pub to: usize,
    pub kind: NetLinkKind,
    /// Both tasks are critical.
    pub critical: bool,
}

/// A rectangle in diagram units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl BoxRect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn mid_y(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

/// A laid-out Network Diagram.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagram {
    pub nodes: Vec<Node>,
    pub links: Vec<NetLink>,
    pub cols: u32,
    pub rows: u32,
    pub geometry: Geometry,
    /// Node index by task index.
    by_task: HashMap<usize, usize>,
    /// Node index by (column, row).
    by_cell: HashMap<(u32, u32), usize>,
}

impl Diagram {
    /// The node drawn for the task at `task` (index into `Project::tasks`).
    pub fn node_of(&self, task: usize) -> Option<usize> {
        self.by_task.get(&task).copied()
    }

    /// Box of node `n`.
    pub fn rect(&self, n: usize) -> Option<BoxRect> {
        let node = self.nodes.get(n)?;
        Some(self.cell_rect(node.col, node.row))
    }

    fn cell_rect(&self, col: u32, row: u32) -> BoxRect {
        let g = &self.geometry;
        BoxRect {
            x: g.margin + col as f32 * (g.box_w + g.col_gap),
            y: g.margin + row as f32 * (g.box_h + g.row_gap),
            w: g.box_w,
            h: g.box_h,
        }
    }

    /// Width and height of the whole diagram.
    pub fn size(&self) -> (f32, f32) {
        let g = &self.geometry;
        let w = 2.0 * g.margin + self.cols as f32 * (g.box_w + g.col_gap) - g.col_gap;
        let h = 2.0 * g.margin + self.rows as f32 * (g.box_h + g.row_gap) - g.row_gap;
        (w.max(2.0 * g.margin), h.max(2.0 * g.margin))
    }

    /// The node whose box contains the point, if any.
    pub fn node_at(&self, x: f32, y: f32) -> Option<usize> {
        let g = &self.geometry;
        let (fx, fy) = ((x - g.margin) / (g.box_w + g.col_gap), (y - g.margin) / (g.box_h + g.row_gap));
        if !fx.is_finite() || !fy.is_finite() || fx < 0.0 || fy < 0.0 || fx > u32::MAX as f32 || fy > u32::MAX as f32 {
            return None;
        }
        let (col, row) = (fx as u32, fy as u32);
        let n = *self.by_cell.get(&(col, row))?;
        let r = self.cell_rect(col, row);
        (x <= r.right() && y <= r.bottom()).then_some(n)
    }

    /// The right-angled line of link `l`: out of the right side of the predecessor's box, into
    /// the left side of the successor's. A link that skips columns runs in the gap above the
    /// successor's row, so it does not cross boxes on the way.
    pub fn route(&self, l: &NetLink) -> Vec<(f32, f32)> {
        let (Some(a), Some(b), Some(na), Some(nb)) = (self.rect(l.from), self.rect(l.to), self.nodes.get(l.from), self.nodes.get(l.to))
        else {
            return Vec::new();
        };
        let g = &self.geometry;
        let (x1, y1, x2, y2) = (a.right(), a.mid_y(), b.x, b.mid_y());
        if nb.col == na.col.saturating_add(1) {
            if na.row == nb.row {
                return vec![(x1, y1), (x2, y2)];
            }
            let xm = x2 - g.col_gap / 2.0;
            return vec![(x1, y1), (xm, y1), (xm, y2), (x2, y2)];
        }
        let lane = b.y - g.row_gap / 2.0;
        let (xa, xb) = (x1 + g.col_gap / 2.0, x2 - g.col_gap / 2.0);
        vec![(x1, y1), (xa, y1), (xa, lane), (xb, lane), (xb, y2), (x2, y2)]
    }
}

/// Which box each task is drawn in (indexed like `Project::tasks`): the box of a collapsed
/// summary above it, else its own when `shown`, otherwise none.
pub fn boxes_for(p: &Project, shown: &[bool], collapsed: &HashSet<TaskId>) -> Vec<Option<usize>> {
    let mut out: Vec<Option<usize>> = Vec::with_capacity(p.tasks.len());
    let mut stack: Vec<(usize, u16)> = Vec::new();
    for (i, t) in p.tasks.iter().enumerate() {
        while stack.last().is_some_and(|&(_, lvl)| lvl >= t.outline_level) {
            stack.pop();
        }
        let parent = stack.last().map(|&(pi, _)| pi);
        // Under a collapsed summary (or a task folded into one), a task is drawn in its box
        // whether or not it is shown.
        let folded = parent.and_then(|pi| {
            let pb = out.get(pi).copied().flatten()?;
            let parent_collapsed = p.tasks.get(pi).is_some_and(|pt| collapsed.contains(&pt.id));
            (parent_collapsed || pb != pi).then_some(pb)
        });
        let b = folded.or_else(|| shown.get(i).copied().unwrap_or(false).then_some(i));
        out.push(b);
        stack.push((i, t.outline_level));
    }
    out
}

/// Lays out the diagram. `boxes` says which box each task is drawn in (see [`boxes_for`]); a
/// task drawn in its own box gets a node.
pub fn layout(p: &Project, boxes: &[Option<usize>], opts: NetworkOptions) -> Diagram {
    let n_tasks = p.tasks.len();
    let box_of = |i: usize| boxes.get(i).copied().flatten().filter(|&b| b < n_tasks);
    // Nodes, in outline order.
    let tasks: Vec<usize> = (0..n_tasks).filter(|&i| box_of(i) == Some(i)).collect();
    let node_of: HashMap<usize, usize> = tasks.iter().enumerate().map(|(n, &i)| (i, n)).collect();
    let n = tasks.len();
    let critical = |node: usize| tasks.get(node).and_then(|&i| p.tasks.get(i)).is_some_and(|t| t.sched.critical);

    // Dependencies between boxes (one line per pair of boxes).
    let index: HashMap<TaskId, usize> = p.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    let mut links: Vec<NetLink> = Vec::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for (si, t) in p.tasks.iter().enumerate() {
        let Some(to) = box_of(si).and_then(|b| node_of.get(&b).copied()) else { continue };
        let deps = t.predecessors.iter().map(|d| (d.predecessor, NetLinkKind::Dependency { kind: d.kind, lag: d.lag }));
        let members = t.members.iter().map(|m| (*m, NetLinkKind::Member));
        for (pred, kind) in deps.chain(members) {
            let TaskRef::Local(pid) = pred else { continue };
            let Some(from) = index.get(&pid).and_then(|&pi| box_of(pi)).and_then(|b| node_of.get(&b).copied()) else {
                continue;
            };
            if from != to && seen.insert((from, to)) {
                links.push(NetLink { from, to, kind, critical: critical(from) && critical(to) });
            }
        }
    }

    // Each node's nearest summary that has a node (summaries sit left of their subtasks).
    let mut owner: Vec<Option<usize>> = vec![None; n];
    {
        let mut stack: Vec<(usize, u16)> = Vec::new();
        let mut nearest: Vec<Option<usize>> = vec![None; n_tasks];
        for (i, t) in p.tasks.iter().enumerate() {
            while stack.last().is_some_and(|&(_, lvl)| lvl >= t.outline_level) {
                stack.pop();
            }
            let parent = stack.last().map(|&(pi, _)| pi);
            let above = parent.and_then(|pi| if node_of.contains_key(&pi) { Some(pi) } else { nearest.get(pi).copied().flatten() });
            if let Some(slot) = nearest.get_mut(i) {
                *slot = above;
            }
            if let (Some(&node), Some(a)) = (node_of.get(&i), above)
                && let Some(o) = owner.get_mut(node)
            {
                *o = node_of.get(&a).copied();
            }
            stack.push((i, t.outline_level));
        }
    }
    // Layout edges: dependencies plus summary -> subtask.
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg: Vec<usize> = vec![0; n];
    let mut has_sibling_pred: Vec<bool> = vec![false; n];
    for l in &links {
        if let Some(s) = succ.get_mut(l.from) {
            s.push(l.to);
        }
        if let Some(d) = indeg.get_mut(l.to) {
            *d += 1;
        }
        if owner.get(l.from) == owner.get(l.to)
            && let Some(h) = has_sibling_pred.get_mut(l.to)
        {
            *h = true;
        }
    }
    let mut is_summary = vec![false; n];
    for (child, o) in owner.iter().enumerate() {
        let Some(s) = *o else { continue };
        if let Some(list) = succ.get_mut(s) {
            list.push(child);
        }
        if let Some(d) = indeg.get_mut(child) {
            *d += 1;
        }
        if let Some(f) = is_summary.get_mut(s) {
            *f = true;
        }
    }
    for list in &mut succ {
        list.sort_unstable();
        list.dedup();
        if opts.critical_first {
            list.sort_by_key(|&s| !critical(s));
        }
    }

    // Columns: longest path from the start (Kahn's algorithm; nodes on a cycle, which a damaged
    // file could hold, go after the rest).
    let mut col: Vec<u32> = vec![0; n];
    let mut left = indeg.clone();
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| left.get(i) == Some(&0)).collect();
    let mut done = vec![false; n];
    let mut max_col = 0;
    let mut topo: Vec<usize> = Vec::with_capacity(n);
    while let Some(a) = queue.pop_front() {
        if let Some(d) = done.get_mut(a) {
            *d = true;
        }
        topo.push(a);
        let ca = col.get(a).copied().unwrap_or(0);
        max_col = max_col.max(ca);
        for &b in succ.get(a).map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(cb) = col.get_mut(b) {
                *cb = (*cb).max(ca.saturating_add(1));
            }
            if let Some(l) = left.get_mut(b) {
                *l = l.saturating_sub(1);
                if *l == 0 {
                    queue.push_back(b);
                }
            }
        }
    }
    for i in 0..n {
        if !done.get(i).copied().unwrap_or(true) {
            max_col = max_col.saturating_add(1);
            if let Some(c) = col.get_mut(i) {
                *c = max_col;
            }
        }
    }

    // A summary moves right, next to its first subtask (inner summaries first).
    for &a in topo.iter().rev() {
        if !is_summary.get(a).copied().unwrap_or(false) {
            continue;
        }
        let first = succ.get(a).and_then(|list| list.iter().filter_map(|&b| col.get(b).copied()).min());
        if let (Some(first), Some(c)) = (first, col.get_mut(a))
            && first > c.saturating_add(1)
        {
            *c = first - 1;
        }
    }
    // Lines from a summary to the subtasks next to it that start its part of the diagram.
    for (child, o) in owner.iter().enumerate() {
        let Some(s) = *o else { continue };
        let adjacent = col.get(child).copied() == col.get(s).map(|c| c.saturating_add(1));
        if adjacent && !has_sibling_pred.get(child).copied().unwrap_or(false) {
            links.push(NetLink { from: s, to: child, kind: NetLinkKind::Outline, critical: false });
        }
    }

    // Rows: each path runs along a row; a branch, or a new start, opens a new row below.
    let mut row: Vec<u32> = vec![0; n];
    let mut placed = vec![false; n];
    let mut taken: HashSet<(u32, u32)> = HashSet::new();
    let mut next_row: u32 = 0;
    // Paths follow the links between tasks; summaries are placed afterwards, beside their subtasks.
    let summary = |i: usize| is_summary.get(i).copied().unwrap_or(false);
    let mut link_succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut link_indeg: Vec<usize> = vec![0; n];
    for l in links.iter().filter(|l| l.kind != NetLinkKind::Outline && !summary(l.from) && !summary(l.to)) {
        if let Some(list) = link_succ.get_mut(l.from) {
            list.push(l.to);
        }
        if let Some(d) = link_indeg.get_mut(l.to) {
            *d += 1;
        }
    }
    for list in &mut link_succ {
        list.sort_unstable();
        list.dedup();
        if opts.critical_first {
            list.sort_by_key(|&s| !critical(s));
        }
    }
    let mut starts: Vec<usize> = (0..n).filter(|&i| !summary(i) && link_indeg.get(i) == Some(&0)).collect();
    if opts.critical_first {
        starts.sort_by_key(|&s| !critical(s));
    }
    // Nodes on a cycle have no start of their own.
    starts.extend((0..n).filter(|&i| !summary(i)));
    for start in starts {
        let mut pending: VecDeque<usize> = VecDeque::from([start]);
        while let Some(first) = pending.pop_front() {
            if placed.get(first).copied().unwrap_or(true) {
                continue;
            }
            let mut r = next_row;
            let mut cur = Some(first);
            while let Some(a) = cur {
                let c = col.get(a).copied().unwrap_or(0);
                // The row it continues on, or the first free one below (a fresh row after a few
                // tries, so crowded diagrams stay linear).
                let mut rr = r;
                let mut tries = 0;
                while taken.contains(&(c, rr)) {
                    tries += 1;
                    rr = if tries > 32 { next_row } else { rr.saturating_add(1) };
                }
                taken.insert((c, rr));
                if let (Some(slot), Some(p)) = (row.get_mut(a), placed.get_mut(a)) {
                    *slot = rr;
                    *p = true;
                }
                next_row = next_row.max(rr.saturating_add(1));
                r = rr;
                cur = None;
                for &b in link_succ.get(a).map(Vec::as_slice).unwrap_or(&[]) {
                    if placed.get(b).copied().unwrap_or(true) {
                        continue;
                    }
                    if cur.is_none() && col.get(b).copied().unwrap_or(0) > c {
                        cur = Some(b);
                    } else {
                        pending.push_back(b);
                    }
                }
            }
        }
    }

    // Summaries (inner ones first): next to their first subtask, on the row of their highest
    // subtask or the first free one below it.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (k, o) in owner.iter().enumerate() {
        if let Some(list) = o.and_then(|s| children.get_mut(s)) {
            list.push(k);
        }
    }
    for &a in topo.iter().rev().filter(|&&a| summary(a)) {
        let c = col.get(a).copied().unwrap_or(0);
        let want = children.get(a).and_then(|list| list.iter().filter_map(|&k| row.get(k).copied()).min());
        let mut rr = want.unwrap_or(next_row);
        let mut tries = 0;
        while taken.contains(&(c, rr)) {
            tries += 1;
            rr = if tries > 32 { next_row } else { rr.saturating_add(1) };
        }
        taken.insert((c, rr));
        if let (Some(slot), Some(p)) = (row.get_mut(a), placed.get_mut(a)) {
            *slot = rr;
            *p = true;
        }
        next_row = next_row.max(rr.saturating_add(1));
    }
    // Summaries on a cycle.
    for a in (0..n).filter(|&a| summary(a) && !placed.get(a).copied().unwrap_or(true)) {
        let c = col.get(a).copied().unwrap_or(0);
        taken.insert((c, next_row));
        if let Some(slot) = row.get_mut(a) {
            *slot = next_row;
        }
        next_row = next_row.saturating_add(1);
    }

    let nodes: Vec<Node> = tasks
        .iter()
        .enumerate()
        .map(|(k, &task)| Node { task, col: col.get(k).copied().unwrap_or(0), row: row.get(k).copied().unwrap_or(0) })
        .collect();
    let by_cell = nodes.iter().enumerate().map(|(k, nd)| ((nd.col, nd.row), k)).collect();
    let cols = if n == 0 { 0 } else { max_col.saturating_add(1) };
    Diagram { nodes, links, cols, rows: next_row, geometry: Geometry::new(opts.ids_only), by_task: node_of, by_cell }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::duration::Minutes;
    use crate::schedule::recalculate;
    use chrono::NaiveDate;

    const DAY: Minutes = 480;

    fn project() -> Project {
        Project::new("Net", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap())
    }

    fn all(p: &Project) -> Diagram {
        let shown = vec![true; p.tasks.len()];
        layout(p, &boxes_for(p, &shown, &HashSet::new()), NetworkOptions::default())
    }

    fn at(d: &Diagram, task: usize) -> (u32, u32) {
        let n = &d.nodes[d.node_of(task).unwrap()];
        (n.col, n.row)
    }

    #[test]
    fn a_chain_runs_along_one_row_and_a_branch_opens_the_next() {
        let mut p = project();
        let ids: Vec<TaskId> = (0..4).map(|i| p.add_task(format!("T{i}"), None)).collect();
        for &id in &ids {
            p.set_duration(id, DAY, false).unwrap();
        }
        // T0 -> T1 -> T2, and T0 -> T3.
        p.link_chain(&ids[0..3]).unwrap();
        p.link(ids[0].into(), ids[3], LinkType::FS, Lag::None).unwrap();
        recalculate(&mut p);
        let d = all(&p);
        assert_eq!([at(&d, 0), at(&d, 1), at(&d, 2), at(&d, 3)], [(0, 0), (1, 0), (2, 0), (1, 1)]);
        assert_eq!((d.cols, d.rows), (3, 2));
        assert_eq!(d.links.len(), 3);
    }

    #[test]
    fn a_task_sits_right_of_all_its_predecessors() {
        let mut p = project();
        let ids: Vec<TaskId> = (0..4).map(|i| p.add_task(format!("T{i}"), None)).collect();
        // T0 -> T1 -> T2 -> T3, and T0 -> T3 directly.
        p.link_chain(&ids).unwrap();
        p.link(ids[0].into(), ids[3], LinkType::SS, Lag::None).unwrap();
        recalculate(&mut p);
        let d = all(&p);
        assert_eq!(at(&d, 3), (3, 0));
        // The long link runs in the gap above the row, not through T1 and T2.
        let long = d.links.iter().find(|l| l.from == 0 && l.to == 3).unwrap();
        let pts = d.route(long);
        assert_eq!(pts.len(), 6);
        let top = d.rect(0).unwrap().y;
        assert!(pts[2].1 < top && pts[3].1 < top, "{pts:?}");
    }

    #[test]
    fn summaries_sit_left_of_their_subtasks_and_collapse_into_one_box() {
        let mut p = project();
        let s = p.add_task("Phase", None);
        let a = p.add_task("A", None);
        let b = p.add_task("B", None);
        let c = p.add_task("After", None);
        for id in [a, b] {
            p.task_mut(id).unwrap().outline_level = 2;
        }
        p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
        p.link(b.into(), c, LinkType::FS, Lag::None).unwrap();
        recalculate(&mut p);
        let d = all(&p);
        assert_eq!([at(&d, 0), at(&d, 1), at(&d, 2), at(&d, 3)], [(0, 0), (1, 0), (2, 0), (3, 0)]);
        // One outline line, to A (B follows A through its link).
        assert_eq!(d.links.iter().filter(|l| l.kind == NetLinkKind::Outline).count(), 1);

        let collapsed: HashSet<TaskId> = [s].into();
        let shown = vec![true, false, false, true];
        let d = layout(&p, &boxes_for(&p, &shown, &collapsed), NetworkOptions::default());
        assert_eq!(d.nodes.len(), 2);
        assert_eq!(d.links.len(), 1, "B -> After is drawn from the collapsed summary");
        assert_eq!(at(&d, 3), (1, 0));
        // Filtered out (not collapsed): no box and no line.
        let d = layout(&p, &boxes_for(&p, &shown, &HashSet::new()), NetworkOptions::default());
        assert!(d.links.is_empty());
    }

    #[test]
    fn critical_first_puts_the_critical_path_on_the_top_row() {
        let mut p = project();
        let short = p.add_task("Short", None);
        let long = p.add_task("Long", None);
        let end = p.add_task("End", None);
        p.set_duration(short, DAY, false).unwrap();
        p.set_duration(long, 5 * DAY, false).unwrap();
        p.link(short.into(), end, LinkType::FS, Lag::None).unwrap();
        p.link(long.into(), end, LinkType::FS, Lag::None).unwrap();
        recalculate(&mut p);
        assert!(p.task(long).unwrap().sched.critical && !p.task(short).unwrap().sched.critical);
        let d = all(&p);
        assert_eq!((at(&d, 0).1, at(&d, 1).1), (0, 1));
        let shown = vec![true; 3];
        let d = layout(&p, &boxes_for(&p, &shown, &HashSet::new()), NetworkOptions { critical_first: true, ..Default::default() });
        assert_eq!((at(&d, 1).1, at(&d, 2).1), (0, 0), "Long and End share the top row");
        assert!(d.links.iter().any(|l| l.critical));
    }

    #[test]
    fn hit_testing_finds_boxes_and_misses_gaps() {
        let mut p = project();
        let a = p.add_task("A", None);
        let b = p.add_task("B", None);
        p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
        recalculate(&mut p);
        let d = all(&p);
        let r = d.rect(1).unwrap();
        assert_eq!(d.node_at(r.x + 1.0, r.y + 1.0), Some(1));
        assert_eq!(d.node_at(r.x - 2.0, r.y + 1.0), None);
        assert_eq!(d.node_at(f32::NAN, 0.0), None);
        assert_eq!(d.node_at(-5.0, 1e30), None);
        let (w, h) = d.size();
        assert!(w >= r.right() && h >= r.bottom());
    }

    #[test]
    fn cycles_from_a_damaged_file_still_lay_out() {
        let mut p = project();
        let a = p.add_task("A", None);
        let b = p.add_task("B", None);
        p.task_mut(a).unwrap().predecessors.push(crate::model::Dependency::fs(b));
        p.task_mut(b).unwrap().predecessors.push(crate::model::Dependency::fs(a));
        let d = all(&p);
        assert_eq!(d.nodes.len(), 2);
        assert_ne!(at(&d, 0), at(&d, 1));
        for l in &d.links {
            assert!(!d.route(l).is_empty());
        }
    }

    #[test]
    fn an_empty_plan_has_an_empty_diagram() {
        let p = project();
        let d = all(&p);
        assert!(d.nodes.is_empty() && d.links.is_empty());
        assert_eq!((d.cols, d.rows), (0, 0));
        assert!(d.size().0 > 0.0);
    }
}

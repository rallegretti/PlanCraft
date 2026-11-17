//! Building a plan from a worksheet whose columns have been matched to task fields.

use std::collections::{BTreeMap, HashMap};

use crate::dates::prelude::*;

use super::{DateOrder, Field, Grid, ImportOptions};
use crate::calendar::WorkCal;
use crate::duration::Lag;
use crate::duration::{Minutes, TypedDuration};
use crate::ids::{ResourceId, TaskId};
use crate::limits::{self, MAX_LINKS_PER_TASK, MAX_MINUTES, MAX_OUTLINE_LEVEL, MAX_RESOURCES, MAX_TASKS, MAX_UNITS};
use crate::model::{
    Assignment, Constraint, ConstraintKind, Dependency, LinkType, Project, ResourceType, ScheduleMode, Task, TaskKind, TaskRef, WorkContour,
};

/// Counts repeated notes, so a thousand bad cells make one line.
#[derive(Default)]
struct Notes(BTreeMap<String, usize>);

impl Notes {
    fn add(&mut self, msg: impl Into<String>) {
        let n = self.0.entry(msg.into()).or_insert(0);
        *n = n.saturating_add(1);
    }

    fn into_vec(self) -> Vec<String> {
        self.0.into_iter().map(|(m, n)| if n > 1 { format!("{m} ({n} rows)") } else { m }).collect()
    }
}

/// Builds the plan (recalculated) and notes on what could not be read or was left out.
pub fn build(grid: &Grid, options: &ImportOptions) -> Result<(Project, Vec<String>), String> {
    let (mut p, mut notes) = crate::guard::catch("importing a spreadsheet", || build_plan(grid, options))
        .map_err(|p| format!("PlanCraft hit an internal error importing the file ({})", p.message))??;
    // Like opening a plan file: whatever the rows produced is brought into range. A date clamped
    // to the last allowed day can still move past it (an actual start set to that day's 8:00).
    let fixed = limits::sanitize(&mut p);
    if !fixed.is_empty() {
        crate::schedule::recalculate(&mut p);
        notes.extend(fixed);
    }
    Ok((p, notes))
}

fn build_plan(grid: &Grid, options: &ImportOptions) -> Result<(Project, Vec<String>), String> {
    let col = |f: Field| options.columns.iter().position(|c| *c == Some(f));
    let name_col = col(Field::Name).ok_or("choose the column that holds the task names")?;
    let get = |r: usize, f: Field| col(f).map(|c| grid.cell(r, c).trim()).filter(|s| !s.is_empty());
    let mut notes = Notes::default();

    // Rows with a name become tasks, in order.
    let rows: Vec<usize> = (0..grid.rows.len()).filter(|&r| !grid.cell(r, name_col).trim().is_empty()).take(MAX_TASKS).collect();
    if rows.is_empty() {
        return Err("no row has a task name".to_string());
    }
    for r in 0..grid.rows.len() {
        if grid.cell(r, name_col).trim().is_empty() {
            notes.add("a row without a task name was skipped");
        }
    }

    let name = options.name.trim();
    let mut p = Project::new(if name.is_empty() { "Imported plan" } else { name }, crate::dates::today());
    let order = options.date_order;
    let read_dates = |field: Field, finish: bool, notes: &mut Notes| -> Vec<Option<DateTime>> {
        rows.iter()
            .map(|&r| {
                let text = get(r, field)?;
                let date = parse_date(&p, text, order, finish);
                if date.is_none() && !is_not_applicable(text) {
                    notes.add(format!("a {} date that could not be read was left out", field.label()));
                }
                date
            })
            .collect()
    };
    let starts = read_dates(Field::Start, false, &mut notes);
    let finishes = read_dates(Field::Finish, true, &mut notes);

    // The plan starts on the earliest date in the file (today when it has none).
    let first = starts.iter().chain(finishes.iter()).flatten().min().map(|d| d.date());
    if let Some(day) = first {
        p.start = WorkCal::single(p.base_calendar()).day_start(limits::clamp_date(day));
    }

    // Tasks and the outline.
    let mpd = p.settings.time.minutes_per_day;
    let mut prev = 0u16;
    for (i, &r) in rows.iter().enumerate() {
        let id = TaskId(u32::try_from(i.saturating_add(1)).unwrap_or(u32::MAX));
        let mut task = Task::new(id, grid.cell(r, name_col).trim(), mpd);
        let level = get(r, Field::OutlineLevel)
            .and_then(|t| t.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .map(|v| v.round().clamp(1.0, f64::from(MAX_OUTLINE_LEVEL)) as u16)
            .or_else(|| get(r, Field::Wbs).map(wbs_depth))
            .unwrap_or(1);
        task.outline_level = level.clamp(1, MAX_OUTLINE_LEVEL).min(prev.saturating_add(1));
        prev = task.outline_level;
        if let Some(text) = get(r, Field::Notes) {
            task.notes = text.to_string();
        }
        p.tasks.push(task);
    }
    p.ids.task = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    let ids: Vec<TaskId> = p.tasks.iter().map(|t| t.id).collect();
    let summary: Vec<bool> = (0..ids.len()).map(|i| p.has_children(i)).collect();
    let is_summary = |i: usize| summary.get(i).copied().unwrap_or(false);

    // Durations and milestones. One working-time calendar for all rows: it keeps what it has
    // worked out about the months it has seen.
    let base = p.base_calendar().clone();
    let cal = WorkCal::single(&base);
    for (i, &r) in rows.iter().enumerate() {
        let Some(&id) = ids.get(i) else { continue };
        let milestone = get(r, Field::Milestone).is_some_and(is_yes);
        let duration = get(r, Field::Duration);
        if is_summary(i) {
            // Its duration follows from its subtasks (as the file's own value usually does).
            if milestone {
                notes.add("a summary task can't be a milestone; its subtasks set its dates");
            }
            continue;
        }
        let minutes = match duration {
            Some(text) => match parse_duration(&p, text) {
                Some(d) => Some(d),
                None => {
                    notes.add("a duration that could not be read was left at 1 day");
                    None
                }
            },
            // No duration: the working time between the start and the finish.
            None => match (starts.get(i).copied().flatten(), finishes.get(i).copied().flatten()) {
                // Durations stop at 5 years of work, which 25 years of calendar always holds on a
                // working week; counting further would only take time.
                (Some(s), Some(f)) if f >= s => {
                    let f = s.checked_add_signed(crate::dates::days(25 * 366)).map_or(f, |cap| f.min(cap));
                    Some(TypedDuration { minutes: cal.between(s, f), estimated: false, elapsed: false })
                }
                _ => None,
            },
        };
        // The tasks are new (no assignments, estimates or members), so this is what
        // `set_duration` and `set_milestone` do, without looking each task up by its id.
        let auto_milestone = p.settings.new_tasks_auto;
        let Some(t) = p.tasks.get_mut(i).filter(|t| t.id == id) else { continue };
        if let Some(typed) = minutes {
            t.duration = typed.minutes.clamp(0, MAX_MINUTES);
            t.estimated = typed.estimated;
            t.elapsed = typed.elapsed && t.duration > 0;
            if t.duration == 0 && auto_milestone {
                t.kind = TaskKind::Milestone;
            }
        }
        if milestone {
            if t.duration > 0 && minutes.is_some() {
                notes.add("a task marked as a milestone had a duration; milestones take none");
            }
            t.kind = TaskKind::Milestone;
            t.duration = 0;
            t.estimated = false;
            t.elapsed = false;
        }
    }

    links(&mut p, grid, options, &rows, &ids, &mut notes);
    resources(&mut p, grid, options, &rows, &ids, &summary, &mut notes);

    limits::check_counts(&p).map_err(|e| e.to_string())?;
    let mut sanitized = limits::sanitize(&mut p);
    let schedule = |p: &mut Project| {
        crate::schedule::try_recalculate(p).map(|_| ()).map_err(|e| format!("the plan could not be scheduled ({})", e.message))
    };
    schedule(&mut p)?;

    date_constraints(&mut p, &starts, &finishes, &summary, &mut notes, &schedule)?;

    // Progress, now that the tasks have dates (complete work starts where the task does).
    if let Some(c) = col(Field::PercentComplete) {
        let values: Vec<Option<f64>> = rows.iter().map(|&r| parse_number(grid.cell(r, c))).collect();
        // Spreadsheets store 50% as 0.5 (and 100% as 1): a column of numbers no bigger than 1,
        // none written with a %, holds fractions.
        let fractions = values.iter().flatten().all(|v| *v <= 1.0) && !rows.iter().any(|&r| grid.cell(r, c).contains('%'));
        let mut changed = false;
        for (i, value) in values.iter().enumerate() {
            let Some(v) = value else {
                if rows.get(i).is_some_and(|&r| !grid.cell(r, c).trim().is_empty()) {
                    notes.add("a % Complete that could not be read was left at 0%");
                }
                continue;
            };
            if is_summary(i) {
                // Its subtasks' values (when the file has them) give it the same figure.
                let level = p.tasks.get(i).map_or(0, |t| t.outline_level);
                let subtasks_have_values = p
                    .tasks
                    .iter()
                    .enumerate()
                    .skip(i.saturating_add(1))
                    .take_while(|(_, t)| t.outline_level > level)
                    .any(|(j, _)| values.get(j).copied().flatten().is_some());
                if !subtasks_have_values {
                    notes.add("a summary task's % Complete comes from its subtasks; the file's value was not used");
                }
                continue;
            }
            let Some(&id) = ids.get(i) else { continue };
            let pct = if fractions { v * 100.0 } else { *v };
            if p.tasks.get(i).is_some_and(|t| t.id == id) && p.set_percent_complete_at(i, pct.round().clamp(0.0, 100.0) as u8).is_ok() {
                changed = true;
            }
        }
        if changed {
            schedule(&mut p)?;
        }
    }

    // Columns nobody chose.
    let unused: Vec<&str> = grid
        .headers
        .iter()
        .enumerate()
        .filter(|(c, h)| !h.trim().is_empty() && options.columns.get(*c).copied().flatten().is_none())
        .map(|(_, h)| h.trim())
        .collect();
    let mut out = notes.into_vec();
    if !unused.is_empty() {
        let shown: Vec<&str> = unused.iter().take(10).copied().collect();
        let more = unused.len().saturating_sub(shown.len());
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        out.push(format!("these columns were not imported: {}{tail}", shown.join(", ")));
    }
    out.append(&mut sanitized);
    Ok((p, out))
}

/// Keeps the file's dates: a task (or summary) the schedule puts earlier than the file says gets
/// the file's date as a Start No Earlier Than constraint (with only a finish, Finish No Earlier
/// Than). Constraints are added in rounds, earliest tasks first: a task whose date follows from
/// one constrained in this round (through links or its summary) waits for the next round, when
/// the schedule may already put it on its date. Without the rounds a constraint early in a chain
/// would pin every later task that has a date too.
fn date_constraints(
    p: &mut Project,
    starts: &[Option<DateTime>],
    finishes: &[Option<DateTime>],
    summary: &[bool],
    notes: &mut Notes,
    schedule: &dyn Fn(&mut Project) -> Result<(), String>,
) -> Result<(), String> {
    /// Each round recalculates the plan; after this many, what is left is constrained at once.
    const ROUNDS: usize = 8;
    let n = p.tasks.len();
    let index: HashMap<TaskId, usize> = p.tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    // Edges from each task to the ones whose dates follow from it: link successors and subtasks.
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut incoming = vec![0usize; n];
    let mut stack: Vec<(usize, u16)> = Vec::new();
    for (i, t) in p.tasks.iter().enumerate() {
        while stack.last().is_some_and(|&(_, level)| level >= t.outline_level) {
            stack.pop();
        }
        let links = t.predecessors.iter().filter_map(|d| match d.predecessor {
            TaskRef::Local(id) => index.get(&id).copied(),
            TaskRef::External { .. } => None,
        });
        for f in links.chain(stack.last().map(|&(parent, _)| parent)) {
            if let Some(list) = next.get_mut(f) {
                list.push(i);
            }
            if let Some(c) = incoming.get_mut(i) {
                *c = c.saturating_add(1);
            }
        }
        stack.push((i, t.outline_level));
    }
    let date = |list: &[Option<DateTime>], i: usize| list.get(i).copied().flatten();
    let is_summary = |i: usize| summary.get(i).copied().unwrap_or(false);
    // A summary's date usually comes from its subtasks; it is the summary's own (and kept as a
    // constraint) only when no subtask starts on or before it in the file.
    let mut subtree_end = vec![n; n];
    let mut open: Vec<(usize, u16)> = Vec::new();
    for (i, t) in p.tasks.iter().enumerate() {
        while open.last().is_some_and(|&(_, level)| level >= t.outline_level) {
            if let Some((top, _)) = open.pop()
                && let Some(e) = subtree_end.get_mut(top)
            {
                *e = i;
            }
        }
        open.push((i, t.outline_level));
    }
    let own_date = |i: usize| match date(starts, i) {
        Some(s) if is_summary(i) => {
            let end = subtree_end.get(i).copied().unwrap_or(n);
            !(i.saturating_add(1)..end).any(|j| date(starts, j).is_some_and(|d| d <= s))
        }
        _ => true,
    };
    for round in 0..ROUNDS {
        let wanted: Vec<Option<Constraint>> = (0..n)
            .map(|i| {
                let t = p.tasks.get(i)?;
                if t.mode != ScheduleMode::Auto || t.constraint.kind != ConstraintKind::Asap || !own_date(i) {
                    return None;
                }
                match (date(starts, i), t.sched.start, date(finishes, i), t.sched.finish) {
                    (Some(s), Some(at), _, _) if s > at => Some(Constraint { kind: ConstraintKind::Snet, date: Some(s) }),
                    (None, _, Some(f), Some(at)) if f > at && !is_summary(i) => {
                        Some(Constraint { kind: ConstraintKind::Fnet, date: Some(f) })
                    }
                    _ => None,
                }
            })
            .collect();
        let is_wanted = |i: usize| wanted.get(i).is_some_and(Option::is_some);
        if !(0..n).any(is_wanted) {
            break;
        }
        // Which wanted tasks follow from another wanted task (in link and outline order; a task
        // in a loop of links is never reached and counts as following).
        let mut follows = vec![false; n];
        let mut reached = vec![false; n];
        let mut left = incoming.clone();
        let mut ready: Vec<usize> = (0..n).filter(|&i| left.get(i) == Some(&0)).collect();
        while let Some(i) = ready.pop() {
            if let Some(r) = reached.get_mut(i) {
                *r = true;
            }
            let passes = is_wanted(i) || follows.get(i).copied().unwrap_or(false);
            for &s in next.get(i).map_or(&[][..], Vec::as_slice) {
                if passes && let Some(f) = follows.get_mut(s) {
                    *f = true;
                }
                if let Some(l) = left.get_mut(s) {
                    *l = l.saturating_sub(1);
                    if *l == 0 {
                        ready.push(s);
                    }
                }
            }
        }
        let last = round.saturating_add(1) == ROUNDS;
        let free = |i: usize| reached.get(i).copied().unwrap_or(false) && !follows.get(i).copied().unwrap_or(true);
        let mut chosen: Vec<usize> = (0..n).filter(|&i| is_wanted(i) && (last || free(i))).collect();
        if chosen.is_empty() {
            // Only tasks caught in loops of links are left: constrain them all.
            chosen = (0..n).filter(|&i| is_wanted(i)).collect();
        }
        for i in chosen {
            let (Some(Some(c)), Some(t)) = (wanted.get(i), p.tasks.get_mut(i)) else { continue };
            t.constraint = *c;
            notes.add(match c.kind {
                ConstraintKind::Fnet => "a task kept its finish date from the file as a Finish No Earlier Than constraint",
                _ => "a task kept its start date from the file as a Start No Earlier Than constraint",
            });
        }
        schedule(p)?;
    }
    for (i, t) in p.tasks.iter().enumerate() {
        if let (Some(s), Some(at)) = (date(starts, i), t.sched.start)
            && s < at
            && !is_summary(i)
        {
            notes.add("a task starts later than the file says, after its predecessors");
        }
        if is_summary(i) && date(starts, i).is_none() && date(finishes, i).is_some() {
            notes.add("a summary task's finish date was not used; its subtasks set it");
        }
    }
    Ok(())
}

/// Links from the Predecessors column. A link names its predecessor by the ID column (or, with
/// none, the task's row number among the imported tasks), as in `3`, `3FS+2d`, `4SS-1 day`.
fn links(p: &mut Project, grid: &Grid, options: &ImportOptions, rows: &[usize], ids: &[TaskId], notes: &mut Notes) {
    let col = |f: Field| options.columns.iter().position(|c| *c == Some(f));
    let Some(pred_col) = col(Field::Predecessors) else { return };
    let mut keys: HashMap<String, usize> = HashMap::new();
    for (i, &r) in rows.iter().enumerate() {
        let key = match col(Field::Id) {
            Some(c) => normalize_key(grid.cell(r, c)),
            None => i.saturating_add(1).to_string(),
        };
        if key.is_empty() {
            continue;
        }
        match keys.entry(key) {
            std::collections::hash_map::Entry::Occupied(_) => notes.add("two rows have the same ID; links name the first of them"),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(i);
            }
        }
    }
    // Where each row's subtree ends (exclusive): a summary can't link to its own subtasks.
    let mut end = vec![rows.len(); rows.len()];
    let mut stack: Vec<(usize, u16)> = Vec::new();
    for (i, t) in p.tasks.iter().enumerate() {
        while let Some(&(top, level)) = stack.last() {
            if level >= t.outline_level {
                if let Some(e) = end.get_mut(top) {
                    *e = i;
                }
                stack.pop();
            } else {
                break;
            }
        }
        stack.push((i, t.outline_level));
    }
    let within = |outer: usize, inner: usize| outer < inner && end.get(outer).is_some_and(|e| inner < *e);
    let time = p.settings.time.clone();
    for (i, &r) in rows.iter().enumerate() {
        let text = grid.cell(r, pred_col);
        for part in text.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
            if part.contains('\\') {
                notes.add("a link to a task in another project was left out");
                continue;
            }
            let Some((pi, rest)) = link_target(part, &keys) else {
                notes.add("a link to a task that is not in the file was left out");
                continue;
            };
            let (kind, lag_text) = match (rest.get(..2).and_then(LinkType::parse), rest.get(2..)) {
                (Some(kind), Some(lag)) => (kind, lag.trim()),
                _ => (LinkType::FS, rest),
            };
            let Some(lag) = Lag::parse(lag_text, &time) else {
                notes.add("a link whose lag could not be read was left out");
                continue;
            };
            if pi == i || within(pi, i) || within(i, pi) {
                notes.add("a link between a task and itself or its own summary was left out");
                continue;
            }
            let (Some(&pred), Some(task)) = (ids.get(pi), p.tasks.get_mut(i)) else { continue };
            if task.predecessors.iter().any(|d| d.predecessor == TaskRef::Local(pred)) {
                continue;
            }
            if task.predecessors.len() >= MAX_LINKS_PER_TASK {
                notes.add(format!("a task can have at most {MAX_LINKS_PER_TASK} predecessors; the rest were left out"));
                break;
            }
            task.predecessors.push(Dependency { predecessor: TaskRef::Local(pred), kind, lag });
        }
    }
}

/// Assignments from the Resource Names column: names separated by commas or semicolons, each
/// with optional units in brackets (`Ana[50%]`). A new name makes a new work resource.
fn resources(p: &mut Project, grid: &Grid, options: &ImportOptions, rows: &[usize], ids: &[TaskId], summary: &[bool], notes: &mut Notes) {
    let Some(res_col) = options.columns.iter().position(|c| *c == Some(Field::Resources)) else { return };
    let mut by_name: HashMap<String, ResourceId> = HashMap::new();
    for (i, &r) in rows.iter().enumerate() {
        let text = grid.cell(r, res_col).trim();
        if text.is_empty() {
            continue;
        }
        if summary.get(i).copied().unwrap_or(false) {
            notes.add("resources on summary tasks were left out; assign them to the subtasks");
            continue;
        }
        let Some(&task) = ids.get(i) else { continue };
        let Some(duration) = p.tasks.get(i).filter(|t| t.id == task).map(|t| t.duration) else { continue };
        let mut assigned: Vec<ResourceId> = Vec::new();
        for part in text.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
            // `Ana`, `Ana[50%]`, `Ana[0.5]`, or a material with its amount: `Cement[5 bags]`.
            let (name, units, material) = match part.split_once('[') {
                Some((n, rest)) => {
                    let inner = rest.trim_end_matches(']').trim();
                    let number_end = inner.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(inner.len());
                    let (number, label) = (inner.get(..number_end).unwrap_or(""), inner.get(number_end..).unwrap_or("").trim());
                    let value = parse_number(number).unwrap_or(100.0);
                    match label {
                        "%" => (n.trim(), value / 100.0, None),
                        "" => (n.trim(), if value > 10.0 { value / 100.0 } else { value }, None),
                        label => (n.trim(), value, Some(label)),
                    }
                }
                None => (part, 1.0, None),
            };
            if name.is_empty() {
                continue;
            }
            let key = name.to_lowercase();
            let rid = match by_name.get(&key) {
                Some(&rid) => rid,
                None if p.resources.len() >= MAX_RESOURCES => {
                    notes.add(format!("a plan can have at most {MAX_RESOURCES} resources; the rest were left out"));
                    continue;
                }
                None => {
                    let rid = p.add_resource(name);
                    if let (Some(label), Some(r)) = (material, p.resources.last_mut().filter(|r| r.id == rid)) {
                        r.kind = ResourceType::Material;
                        r.material_label = label.to_string();
                    }
                    by_name.insert(key, rid);
                    rid
                }
            };
            let is_work = p.resources.iter().rev().find(|r| r.id == rid).is_none_or(|r| r.kind == ResourceType::Work);
            if assigned.contains(&rid) {
                continue;
            }
            if p.assignments.len() >= limits::MAX_ASSIGNMENTS {
                notes.add(format!("a plan can have at most {} assignments; the rest were left out", limits::MAX_ASSIGNMENTS));
                break;
            }
            assigned.push(rid);
            let units = if units.is_finite() && units > 0.0 { units.min(MAX_UNITS) } else { 1.0 };
            // What `assign` does for a new resource on a new Fixed Units task that is not effort
            // driven (every imported task): a work resource works the duration at its units, a
            // material one has no work. Done directly, as `assign` looks up the task and its
            // assignments in the whole plan.
            let id = p.ids.next_assignment();
            p.assignments.push(Assignment {
                id,
                task,
                resource: rid,
                units,
                work: if is_work { limits::clamp_minutes((duration as f64 * units).round() as Minutes) } else { 0 },
                actual_work: 0,
                cost: 0.0,
                contour: WorkContour::Flat,
            });
        }
    }
}

/// The task a link names, and the rest of the link (type and lag). The task's key is the longest
/// start of `part` that is a known ID (`T12FS+1d` names `T12`, not `T1`), and what follows it
/// must be empty or start a link type or lag, so `10` never quietly links to task `1`.
fn link_target<'a>(part: &'a str, keys: &HashMap<String, usize>) -> Option<(usize, &'a str)> {
    /// Longer text is not a link (IDs are short); it is reported, not searched.
    const LONGEST: usize = 256;
    if part.len() > LONGEST {
        return None;
    }
    let ends: Vec<usize> = part.char_indices().map(|(i, _)| i).skip(1).chain(std::iter::once(part.len())).collect();
    ends.into_iter().rev().find_map(|end| {
        let (key, rest) = (part.get(..end)?, part.get(end..)?.trim());
        let i = *keys.get(&normalize_key(key))?;
        let starts_link = rest.is_empty() || rest.starts_with(['+', '-']) || rest.get(..2).and_then(LinkType::parse).is_some();
        starts_link.then_some((i, rest))
    })
}

/// A key for matching IDs: numbers without leading zeros (`007` and `7` match), other text as is.
fn normalize_key(text: &str) -> String {
    let t = text.trim();
    match t.parse::<u64>() {
        Ok(n) => n.to_string(),
        Err(_) => t.to_string(),
    }
}

/// The outline depth of a WBS code: `1.2.3` is level 3.
fn wbs_depth(code: &str) -> u16 {
    let parts = code.split('.').filter(|s| !s.trim().is_empty()).count();
    u16::try_from(parts.max(1)).unwrap_or(MAX_OUTLINE_LEVEL).min(MAX_OUTLINE_LEVEL)
}

fn is_yes(text: &str) -> bool {
    matches!(text.trim().to_lowercase().as_str(), "yes" | "y" | "true" | "1" | "x" | "✓" | "✔")
}

/// Text spreadsheets put in a date cell that has no date.
fn is_not_applicable(text: &str) -> bool {
    matches!(text.trim().to_lowercase().as_str(), "na" | "n/a" | "-" | "none" | "tbd")
}

/// A number written with a decimal point or a decimal comma (`2.5`, `2,5`), with an optional `%`.
fn parse_number(text: &str) -> Option<f64> {
    let t = text.trim().trim_end_matches('%').trim();
    let t = if t.contains(',') && !t.contains('.') { t.replace(',', ".") } else { t.to_string() };
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A duration in the plan's units (`5`, `5d`, `3 days`, `2w`, `36h`, `2,5 days`), or elapsed time
/// (`33ed`, `33 edays`).
fn parse_duration(p: &Project, text: &str) -> Option<TypedDuration> {
    p.parse_task_duration(text).or_else(|| {
        // A decimal comma.
        (text.contains(',') && !text.contains('.')).then(|| p.parse_task_duration(&text.replace(',', "."))).flatten()
    })
}

/// The numbers of a date written as three numbers (`03/04/2026`, `3.4.26`, `03-04-2026`), when the
/// year comes last. `None` for anything else (an ISO date starts with its year).
pub(crate) fn numeric_date_parts(text: &str) -> Option<(u32, u32, i32)> {
    let date = text.trim().split([' ', 'T']).next()?;
    let mut parts = date.split(['/', '.', '-']);
    let (a, b, c) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || a.len() > 2 || b.len() > 2 || !(c.len() == 2 || c.len() == 4) {
        return None;
    }
    let year: i32 = c.parse().ok()?;
    let year = if c.len() == 2 { 2000 + year } else { year };
    Some((a.parse().ok()?, b.parse().ok()?, year))
}

/// A weekday's name or its abbreviation (`Mon`, `Tues`, `Thursday`).
fn is_weekday(word: &str) -> bool {
    const DAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    let w = word.to_lowercase();
    w.chars().count() >= 2 && DAYS.iter().any(|d| d.starts_with(w.as_str()))
}

/// A time of day after a date (`13:30`, `1:30 PM`).
fn time_of_day(text: &str) -> Option<Time> {
    let t = text.trim().to_uppercase();
    ["%H:%M", "%H:%M:%S", "%I:%M %p", "%I:%M:%S %p", "%I:%M%p"].iter().find_map(|f| Time::parse_from_str(&t, f).ok())
}

/// Reads a date, with `order` for dates written as three numbers. A date without a time starts
/// at the start of the working day (or, for a finish, ends at its end).
pub(crate) fn parse_date(p: &Project, text: &str, order: DateOrder, finish: bool) -> Option<DateTime> {
    let mut t = text.trim();
    // A weekday in front, as Microsoft Project writes dates ("Mon 10/19/26", "Monday, 19 Oct 2026").
    if let Some((word, rest)) = t.split_once(' ')
        && is_weekday(word.trim_end_matches([',', '.']))
    {
        t = rest.trim();
    }
    let found = if let Some((a, b, year)) = numeric_date_parts(t) {
        let (month, day) = match order {
            DateOrder::MonthFirst => (a, b),
            DateOrder::DayFirst => (b, a),
        };
        let date = crate::dates::ymd(year, month, day)?;
        let time = t.split_once([' ', 'T']).and_then(|(_, rest)| time_of_day(rest));
        Some(match time {
            Some(time) => date.and_time(time),
            None => {
                let wc = WorkCal::single(p.base_calendar());
                if finish { wc.day_finish(date) } else { wc.day_start(date) }
            }
        })
    } else {
        // Month names written out ("October 19, 2026", "19 October 2026"), then the forms people
        // type in PlanCraft.
        ["%B %d, %Y", "%B %d %Y", "%d %B %Y", "%d %B, %Y"]
            .iter()
            .find_map(|f| Date::parse_from_str(t, f).ok())
            .map(|date| {
                let wc = WorkCal::single(p.base_calendar());
                if finish { wc.day_finish(date) } else { wc.day_start(date) }
            })
            .or_else(|| p.parse_datetime(t, finish))
    };
    found.map(limits::clamp_datetime)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(text: &str) -> Grid {
        super::super::csv::read(text, &mut super::super::Limits::default())
    }

    fn import(text: &str) -> (Project, Vec<String>) {
        let g = grid(text);
        build(&g, &ImportOptions::guess(&g, "Imported")).unwrap()
    }

    fn task<'a>(p: &'a Project, name: &str) -> &'a Task {
        p.tasks.iter().find(|t| t.name == name).unwrap()
    }

    /// Regression (found by the hostile sweep): a start in the year 3000 was clamped to the last
    /// allowed day, then progress set the actual start to that day's 8:00, past the limit.
    #[test]
    fn an_imported_plan_is_always_in_range() {
        // The case the sweep found: a "yes" in the % Complete column, a start in the year 3000.
        let g = grid("m\n0,1,0\n0,yes,0,\\,1-1-3000\n");
        let options = ImportOptions {
            columns: vec![None, Some(Field::PercentComplete), Some(Field::Name), None, Some(Field::Start)],
            date_order: DateOrder::DayFirst,
            name: "x".into(),
        };
        let (p, _) = build(&g, &options).unwrap();
        assert!(p.tasks.iter().any(|t| t.actual_start.is_some()), "the case sets an actual start");
        let mut copy = p.clone();
        assert_eq!(limits::sanitize(&mut copy), Vec::<String>::new());
    }

    fn day(y: i32, m: u32, d: u32) -> Date {
        crate::dates::ymd(y, m, d).unwrap()
    }

    #[test]
    fn a_simple_list_becomes_linked_tasks_with_resources() {
        let (p, notes) = import(
            "Task,Duration,Start,Predecessors,Resources\n\
             Design,3d,2026-10-19,,Ana\n\
             Build,5 days,,1,\"Ana[50%], Ben\"\n\
             Launch,0,,2FS+1d,\n",
        );
        assert_eq!(p.tasks.len(), 3, "{notes:?}");
        assert_eq!(p.start.date(), day(2026, 10, 19));
        let build = task(&p, "Build");
        assert_eq!(build.duration, 5 * 480);
        assert_eq!(build.predecessors.len(), 1);
        assert_eq!(build.sched.start.unwrap().date(), day(2026, 10, 22));
        assert!(task(&p, "Launch").is_milestone());
        assert_eq!(task(&p, "Launch").predecessors.first().unwrap().kind, LinkType::FS);
        assert_eq!(p.resources.len(), 2);
        assert_eq!(p.format_resource_names(build.id), "Ana[50%],Ben");
    }

    /// Imported assignments are what typing the same resource names in the app gives.
    #[test]
    fn assignments_match_typing_the_resource_names() {
        let (p, _) = import("Name,Duration,Resources\nA,3d,\"Ana[50%], Ben\"\nB,0,Ana\n");
        let mut typed = Project::new("x", day(2026, 10, 19));
        for (name, d) in [("A", 3 * 480), ("B", 0)] {
            let id = typed.add_task(name, None);
            typed.set_duration(id, d, false).unwrap();
        }
        typed.apply_resource_names(typed.tasks[0].id, "Ana[50%], Ben").unwrap();
        typed.apply_resource_names(typed.tasks[1].id, "Ana").unwrap();
        let shape = |p: &Project| p.assignments.iter().map(|a| (a.units, a.work)).collect::<Vec<_>>();
        assert_eq!(shape(&p), shape(&typed));
        assert!(task(&p, "B").is_milestone());
    }

    #[test]
    fn outline_levels_and_wbs_codes_make_summaries() {
        let (p, _) = import("WBS,Name,Duration\n1,Phase,\n1.1,Step A,2d\n1.2,Step B,3d\n2,Wrap up,1d\n");
        assert_eq!(p.tasks.iter().map(|t| t.outline_level).collect::<Vec<_>>(), [1, 2, 2, 1]);
        assert!(p.has_children(0));
        let (p, _) = import("Name,Outline Level\nTop,1\nDeep,5\n");
        assert_eq!(p.tasks.iter().map(|t| t.outline_level).collect::<Vec<_>>(), [1, 2], "a level never jumps by more than one");
    }

    #[test]
    fn later_start_dates_become_constraints_and_earlier_ones_are_reported() {
        let (p, notes) =
            import("Name,Duration,Start,Predecessors\nA,2d,2026-10-19,\nB,1d,2026-10-26,\nC,1d,2026-10-19,1\nD,1d,2026-11-02,1\n");
        assert_eq!(task(&p, "A").constraint.kind, ConstraintKind::Asap, "the earliest date is the plan's start");
        assert_eq!(task(&p, "B").constraint.kind, ConstraintKind::Snet);
        assert_eq!(task(&p, "B").sched.start.unwrap().date(), day(2026, 10, 26));
        assert_eq!(task(&p, "C").sched.start.unwrap().date(), day(2026, 10, 21));
        assert!(notes.iter().any(|n| n.contains("starts later than the file says")), "{notes:?}");
        assert_eq!(task(&p, "D").constraint.kind, ConstraintKind::Snet);
    }

    #[test]
    fn a_start_and_finish_without_a_duration_give_the_working_time_between() {
        let (p, _) = import("Name,Start,Finish\nA,2026-10-19,2026-10-23\n");
        assert_eq!(task(&p, "A").duration, 5 * 480);
    }

    #[test]
    fn dates_follow_the_chosen_order() {
        let g = grid("Name,Start\nA,03/04/2026\n");
        let mut options = ImportOptions::guess(&g, "x");
        options.date_order = DateOrder::DayFirst;
        let (p, _) = build(&g, &options).unwrap();
        assert_eq!(p.start.date(), day(2026, 4, 3));
        options.date_order = DateOrder::MonthFirst;
        let (p, _) = build(&g, &options).unwrap();
        assert_eq!(p.start.date(), day(2026, 3, 4));
        let (p, _) = import("Name,Start\nA,Mon 10/19/26\n");
        assert_eq!(p.start.date(), day(2026, 10, 19));
    }

    #[test]
    fn predecessors_follow_the_id_column_when_there_is_one() {
        let (p, notes) = import("ID,Name,Predecessors\n10,A,\n20,B,10\n30,C,\"20, 99\"\n");
        assert_eq!(task(&p, "B").predecessors.first().map(|d| d.predecessor), Some(TaskRef::Local(task(&p, "A").id)));
        assert_eq!(task(&p, "C").predecessors.len(), 1);
        assert!(notes.iter().any(|n| n.contains("not in the file")), "{notes:?}");
    }

    #[test]
    fn progress_reads_percentages_and_fractions() {
        let (p, _) = import("Name,Duration,Start,% Complete\nA,2d,2026-10-19,50%\nB,2d,2026-10-19,100\n");
        assert_eq!((task(&p, "A").percent_complete, task(&p, "B").percent_complete), (50, 100));
        let (p, _) = import("Name,Duration,Progress\nA,2d,0.25\nB,2d,1\n");
        assert_eq!((task(&p, "A").percent_complete, task(&p, "B").percent_complete), (25, 100));
    }

    #[test]
    fn bad_cells_are_reported_not_fatal() {
        let (p, notes) = import("Name,Duration,Start,Predecessors,Extra\nA,soon,someday,X\n,3d\nB,2d,,1SS+what,\n");
        assert_eq!(p.tasks.len(), 2);
        for expected in [
            "duration that could not be read",
            "Start date that could not be read",
            "not in the file",
            "lag",
            "without a task name",
            "not imported: Extra",
        ] {
            assert!(notes.iter().any(|n| n.contains(expected)), "missing note '{expected}' in {notes:?}");
        }
    }

    #[test]
    fn links_inside_an_outline_and_to_itself_are_left_out() {
        let (p, notes) = import("Name,Outline Level,Predecessors\nPhase,1,2\nStep,2,\nOther,1,3\n");
        assert!(p.tasks.iter().all(|t| t.predecessors.is_empty()));
        assert!(notes.iter().any(|n| n.contains("itself or its own summary")), "{notes:?}");
    }

    /// Regression (review): a constraint early in a chain pinned every later task with a date.
    #[test]
    fn only_the_first_task_of_a_chain_is_constrained() {
        let (p, _) = import("Name,Duration,Start,Predecessors\nA,1d,2026-10-19,\nB,1d,2026-10-26,\nC,1d,2026-10-27,2\nD,1d,2026-10-28,3\n");
        let kinds: Vec<ConstraintKind> = p.tasks.iter().map(|t| t.constraint.kind).collect();
        assert_eq!(kinds, [ConstraintKind::Asap, ConstraintKind::Snet, ConstraintKind::Asap, ConstraintKind::Asap]);
        assert_eq!(task(&p, "D").sched.start.unwrap().date(), day(2026, 10, 28));
    }

    /// Regression (review): a summary row's start date was dropped, so its subtasks started on the
    /// plan's first day.
    #[test]
    fn a_summary_rows_start_date_holds_its_subtasks() {
        let (p, notes) =
            import("Name,Outline Level,Duration,Start,% Complete\nPhase 1,1,,2026-10-19,\nA,2,2d,,\nPhase 2,1,,2026-11-02,50\nB,2,3d,,\n");
        assert_eq!(task(&p, "Phase 2").constraint.kind, ConstraintKind::Snet);
        assert_eq!(task(&p, "B").sched.start.unwrap().date(), day(2026, 11, 2));
        assert_eq!(task(&p, "B").constraint.kind, ConstraintKind::Asap);
        assert!(notes.iter().any(|n| n.contains("summary task's % Complete")), "{notes:?}");
    }

    /// Regression (review): "Oct 19, 2026" lost its month to the weekday rule.
    #[test]
    fn month_names_and_weekdays_are_read() {
        for text in ["Oct 19, 2026", "October 19, 2026", "Mon 10/19/26", "Monday, 19 Oct 2026", "19 October 2026"] {
            let (p, _) = import(&format!("Name,Start\nA,\"{text}\"\n"));
            assert_eq!(p.start.date(), day(2026, 10, 19), "{text}");
        }
    }

    /// Regression (review): IDs that are not numbers (T1, T2) never linked.
    #[test]
    fn ids_that_are_not_numbers_link() {
        let (p, notes) = import("ID,Name,Predecessors\nT1,A,\nT2,B,T1\nT12,C,\"T2FS+1d, T1 SS\"\nT3,D,10\n");
        assert_eq!(task(&p, "B").predecessors.len(), 1);
        let c = task(&p, "C");
        assert_eq!(c.predecessors.len(), 2);
        assert_eq!(c.predecessors[0].predecessor, TaskRef::Local(task(&p, "B").id));
        assert_eq!(c.predecessors[1].kind, LinkType::SS);
        assert!(task(&p, "D").predecessors.is_empty(), "10 is not a key, and never task 1");
        assert!(notes.iter().any(|n| n.contains("not in the file")), "{notes:?}");
    }

    /// Regression (review): a workbook's 100% arrives as 1 and was read as 1%.
    #[test]
    fn a_column_of_zeros_and_ones_is_read_as_fractions() {
        let (p, _) = import("Name,Duration,Start,% Complete\nA,1d,2026-10-19,1\nB,1d,2026-10-19,0\n");
        assert_eq!((task(&p, "A").percent_complete, task(&p, "B").percent_complete), (100, 0));
    }

    /// Regression (review): a material amount made a work resource at 500%.
    #[test]
    fn material_amounts_make_material_resources() {
        let (p, _) = import("Name,Duration,Resources\nPour,2d,\"Cement[5 bags], Ana\"\n");
        let cement = p.resources.iter().find(|r| r.name == "Cement").unwrap();
        assert_eq!(cement.kind, ResourceType::Material);
        assert_eq!(cement.material_label, "bags");
        assert_eq!(p.format_resource_names(task(&p, "Pour").id), "Cement[5 bags],Ana");
        let a = p.assignments.iter().find(|a| a.resource == cement.id).unwrap();
        assert_eq!((a.units, a.work), (5.0, 0));
    }

    /// Regression (review): each row built its own working-time calendar and counted 400 years
    /// of it, about 5 ms a row. (Scheduling tasks 5 years long takes its own time, as it does
    /// when such a plan is opened, so the test keeps the rows few.)
    #[test]
    fn long_date_spans_are_quick() {
        let rows: String = (0..300).map(|i| format!("T{i},1900-01-01,2300-12-31\n")).collect();
        let t = std::time::Instant::now();
        let (p, _) = import(&format!("Name,Start,Finish\n{rows}"));
        assert_eq!(p.tasks.len(), 300);
        assert!(p.tasks.iter().all(|t| t.duration <= MAX_MINUTES));
        let limit = std::time::Duration::from_secs(if cfg!(debug_assertions) { 20 } else { 2 });
        assert!(t.elapsed() < limit, "took {:?}", t.elapsed());
    }

    #[test]
    fn without_a_name_column_nothing_is_built() {
        let g = grid("Duration\n3d\n");
        assert!(build(&g, &ImportOptions::guess(&g, "x")).is_err());
        let g = grid("Name\n\n");
        assert!(build(&g, &ImportOptions::guess(&g, "x")).is_err());
    }
}

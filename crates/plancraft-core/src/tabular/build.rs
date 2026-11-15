//! Building a plan from a worksheet whose columns have been matched to task fields.

use std::collections::{BTreeMap, HashMap};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use super::{DateOrder, Field, Grid, ImportOptions};
use crate::calendar::WorkCal;
use crate::duration::Lag;
use crate::duration::Minutes;
use crate::ids::{ResourceId, TaskId};
use crate::limits::{self, MAX_LINKS_PER_TASK, MAX_MINUTES, MAX_OUTLINE_LEVEL, MAX_RESOURCES, MAX_TASKS, MAX_UNITS};
use crate::model::{
    Assignment, Constraint, ConstraintKind, Dependency, LinkType, Project, ScheduleMode, Task, TaskKind, TaskRef, WorkContour,
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
    crate::guard::catch("importing a spreadsheet", || build_plan(grid, options))
        .map_err(|p| format!("PlanCraft hit an internal error importing the file ({})", p.message))?
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
    let mut p = Project::new(if name.is_empty() { "Imported plan" } else { name }, chrono::Local::now().date_naive());
    let order = options.date_order;
    let read_dates = |field: Field, finish: bool, notes: &mut Notes| -> Vec<Option<NaiveDateTime>> {
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

    // Durations and milestones.
    for (i, &r) in rows.iter().enumerate() {
        let Some(&id) = ids.get(i) else { continue };
        let milestone = get(r, Field::Milestone).is_some_and(is_yes);
        let duration = get(r, Field::Duration);
        if is_summary(i) {
            if milestone || duration.is_some() {
                notes.add("summary tasks take their duration from their subtasks; the file's value was not used");
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
                (Some(s), Some(f)) if f >= s => Some((WorkCal::single(p.base_calendar()).between(s, f), false)),
                _ => None,
            },
        };
        // The tasks are new (no assignments, estimates or members), so this is what
        // `set_duration` and `set_milestone` do, without looking each task up by its id.
        let auto_milestone = p.settings.new_tasks_auto;
        let Some(t) = p.tasks.get_mut(i).filter(|t| t.id == id) else { continue };
        if let Some((m, estimated)) = minutes {
            t.duration = m.clamp(0, MAX_MINUTES);
            t.estimated = estimated;
            if t.duration == 0 && auto_milestone {
                t.kind = TaskKind::Milestone;
            }
        }
        if milestone {
            t.kind = TaskKind::Milestone;
            t.duration = 0;
            t.estimated = false;
        }
    }

    links(&mut p, grid, options, &rows, &ids, &mut notes);
    resources(&mut p, grid, options, &rows, &ids, &summary, &mut notes);

    limits::check_counts(&p).map_err(|e| e.to_string())?;
    let mut sanitized = limits::sanitize(&mut p);
    let schedule =
        |p: &mut Project| crate::schedule::try_recalculate(p).map_err(|e| format!("the plan could not be scheduled ({})", e.message));
    schedule(&mut p)?;

    // The file's dates: a task the schedule puts earlier than the file says keeps the file's date
    // as a Start No Earlier Than (or, with only a finish, Finish No Earlier Than) constraint.
    let mut constrained = false;
    for i in 0..ids.len() {
        if is_summary(i) {
            continue;
        }
        let Some(t) = p.tasks.get_mut(i) else { continue };
        if t.mode != ScheduleMode::Auto {
            continue;
        }
        let (start, finish) = (starts.get(i).copied().flatten(), finishes.get(i).copied().flatten());
        match (start, t.sched.start, finish, t.sched.finish) {
            (Some(s), Some(scheduled), _, _) if s > scheduled => {
                t.constraint = Constraint { kind: ConstraintKind::Snet, date: Some(s) };
                constrained = true;
                notes.add("a task kept its start date from the file as a Start No Earlier Than constraint");
            }
            (Some(s), Some(scheduled), _, _) if s < scheduled => {
                notes.add("a task starts later than the file says, after its predecessors");
            }
            (None, _, Some(f), Some(scheduled)) if f > scheduled => {
                t.constraint = Constraint { kind: ConstraintKind::Fnet, date: Some(f) };
                constrained = true;
                notes.add("a task kept its finish date from the file as a Finish No Earlier Than constraint");
            }
            _ => {}
        }
    }
    if constrained {
        schedule(&mut p)?;
    }

    // Progress, now that the tasks have dates (complete work starts where the task does).
    if let Some(c) = col(Field::PercentComplete) {
        let values: Vec<Option<f64>> = rows.iter().map(|&r| parse_number(grid.cell(r, c))).collect();
        // Spreadsheets store 50% as 0.5: a column of fractions up to 1 is read as fractions.
        let fractions = values.iter().flatten().all(|v| *v <= 1.0)
            && values.iter().flatten().any(|v| v.fract() != 0.0)
            && !rows.iter().any(|&r| grid.cell(r, c).contains('%'));
        let mut changed = false;
        for (i, value) in values.iter().enumerate() {
            let Some(v) = value else {
                if rows.get(i).is_some_and(|&r| !grid.cell(r, c).trim().is_empty()) {
                    notes.add("a % Complete that could not be read was left at 0%");
                }
                continue;
            };
            let Some(&id) = ids.get(i).filter(|_| !is_summary(i)) else { continue };
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
            let digits_end = part.find(|c: char| !c.is_ascii_digit()).unwrap_or(part.len());
            let (digits, rest) = part.split_at(digits_end);
            let Some(&pi) = keys.get(&normalize_key(digits)) else {
                notes.add("a link to a task that is not in the file was left out");
                continue;
            };
            let rest = rest.trim();
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
            let (name, units) = match part.split_once('[') {
                Some((n, rest)) => {
                    let inner = rest.trim_end_matches(']').trim();
                    let value = parse_number(inner.trim_end_matches('%')).unwrap_or(100.0);
                    let units = if inner.contains('%') || value > 10.0 { value / 100.0 } else { value };
                    (n.trim(), units)
                }
                None => (part, 1.0),
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
                    by_name.insert(key, rid);
                    rid
                }
            };
            if assigned.contains(&rid) {
                continue;
            }
            if p.assignments.len() >= limits::MAX_ASSIGNMENTS {
                notes.add(format!("a plan can have at most {} assignments; the rest were left out", limits::MAX_ASSIGNMENTS));
                break;
            }
            assigned.push(rid);
            let units = if units.is_finite() && units > 0.0 { units.min(MAX_UNITS) } else { 1.0 };
            // What `assign` does for a new work resource on a new Fixed Units task that is not
            // effort driven (every imported task): work is the duration at the units. Done
            // directly, as `assign` looks up the task and its assignments in the whole plan.
            let id = p.ids.next_assignment();
            p.assignments.push(Assignment {
                id,
                task,
                resource: rid,
                units,
                work: limits::clamp_minutes((duration as f64 * units).round() as Minutes),
                actual_work: 0,
                cost: 0.0,
                contour: WorkContour::Flat,
            });
        }
    }
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

/// A duration in the plan's units (`5`, `5d`, `3 days`, `2w`, `36h`, `2,5 days`).
fn parse_duration(p: &Project, text: &str) -> Option<(Minutes, bool)> {
    p.parse_duration(text).or_else(|| {
        // A decimal comma.
        (text.contains(',') && !text.contains('.')).then(|| p.parse_duration(&text.replace(',', "."))).flatten()
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

/// A time of day after a date (`13:30`, `1:30 PM`).
fn time_of_day(text: &str) -> Option<NaiveTime> {
    let t = text.trim().to_uppercase();
    ["%H:%M", "%H:%M:%S", "%I:%M %p", "%I:%M:%S %p", "%I:%M%p"].iter().find_map(|f| NaiveTime::parse_from_str(&t, f).ok())
}

/// Reads a date, with `order` for dates written as three numbers. A date without a time starts
/// at the start of the working day (or, for a finish, ends at its end).
pub(crate) fn parse_date(p: &Project, text: &str, order: DateOrder, finish: bool) -> Option<NaiveDateTime> {
    let mut t = text.trim();
    // A weekday in front, as Microsoft Project writes dates ("Mon 10/19/26").
    if let Some((word, rest)) = t.split_once(' ')
        && word.chars().all(char::is_alphabetic)
        && rest.trim_start().starts_with(|c: char| c.is_ascii_digit())
    {
        t = rest.trim();
    }
    let found = if let Some((a, b, year)) = numeric_date_parts(t) {
        let (month, day) = match order {
            DateOrder::MonthFirst => (a, b),
            DateOrder::DayFirst => (b, a),
        };
        let date = NaiveDate::from_ymd_opt(year, month, day)?;
        let time = t.split_once([' ', 'T']).and_then(|(_, rest)| time_of_day(rest));
        Some(match time {
            Some(time) => date.and_time(time),
            None => {
                let wc = WorkCal::single(p.base_calendar());
                if finish { wc.day_finish(date) } else { wc.day_start(date) }
            }
        })
    } else {
        p.parse_datetime(t, finish)
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

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
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

    #[test]
    fn without_a_name_column_nothing_is_built() {
        let g = grid("Duration\n3d\n");
        assert!(build(&g, &ImportOptions::guess(&g, "x")).is_err());
        let g = grid("Name\n\n");
        assert!(build(&g, &ImportOptions::guess(&g, "x")).is_err());
    }
}

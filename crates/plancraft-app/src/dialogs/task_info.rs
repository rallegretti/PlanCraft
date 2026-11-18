//! Task Information dialog.

use egui::{Context, RichText, Ui};
use plancraft_core::custom::CustomFieldKind;
use plancraft_core::prelude::*;
use plancraft_core::workspace::Document;

use crate::edit::EditExt;
use crate::format;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Predecessors,
    Resources,
    Advanced,
    Estimate,
    Milestone,
    Custom,
    Notes,
}

/// One custom field in the dialog: the text it opened with and the text being edited.
#[derive(Clone)]
struct CustomRow {
    field: CustomFieldId,
    original: String,
    text: String,
}

#[derive(Clone, PartialEq)]
struct PredRow {
    pred: TaskRef,
    kind: LinkType,
    lag: String,
}

#[derive(Clone, PartialEq)]
struct ResRow {
    resource: ResourceId,
    units: String,
    work: String,
}

#[derive(Clone, Copy, PartialEq)]
enum EstKind {
    None,
    Complexity,
    ThreePoint,
    Work,
}

#[derive(Clone)]
pub struct TaskInfo {
    /// The fields as the dialog opened: OK writes only what was changed since, so changes an
    /// agent (or the table) made meanwhile are kept.
    orig: Option<Box<TaskInfo>>,
    id: TaskId,
    /// Multiple Task Information: the other selected tasks, which get the same changes.
    others: Vec<TaskId>,
    tab: Tab,
    name: String,
    duration: String,
    estimated: bool,
    percent: String,
    priority: String,
    mode: ScheduleMode,
    start: String,
    finish: String,
    milestone: bool,
    active: bool,
    preds: Vec<PredRow>,
    resources: Vec<ResRow>,
    constraint: ConstraintKind,
    constraint_date: String,
    deadline: String,
    task_type: TaskType,
    effort_driven: bool,
    calendar: Option<CalendarId>,
    ignore_res_cal: bool,
    fixed_cost: String,
    actual_start: String,
    actual_finish: String,
    /// Resume date of an in-progress task, and the text it opened with.
    resume: String,
    resume_orig: String,
    on_timeline: bool,
    est_kind: EstKind,
    points: String,
    optimistic: String,
    most_likely: String,
    pessimistic: String,
    est_work: String,
    deliverable: String,
    members: Vec<TaskRef>,
    notes: String,
    custom: Vec<CustomRow>,
    error: Option<String>,
    summary: bool,
}

/// A custom value as text the dialog can edit and parse back (ISO dates, plain numbers).
fn custom_edit_text(p: &Project, field: CustomFieldId, v: Option<&CustomValue>) -> String {
    match v {
        None => String::new(),
        Some(CustomValue::Date(d)) => format::iso_date(Some(*d)),
        Some(CustomValue::Number(n)) => format!("{n}"),
        Some(other) => p.custom_value_text(field, other),
    }
}

impl TaskInfo {
    pub fn new(p: &Project, id: TaskId, notes_tab: bool) -> Option<Self> {
        let t = p.task(id)?;
        let (est_kind, points, o, m, pe, w) = match t.estimate {
            None => (EstKind::None, String::new(), String::new(), String::new(), String::new(), String::new()),
            Some(Estimate::Complexity { points }) => {
                (EstKind::Complexity, points.to_string(), String::new(), String::new(), String::new(), String::new())
            }
            Some(Estimate::ThreePoint { optimistic, most_likely, pessimistic }) => (
                EstKind::ThreePoint,
                String::new(),
                format_work(optimistic),
                format_work(most_likely),
                format_work(pessimistic),
                String::new(),
            ),
            Some(Estimate::Work { minutes }) => {
                (EstKind::Work, String::new(), String::new(), String::new(), String::new(), format_work(minutes))
            }
        };
        let mut info = Self {
            orig: None,
            id,
            others: Vec::new(),
            tab: if notes_tab { Tab::Notes } else { Tab::General },
            name: t.name.clone(),
            duration: p.format_task_minutes(t, t.sched.duration, t.estimated),
            estimated: t.estimated,
            percent: format!("{}", t.percent_complete),
            priority: t.priority.to_string(),
            mode: t.mode,
            start: format::iso(t.sched.start),
            finish: format::iso(t.sched.finish),
            milestone: t.is_milestone(),
            active: t.active,
            preds: t
                .predecessors
                .iter()
                .map(|d| PredRow { pred: d.predecessor, kind: d.kind, lag: d.lag.format(&p.settings.time) })
                .collect(),
            resources: p
                .assignments_for_task(id)
                .map(|a| ResRow { resource: a.resource, units: format::units(a.units), work: format_work(a.work) })
                .collect(),
            constraint: t.constraint.kind,
            constraint_date: format::iso(t.constraint.date),
            deadline: format::iso(t.deadline),
            task_type: t.task_type,
            effort_driven: t.effort_driven,
            calendar: t.calendar,
            ignore_res_cal: t.ignore_resource_calendars,
            fixed_cost: format!("{:.2}", t.fixed_cost),
            actual_start: format::iso(t.actual_start),
            actual_finish: format::iso(t.actual_finish),
            resume: format::iso(t.sched.resume),
            resume_orig: format::iso(t.sched.resume),
            on_timeline: t.show_on_timeline,
            est_kind,
            points,
            optimistic: o,
            most_likely: m,
            pessimistic: pe,
            est_work: w,
            deliverable: t.deliverable.clone(),
            members: t.members.clone(),
            notes: t.notes.clone(),
            custom: p
                .custom_fields
                .iter()
                .map(|f| {
                    let text = custom_edit_text(p, f.id, t.custom.get(&f.id));
                    CustomRow { field: f.id, original: text.clone(), text }
                })
                .collect(),
            error: None,
            summary: t.sched.is_summary,
        };
        info.orig = Some(Box::new(info.clone()));
        Some(info)
    }

    /// Multiple Task Information for `ids` (one task: plain Task Information). Text fields whose
    /// value differs between the tasks start empty; OK writes only the fields changed in the
    /// dialog, to every task, so the others keep their own values. Predecessors, resources and
    /// milestone members are left out: they belong to one task.
    pub fn new_multi(p: &Project, ids: &[TaskId]) -> Option<Self> {
        let infos: Vec<TaskInfo> = ids.iter().filter_map(|id| Self::new(p, *id, false)).collect();
        let (first, rest) = infos.split_first()?;
        let mut base = first.clone();
        if rest.is_empty() {
            return Some(base);
        }
        macro_rules! mixed {
            ($($field:ident),*) => {$(
                if rest.iter().any(|o| o.$field != base.$field) {
                    base.$field = String::new();
                }
            )*};
        }
        mixed!(
            name,
            duration,
            percent,
            priority,
            start,
            finish,
            constraint_date,
            deadline,
            fixed_cost,
            actual_start,
            actual_finish,
            points,
            optimistic,
            most_likely,
            pessimistic,
            est_work,
            deliverable,
            notes
        );
        if rest.iter().any(|o| o.resume != base.resume) {
            base.resume = String::new();
            base.resume_orig = String::new();
        }
        for (k, row) in base.custom.iter_mut().enumerate() {
            if rest.iter().any(|o| o.custom.get(k).is_none_or(|r| r.text != row.text)) {
                row.text = String::new();
                row.original = String::new();
            }
        }
        base.others = rest.iter().map(|i| i.id).collect();
        base.summary = infos.iter().all(|i| i.summary);
        if matches!(base.tab, Tab::Predecessors | Tab::Resources | Tab::Milestone) {
            base.tab = Tab::General;
        }
        base.orig = None;
        base.orig = Some(Box::new(base.clone()));
        Some(base)
    }

    fn is_multi(&self) -> bool {
        !self.others.is_empty()
    }

    /// Writes the fields changed since the dialog opened to its task, or to each of its tasks
    /// (Multiple Task Information), in one edit.
    fn apply_all(&self, p: &mut Project) -> Result<(), String> {
        let Some(o) = self.orig.as_deref().filter(|_| self.is_multi()) else { return self.apply(p) };
        for id in std::iter::once(self.id).chain(self.others.iter().copied()) {
            // Deleted meanwhile (by an agent): nothing to change there.
            let Some(t) = p.task(id) else { continue };
            let mut one = self.clone();
            let mut orig = o.clone();
            one.id = id;
            // What the dialog didn't change follows each task (its kind and schedule mode).
            one.summary = t.sched.is_summary;
            orig.summary = one.summary;
            if self.mode == o.mode {
                one.mode = t.mode;
                orig.mode = t.mode;
            }
            if self.milestone == o.milestone {
                one.milestone = t.is_milestone();
                orig.milestone = one.milestone;
            }
            // Only "Estimated" ticked: each task keeps its own duration (the field may be empty
            // because the durations differ).
            if self.duration == o.duration {
                let own = p.format_task_minutes(t, t.sched.duration, t.estimated);
                one.duration = own.clone();
                orig.duration = own;
            }
            let name = t.name.clone();
            one.orig = Some(Box::new(orig));
            one.apply(p).map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(())
    }

    /// Writes the fields changed since the dialog opened, in one edit.
    fn apply(&self, p: &mut Project) -> Result<(), String> {
        let id = self.id;
        if p.task(id).is_none() {
            return Err("task was deleted".into());
        }
        // Nothing to compare with means nothing was changed here.
        let Some(o) = self.orig.as_deref() else { return Ok(()) };
        let err = |e: PlanError| e.to_string();
        let opt_date = |p: &Project, s: &str, finish: bool| -> Result<Option<DateTime>, String> {
            if s.trim().is_empty() || s.trim().eq_ignore_ascii_case("na") {
                Ok(None)
            } else {
                p.parse_datetime(s, finish).map(Some).ok_or_else(|| format!("'{s}' is not a valid date"))
            }
        };
        if self.name != o.name && self.name.trim().is_empty() {
            return Err("The task name cannot be empty.".into());
        }
        let priority = if self.priority != o.priority {
            Some(self.priority.trim().parse::<u16>().map_err(|_| "Priority must be a number from 0 to 1000")?.min(1000))
        } else {
            None
        };
        let fixed_cost = if self.fixed_cost.trim() != o.fixed_cost.trim() {
            Some(format::parse_money(&self.fixed_cost).ok_or_else(|| format!("'{}' is not a valid fixed cost", self.fixed_cost))?)
        } else {
            None
        };
        if self.mode != o.mode {
            p.set_schedule_mode(&[id], self.mode).map_err(err)?;
        }
        {
            let t = p.task_mut(id).ok_or("task was deleted")?;
            if self.name != o.name {
                t.name = self.name.trim().to_string();
            }
            if self.active != o.active {
                t.active = self.active;
            }
            if let Some(v) = priority {
                t.priority = v;
            }
            if self.task_type != o.task_type {
                t.task_type = self.task_type;
            }
            if self.effort_driven != o.effort_driven {
                t.effort_driven = self.effort_driven;
            }
            if self.calendar != o.calendar {
                t.calendar = self.calendar;
            }
            if self.ignore_res_cal != o.ignore_res_cal {
                t.ignore_resource_calendars = self.ignore_res_cal;
            }
            if self.on_timeline != o.on_timeline {
                t.show_on_timeline = self.on_timeline;
            }
            if self.notes != o.notes {
                t.notes = self.notes.clone();
            }
            if self.deliverable != o.deliverable {
                t.deliverable = self.deliverable.clone();
            }
            if let Some(v) = fixed_cost {
                t.fixed_cost = v;
            }
        }
        if self.constraint != o.constraint || self.constraint_date != o.constraint_date {
            let cdate = opt_date(p, &self.constraint_date, !self.constraint.is_start_constraint())?;
            if self.constraint.needs_date() && cdate.is_none() {
                return Err(format!("'{}' needs a constraint date.", self.constraint.label()));
            }
            let t = p.task_mut(id).ok_or("task was deleted")?;
            t.constraint = Constraint { kind: self.constraint, date: if self.constraint.needs_date() { cdate } else { None } };
        }
        if self.deadline != o.deadline {
            let deadline = opt_date(p, &self.deadline, true)?;
            p.task_mut(id).ok_or("task was deleted")?.deadline = deadline;
        }
        if self.mode == ScheduleMode::Manual && (self.start != o.start || self.finish != o.finish) {
            let start = opt_date(p, &self.start, false)?;
            let finish = opt_date(p, &self.finish, true)?;
            let t = p.task_mut(id).ok_or("task was deleted")?;
            if self.start != o.start {
                t.manual_start = start;
            }
            if self.finish != o.finish {
                t.manual_finish = finish;
            }
        }
        let estimate_changed = self.est_kind != o.est_kind
            || self.points != o.points
            || self.optimistic != o.optimistic
            || self.most_likely != o.most_likely
            || self.pessimistic != o.pessimistic
            || self.est_work != o.est_work;
        if estimate_changed {
            let estimate = match self.est_kind {
                EstKind::None => None,
                EstKind::Complexity => {
                    Some(Estimate::Complexity { points: self.points.trim().parse().map_err(|_| "Complexity points must be a number")? })
                }
                EstKind::ThreePoint => {
                    let w = |s: &str| parse_work(s, &p.settings.time).ok_or_else(|| format!("'{s}' is not a valid amount of work"));
                    Some(Estimate::ThreePoint {
                        optimistic: w(&self.optimistic)?,
                        most_likely: w(&self.most_likely)?,
                        pessimistic: w(&self.pessimistic)?,
                    })
                }
                EstKind::Work => {
                    Some(Estimate::Work { minutes: parse_work(&self.est_work, &p.settings.time).ok_or("Invalid work estimate")? })
                }
            };
            p.task_mut(id).ok_or("task was deleted")?.estimate = estimate;
        }
        if !self.summary {
            if self.milestone != o.milestone {
                p.set_milestone(id, self.milestone).map_err(err)?;
            }
            if !self.milestone && (self.duration != o.duration || self.estimated != o.estimated) {
                let typed = p.parse_task_duration(&self.duration).ok_or_else(|| format!("'{}' is not a valid duration", self.duration))?;
                let typed = TypedDuration { estimated: typed.estimated || self.estimated, ..typed };
                p.set_typed_duration(id, typed).map_err(err)?;
            }
        }
        if self.preds != o.preds {
            let mut deps = Vec::new();
            for r in &self.preds {
                let lag = Lag::parse(&r.lag, &p.settings.time).ok_or_else(|| format!("'{}' is not a valid lag", r.lag))?;
                deps.push(Dependency { predecessor: r.pred, kind: r.kind, lag });
            }
            p.set_predecessors(id, deps).map_err(err)?;
        }
        if self.milestone && self.members != o.members {
            if let Some(t) = p.task_mut(id) {
                t.members.clear();
            }
            p.add_milestone_members(id, &self.members).map_err(err)?;
        }
        if !self.summary && self.resources != o.resources {
            self.apply_resources(p, &o.resources)?;
        }
        // Custom fields: only the ones edited here, so values changed meanwhile are kept.
        for row in self.custom.iter().filter(|r| r.text.trim() != r.original.trim()) {
            let listed = p.custom_field(row.field).is_some_and(|f| f.choices.iter().any(|c| c == crate::fields::NO_VALUE));
            let text = if row.text == crate::fields::NO_VALUE && !listed { "" } else { row.text.as_str() };
            let value = p.parse_custom_value(row.field, text).map_err(err)?;
            p.set_custom_value(id, row.field, value).map_err(err)?;
        }
        // Progress last so actual dates use the new schedule.
        plancraft_core::schedule::recalculate(p);
        if self.percent.trim() != o.percent.trim() {
            let pct: u8 =
                self.percent.trim().trim_end_matches('%').parse::<f64>().map_err(|_| "% complete must be a number")?.clamp(0.0, 100.0)
                    as u8;
            p.set_percent_complete(id, pct).map_err(err)?;
        }
        // An emptied (or "NA") field clears the date, with the same rules as the table's Actual
        // Start and Actual Finish columns (`columns::set_actual_start` / `set_actual_finish`).
        // The finish first: clearing both clears the finish before the start is checked.
        if self.actual_finish != o.actual_finish {
            let d = opt_date(p, &self.actual_finish, true)?;
            crate::columns::set_actual_finish(p, id, d)?;
        }
        if self.actual_start != o.actual_start {
            let d = opt_date(p, &self.actual_start, false)?;
            crate::columns::set_actual_start(p, id, d)?;
        }
        if self.resume.trim() != self.resume_orig.trim() {
            let resume = opt_date(p, &self.resume, false)?;
            p.set_resume(id, resume).map_err(err)?;
        }
        Ok(())
    }

    /// Writes the Resources tab's changes since the dialog opened (`orig`): resources removed
    /// there are unassigned, added ones assigned, and edited units or work set. Assignments an
    /// agent made meanwhile are kept.
    fn apply_resources(&self, p: &mut Project, orig: &[ResRow]) -> Result<(), String> {
        let id = self.id;
        let err = |e: PlanError| e.to_string();
        for gone in orig.iter().filter(|o| !self.resources.iter().any(|r| r.resource == o.resource)) {
            let found: Vec<AssignmentId> = p.assignments_for_task(id).filter(|a| a.resource == gone.resource).map(|a| a.id).collect();
            for a in found {
                p.unassign(a).map_err(err)?;
            }
        }
        for r in &self.resources {
            let before = orig.iter().find(|o| o.resource == r.resource);
            if before == Some(r) {
                continue;
            }
            let units = format::parse_units(&r.units).ok_or_else(|| format!("'{}' is not valid units", r.units))?;
            let work = if r.work.trim().is_empty() {
                None
            } else {
                Some(parse_work(&r.work, &p.settings.time).ok_or_else(|| format!("'{}' is not valid work", r.work))?)
            };
            let existing = p.assignments.iter().find(|a| a.task == id && a.resource == r.resource).map(|a| a.id);
            match (existing, before) {
                (Some(aid), Some(b)) => {
                    if r.units.trim() != b.units.trim() {
                        p.set_assignment_units(aid, units).map_err(err)?;
                    }
                    if let Some(w) = work.filter(|_| r.work.trim() != b.work.trim()) {
                        p.set_assignment_work(aid, w).map_err(err)?;
                    }
                }
                // Already assigned meanwhile (by an agent): keep that assignment.
                (Some(_), None) => {}
                (None, _) => {
                    p.assign(id, r.resource, Some(units), work).map_err(err)?;
                }
            }
        }
        Ok(())
    }

    /// Returns false when the dialog should close.
    pub fn show(&mut self, ctx: &Context, doc: &mut Document) -> bool {
        let mut open = true;
        let mut close = false;
        let multi = format!("Multiple Task Information ({} tasks)", self.others.len().saturating_add(1));
        let title = if self.is_multi() {
            multi.as_str()
        } else if self.milestone {
            "Milestone Information"
        } else if self.summary {
            "Summary Task Information"
        } else {
            "Task Information"
        };
        egui::Window::new(title)
            .id(egui::Id::new("task_info_dialog"))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 120.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .min_height(420.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(320.0));
                    ui.label("Duration:");
                    // Several tasks: each milestone or summary among them keeps its own duration.
                    let duration_editable = if self.is_multi() { !self.summary } else { !self.milestone && !self.summary };
                    ui.add_enabled(duration_editable, egui::TextEdit::singleline(&mut self.duration).desired_width(80.0));
                    ui.checkbox(&mut self.estimated, "Estimated");
                });
                ui.add_space(4.0);
                if self.is_multi() {
                    ui.label(
                        RichText::new("Empty fields differ between the tasks. Only the fields you change are applied, to every task.")
                            .color(theme::pal().TEXT_WEAK),
                    );
                }
                ui.horizontal(|ui| {
                    let mut tabs = vec![(Tab::General, "General")];
                    // Links, assignments and milestone members belong to one task.
                    if !self.is_multi() {
                        tabs.extend([(Tab::Predecessors, "Predecessors"), (Tab::Resources, "Resources")]);
                    }
                    tabs.extend([(Tab::Advanced, "Advanced"), (Tab::Estimate, "Estimate")]);
                    if self.milestone && !self.is_multi() {
                        tabs.push((Tab::Milestone, "Milestone"));
                    }
                    if !self.custom.is_empty() {
                        tabs.push((Tab::Custom, "Custom Fields"));
                    }
                    tabs.push((Tab::Notes, "Notes"));
                    for (t, l) in tabs {
                        ui.selectable_value(&mut self.tab, t, l);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| match self.tab {
                    Tab::General => self.general(ui, &doc.project),
                    Tab::Predecessors => self.predecessors(ui, &doc.project),
                    Tab::Resources => self.resources_tab(ui, &doc.project),
                    Tab::Advanced => self.advanced(ui, &doc.project),
                    Tab::Estimate => self.estimate(ui, &doc.project),
                    Tab::Milestone => self.milestone_tab(ui, &doc.project),
                    Tab::Custom => self.custom_tab(ui, &doc.project),
                    Tab::Notes => {
                        ui.add(egui::TextEdit::multiline(&mut self.notes).desired_rows(14).desired_width(f32::INFINITY));
                    }
                });
                if let Some(e) = &self.error {
                    ui.colored_label(theme::pal().ERROR, e);
                }
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.add(egui::Button::new(RichText::new("  OK  ").color(egui::Color32::WHITE)).fill(theme::pal().ACCENT)).clicked() {
                        let label = if self.is_multi() {
                            "Multiple Task Information".to_string()
                        } else {
                            format!("Task Information: {}", self.name)
                        };
                        match doc.edit_msg(&label, |p| self.apply_all(p)) {
                            Ok(()) => close = true,
                            Err(e) => self.error = Some(e),
                        }
                    }
                });
            });
        open && !close
    }

    fn general(&mut self, ui: &mut Ui, p: &Project) {
        egui::Grid::new("ti_general").num_columns(4).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Percent complete:");
            ui.add(egui::TextEdit::singleline(&mut self.percent).desired_width(60.0));
            ui.label("Priority:");
            ui.add(egui::TextEdit::singleline(&mut self.priority).desired_width(60.0));
            ui.end_row();
            ui.label("Schedule mode:");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.mode, ScheduleMode::Manual, "Manually Scheduled");
                ui.radio_value(&mut self.mode, ScheduleMode::Auto, "Auto Scheduled");
            });
            ui.checkbox(&mut self.active, "Active");
            ui.checkbox(&mut self.on_timeline, "Display on Timeline");
            ui.end_row();
            let manual = self.mode == ScheduleMode::Manual;
            ui.label("Start:");
            ui.add_enabled(manual, egui::TextEdit::singleline(&mut self.start).desired_width(140.0));
            ui.label("Finish:");
            ui.add_enabled(manual, egui::TextEdit::singleline(&mut self.finish).desired_width(140.0));
            ui.end_row();
            ui.label("Actual start:");
            ui.add(egui::TextEdit::singleline(&mut self.actual_start).desired_width(140.0).hint_text("NA"));
            ui.label("Actual finish:");
            ui.add(egui::TextEdit::singleline(&mut self.actual_finish).desired_width(140.0).hint_text("NA"));
            ui.end_row();
            if !self.summary && !self.milestone {
                let stop = p.task(self.id).and_then(|t| t.sched.stop);
                ui.label("Stop:");
                ui.label(RichText::new(if stop.is_some() { format::iso(stop) } else { "NA".into() }).color(theme::pal().TEXT_WEAK))
                    .on_hover_text("End of the work done so far");
                ui.label("Resume:");
                ui.add_enabled(
                    stop.is_some(),
                    egui::TextEdit::singleline(&mut self.resume).desired_width(140.0).hint_text("right after Stop"),
                )
                .on_hover_text("When the remaining work restarts. A later date splits the task.");
                ui.end_row();
            }
            ui.label("");
            ui.add_enabled(!self.summary, egui::Checkbox::new(&mut self.milestone, "Mark task as milestone"));
            ui.end_row();
        });
        if self.mode == ScheduleMode::Auto {
            ui.label(RichText::new("Start and finish of auto-scheduled tasks are calculated from links, constraints and calendars. Set a constraint on the Advanced tab to pin a date.").small().color(theme::pal().TEXT_WEAK));
        }
        if let Some(t) = p.task(self.id) {
            ui.add_space(6.0);
            egui::Grid::new("ti_computed").num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new("Scheduled:").color(theme::pal().TEXT_WEAK));
                ui.label(format!("{} – {}", format::date_long(t.sched.start), format::date_long(t.sched.finish)));
                ui.end_row();
                ui.label(RichText::new("Work:").color(theme::pal().TEXT_WEAK));
                ui.label(format!("{} planned, {} actual", format_work(t.sched.work), format_work(t.sched.actual_work)));
                ui.end_row();
                ui.label(RichText::new("Slack:").color(theme::pal().TEXT_WEAK));
                ui.label(format!(
                    "total {}, free {}{}",
                    p.format_duration(t.sched.total_slack, false),
                    p.format_duration(t.sched.free_slack, false),
                    if t.sched.critical { " · critical" } else { "" }
                ));
                ui.end_row();
                ui.label(RichText::new("Cost:").color(theme::pal().TEXT_WEAK));
                ui.label(format::money(p, t.sched.cost));
                ui.end_row();
                for w in &t.sched.warnings {
                    ui.label(RichText::new("⚠").color(theme::pal().ERROR));
                    ui.label(RichText::new(w).color(theme::pal().ERROR));
                    ui.end_row();
                }
            });
        }
    }

    fn predecessors(&mut self, ui: &mut Ui, p: &Project) {
        let mut remove = None;
        egui::Grid::new("ti_preds").striped(true).num_columns(5).spacing([10.0, 6.0]).show(ui, |ui| {
            for h in ["ID", "Task Name", "Type", "Lag", ""] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for (i, row) in self.preds.iter_mut().enumerate() {
                let id_text = match row.pred {
                    TaskRef::Local(id) => p.row_number(id).map(|r| r.to_string()).unwrap_or_default(),
                    TaskRef::External { task, .. } => format!("ext {}", task.0),
                };
                ui.label(id_text);
                ui.label(p.task_ref_name(&row.pred));
                egui::ComboBox::from_id_salt(("pred_type", i)).selected_text(row.kind.label()).width(170.0).show_ui(ui, |ui| {
                    for k in LinkType::ALL {
                        ui.selectable_value(&mut row.kind, k, k.label());
                    }
                });
                ui.add(egui::TextEdit::singleline(&mut row.lag).desired_width(80.0).hint_text("0d"));
                if ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            self.preds.remove(i);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Add predecessor:");
            egui::ComboBox::from_id_salt("pred_add").selected_text("Choose a task…").width(320.0).show_ui(ui, |ui| {
                for (i, t) in p.tasks.iter().enumerate() {
                    if t.id == self.id || self.preds.iter().any(|r| r.pred == TaskRef::Local(t.id)) {
                        continue;
                    }
                    let label = format!("{}  {}{}", i + 1, "  ".repeat(t.outline_level as usize - 1), t.name);
                    if ui.selectable_label(false, label).clicked() {
                        self.preds.push(PredRow { pred: TaskRef::Local(t.id), kind: LinkType::FS, lag: String::new() });
                    }
                }
                for e in &p.external_projects {
                    for s in &e.tasks {
                        let r = TaskRef::External { project: e.id, task: s.id };
                        if self.preds.iter().any(|x| x.pred == r) {
                            continue;
                        }
                        if ui.selectable_label(false, format!("🔗 {}\\{}", e.name, s.name)).clicked() {
                            self.preds.push(PredRow { pred: r, kind: LinkType::FS, lag: String::new() });
                        }
                    }
                }
            });
        });
        ui.label(
            RichText::new(
                "Lag examples: 2d, -1d (lead), 50%, 3ed (elapsed days). Link to tasks in other files via Project > Links Between Projects.",
            )
            .small()
            .color(theme::pal().TEXT_WEAK),
        );
    }

    fn resources_tab(&mut self, ui: &mut Ui, p: &Project) {
        if self.summary {
            ui.label("Resources cannot be assigned to summary tasks.");
            return;
        }
        let mut remove = None;
        egui::Grid::new("ti_res").striped(true).num_columns(5).spacing([10.0, 6.0]).show(ui, |ui| {
            for h in ["Resource Name", "Type", "Units", "Work", ""] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for (i, row) in self.resources.iter_mut().enumerate() {
                let r = p.resource(row.resource);
                ui.label(r.map(|r| r.name.clone()).unwrap_or_default());
                ui.label(r.map(|r| r.category.label()).unwrap_or_default());
                ui.add(egui::TextEdit::singleline(&mut row.units).desired_width(60.0));
                ui.add(egui::TextEdit::singleline(&mut row.work).desired_width(70.0).hint_text("auto"));
                if ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            self.resources.remove(i);
        }
        ui.horizontal(|ui| {
            ui.label("Add resource:");
            egui::ComboBox::from_id_salt("res_add").selected_text("Choose…").width(260.0).show_ui(ui, |ui| {
                for r in &p.resources {
                    if self.resources.iter().any(|x| x.resource == r.id) {
                        continue;
                    }
                    if ui.selectable_label(false, format!("{} ({})", r.name, r.category.label())).clicked() {
                        self.resources.push(ResRow { resource: r.id, units: "100%".into(), work: String::new() });
                    }
                }
            });
        });
        ui.label(
            RichText::new("Leave Work empty to derive it from the duration (or the estimate for a first assignment).")
                .small()
                .color(theme::pal().TEXT_WEAK),
        );
    }

    fn advanced(&mut self, ui: &mut Ui, p: &Project) {
        egui::Grid::new("ti_adv").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Deadline:");
            ui.add(egui::TextEdit::singleline(&mut self.deadline).desired_width(160.0).hint_text("NA"));
            ui.end_row();
            ui.label("Constraint type:");
            egui::ComboBox::from_id_salt("ti_ctype").selected_text(self.constraint.label()).width(200.0).show_ui(ui, |ui| {
                for k in ConstraintKind::ALL {
                    ui.selectable_value(&mut self.constraint, k, k.label());
                }
            });
            ui.end_row();
            ui.label("Constraint date:");
            ui.add_enabled(
                self.constraint.needs_date(),
                egui::TextEdit::singleline(&mut self.constraint_date).desired_width(160.0).hint_text("YYYY-MM-DD"),
            );
            ui.end_row();
            ui.label("Task type:");
            egui::ComboBox::from_id_salt("ti_ttype")
                .selected_text(match self.task_type {
                    TaskType::FixedUnits => "Fixed Units",
                    TaskType::FixedDuration => "Fixed Duration",
                    TaskType::FixedWork => "Fixed Work",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.task_type, TaskType::FixedUnits, "Fixed Units");
                    ui.selectable_value(&mut self.task_type, TaskType::FixedDuration, "Fixed Duration");
                    ui.selectable_value(&mut self.task_type, TaskType::FixedWork, "Fixed Work");
                });
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut self.effort_driven, "Effort driven");
            ui.end_row();
            ui.label("Calendar:");
            let cal_name = self.calendar.and_then(|c| p.calendar(c)).map(|c| c.name.clone()).unwrap_or_else(|| "None".into());
            egui::ComboBox::from_id_salt("ti_cal").selected_text(cal_name).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.calendar, None, "None");
                for c in &p.calendars {
                    ui.selectable_value(&mut self.calendar, Some(c.id), &c.name);
                }
            });
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut self.ignore_res_cal, "Scheduling ignores resource calendars");
            ui.end_row();
            ui.label("Fixed cost:");
            ui.add(egui::TextEdit::singleline(&mut self.fixed_cost).desired_width(100.0));
            ui.end_row();
            ui.label("WBS code:");
            ui.label(p.task_index(self.id).map(|i| p.wbs(i)).unwrap_or_default());
            ui.end_row();
            ui.label("Unique ID:");
            ui.label(self.id.0.to_string());
            ui.end_row();
        });
    }

    fn estimate(&mut self, ui: &mut Ui, p: &Project) {
        ui.label("Estimate the effort before (or instead of) assigning resources. Estimates drive workload and, for the first assignment, the task's work and duration.");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.est_kind, EstKind::None, "None");
            ui.radio_value(&mut self.est_kind, EstKind::Complexity, "Complexity points");
            ui.radio_value(&mut self.est_kind, EstKind::ThreePoint, "Three-point (PERT)");
            ui.radio_value(&mut self.est_kind, EstKind::Work, "Work");
        });
        ui.add_space(6.0);
        egui::Grid::new("ti_est").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| match self.est_kind {
            EstKind::None => {}
            EstKind::Complexity => {
                ui.label("Points:");
                ui.add(egui::TextEdit::singleline(&mut self.points).desired_width(80.0));
                ui.end_row();
                ui.label("Conversion:");
                ui.label(format!("1 point = {} (Project > Information)", format_work(p.settings.minutes_per_point)));
                ui.end_row();
                if let Ok(v) = self.points.trim().parse::<f64>() {
                    ui.label("Expected work:");
                    ui.label(format_work(Estimate::Complexity { points: v }.expected_work(&p.settings)));
                    ui.end_row();
                }
            }
            EstKind::ThreePoint => {
                ui.label("Optimistic:");
                ui.add(egui::TextEdit::singleline(&mut self.optimistic).desired_width(80.0).hint_text("e.g. 16h"));
                ui.end_row();
                ui.label("Most likely:");
                ui.add(egui::TextEdit::singleline(&mut self.most_likely).desired_width(80.0));
                ui.end_row();
                ui.label("Pessimistic:");
                ui.add(egui::TextEdit::singleline(&mut self.pessimistic).desired_width(80.0));
                ui.end_row();
                let w = |s: &str| parse_work(s, &p.settings.time);
                if let (Some(o), Some(m), Some(pe)) = (w(&self.optimistic), w(&self.most_likely), w(&self.pessimistic)) {
                    let e = Estimate::ThreePoint { optimistic: o, most_likely: m, pessimistic: pe };
                    ui.label("Expected (PERT):");
                    ui.label(format!("{} ± {}", format_work(e.expected_work(&p.settings)), format_work(e.std_dev())));
                    ui.end_row();
                }
            }
            EstKind::Work => {
                ui.label("Work:");
                ui.add(egui::TextEdit::singleline(&mut self.est_work).desired_width(80.0).hint_text("e.g. 24h"));
                ui.end_row();
            }
        });
    }

    fn custom_tab(&mut self, ui: &mut Ui, p: &Project) {
        egui::Grid::new("ti_custom").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            for row in &mut self.custom {
                let Some(f) = p.custom_field(row.field) else { continue };
                ui.label(&f.name);
                let options: Vec<String> = match f.kind {
                    CustomFieldKind::Flag => vec!["Yes".into(), "No".into(), crate::fields::NO_VALUE.into()],
                    CustomFieldKind::Text if !f.choices.is_empty() => {
                        f.choices.iter().cloned().chain(std::iter::once(crate::fields::NO_VALUE.to_string())).collect()
                    }
                    _ => Vec::new(),
                };
                if options.is_empty() {
                    let hint = match f.kind {
                        CustomFieldKind::Date => "YYYY-MM-DD",
                        CustomFieldKind::Duration => "e.g. 3d",
                        CustomFieldKind::Number | CustomFieldKind::Cost => "number",
                        _ => "",
                    };
                    ui.add(egui::TextEdit::singleline(&mut row.text).hint_text(hint).desired_width(220.0));
                } else {
                    let shown = if row.text.is_empty() { crate::fields::NO_VALUE.to_string() } else { row.text.clone() };
                    egui::ComboBox::from_id_salt(("ti_custom", row.field.0)).width(220.0).selected_text(shown).show_ui(ui, |ui| {
                        for o in options {
                            if ui.selectable_label(row.text == o, &o).clicked() {
                                row.text = if o == crate::fields::NO_VALUE { String::new() } else { o };
                            }
                        }
                    });
                }
                ui.end_row();
            }
        });
    }

    fn milestone_tab(&mut self, ui: &mut Ui, p: &Project) {
        ui.label("Deliverable (the output achieved at this milestone):");
        ui.add(egui::TextEdit::singleline(&mut self.deliverable).desired_width(f32::INFINITY));
        ui.add_space(8.0);
        ui.label(RichText::new("Tasks that make up this milestone").strong());
        ui.label(
            RichText::new("The milestone is reached when all of these tasks finish (they act as finish-to-start predecessors).")
                .small()
                .color(theme::pal().TEXT_WEAK),
        );
        let mut remove = None;
        for (i, m) in self.members.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(p.task_ref_name(m));
                if ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.members.remove(i);
        }
        egui::ComboBox::from_id_salt("ms_member_add").selected_text("Add task…").width(320.0).show_ui(ui, |ui| {
            for (i, t) in p.tasks.iter().enumerate() {
                if t.id == self.id || t.is_milestone() || self.members.contains(&TaskRef::Local(t.id)) {
                    continue;
                }
                if ui.selectable_label(false, format!("{}  {}", i + 1, t.name)).clicked() {
                    self.members.push(TaskRef::Local(t.id));
                }
            }
            for e in &p.external_projects {
                for s in &e.tasks {
                    let r = TaskRef::External { project: e.id, task: s.id };
                    if !self.members.contains(&r) && ui.selectable_label(false, format!("🔗 {}\\{}", e.name, s.name)).clicked() {
                        self.members.push(r);
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Project {
        plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap())
    }

    /// Regression: OK wrote every field back as the dialog opened, undoing what an agent changed
    /// while it was open.
    #[test]
    fn ok_keeps_what_an_agent_changed_while_the_dialog_was_open() {
        let mut p = demo();
        let t = p.tasks.iter().find(|t| !t.sched.is_summary && !t.is_milestone() && !t.predecessors.is_empty()).unwrap();
        let (id, pred) = (t.id, t.predecessors[0].predecessor);
        let mut info = TaskInfo::new(&p, id, false).unwrap();

        // Meanwhile an agent renames it, lengthens it, marks progress, unlinks it and adds a note.
        p.task_mut(id).unwrap().name = "Renamed by the agent".into();
        p.set_duration(id, 9 * 480, false).unwrap();
        p.set_percent_complete(id, 30).unwrap();
        p.set_predecessors(id, Vec::new()).unwrap();
        p.task_mut(id).unwrap().priority = 900;
        plancraft_core::schedule::recalculate(&mut p);

        // The user changes only the notes and the deadline.
        info.notes = "Checked with the vendor".into();
        info.deadline = "2027-03-01".into();
        info.apply(&mut p).unwrap();

        let t = p.task(id).unwrap();
        assert_eq!(t.notes, "Checked with the vendor");
        assert!(t.deadline.is_some());
        assert_eq!(t.name, "Renamed by the agent");
        assert_eq!(t.duration, 9 * 480);
        assert_eq!(t.percent_complete, 30);
        assert!(t.predecessors.is_empty(), "the agent's unlink stays, not {pred:?}");
        assert_eq!(t.priority, 900);
    }

    /// Multiple Task Information: fields that differ start empty, and only the fields changed are
    /// written, to every selected task, which keep everything else of their own.
    #[test]
    fn multiple_task_information_changes_only_what_was_changed_in_every_task() {
        let mut p = demo();
        let picked: Vec<&Task> = p.tasks.iter().filter(|t| !t.sched.is_summary && !t.is_milestone()).take(3).collect();
        let ids: Vec<TaskId> = picked.iter().map(|t| t.id).collect();
        let before: Vec<(String, Minutes, Vec<Dependency>)> =
            picked.iter().map(|t| (t.name.clone(), t.duration, t.predecessors.clone())).collect();
        let milestone = p.tasks.iter().find(|t| t.is_milestone()).unwrap().id;
        let summary = p.tasks.iter().find(|t| t.sched.is_summary).unwrap().id;
        let mut all = ids.clone();
        all.extend([milestone, summary]);
        p.task_mut(ids[0]).unwrap().priority = 700;

        let mut info = TaskInfo::new_multi(&p, &all).unwrap();
        assert!(info.is_multi());
        assert_eq!(info.name, "", "the names differ");
        assert_eq!(info.priority, "", "one priority differs");
        info.priority = "800".into();
        info.notes = "Same note for all".into();
        info.duration = "4d".into();
        info.apply_all(&mut p).unwrap();

        for (id, (name, _, preds)) in ids.iter().zip(&before) {
            let t = p.task(*id).unwrap();
            assert_eq!((t.priority, t.notes.as_str(), &t.name, &t.predecessors), (800, "Same note for all", name, preds));
            assert_eq!(t.duration, 4 * 480);
        }
        let m = p.task(milestone).unwrap();
        assert!(m.is_milestone() && m.notes == "Same note for all", "a milestone keeps no duration");
        assert!(p.task(summary).unwrap().sched.is_summary);

        // Only "Estimated" ticked, with durations that differ: each keeps its own, now estimated.
        assert_eq!(TaskInfo::new_multi(&p, &ids).unwrap().duration, "4 days", "all 4 days now");
        p.set_duration(ids[1], 2 * 480, false).unwrap();
        plancraft_core::schedule::recalculate(&mut p);
        let mut info = TaskInfo::new_multi(&p, &ids).unwrap();
        assert_eq!(info.duration, "");
        info.estimated = true;
        info.apply_all(&mut p).unwrap();
        assert_eq!(p.task(ids[1]).unwrap().duration, 2 * 480);
        assert!(ids.iter().all(|id| p.task(*id).unwrap().estimated));

        // One task: the plain dialog.
        assert!(!TaskInfo::new_multi(&p, &ids[..1]).unwrap().is_multi());
        assert!(TaskInfo::new_multi(&p, &[]).is_none());
    }

    #[test]
    fn ok_writes_the_fields_changed_in_the_dialog() {
        let mut p = demo();
        let id = p.tasks.iter().find(|t| !t.sched.is_summary && !t.is_milestone()).unwrap().id;
        let mut info = TaskInfo::new(&p, id, false).unwrap();
        info.name = "  New name ".into();
        info.duration = "3d".into();
        info.percent = "50".into();
        info.priority = "700".into();
        info.apply(&mut p).unwrap();
        let t = p.task(id).unwrap();
        assert_eq!((t.name.as_str(), t.duration, t.percent_complete, t.priority), ("New name", 3 * 480, 50, 700));
    }

    /// Resources removed or added in the dialog change only those assignments.
    #[test]
    fn resources_tab_keeps_assignments_an_agent_added() {
        let mut p = demo();
        let id = p.tasks.iter().find(|t| !t.sched.is_summary && p.assignments_for_task(t.id).count() == 2).unwrap().id;
        let first = p.assignments_for_task(id).next().unwrap().resource;
        let mut info = TaskInfo::new(&p, id, false).unwrap();
        let extra = p.add_resource("Agent's pick");
        p.assign(id, extra, None, None).unwrap();
        info.resources.retain(|r| r.resource != first);
        info.apply(&mut p).unwrap();
        let now: Vec<ResourceId> = p.assignments_for_task(id).map(|a| a.resource).collect();
        assert!(!now.contains(&first), "removed in the dialog");
        assert!(now.contains(&extra), "the agent's assignment stays: {now:?}");
        assert_eq!(now.len(), 2);
    }

    /// Regression: emptying Actual Start or Actual Finish did nothing (an empty field parsed to
    /// "no date", which was skipped).
    #[test]
    fn emptied_actual_dates_are_cleared() {
        let mut p = demo();
        let id = p.tasks.iter().find(|t| !t.sched.is_summary && !t.is_milestone() && t.percent_complete == 0).unwrap().id;
        p.set_percent_complete(id, 100).unwrap();
        plancraft_core::schedule::recalculate(&mut p);
        let t = p.task(id).unwrap();
        assert!(t.actual_start.is_some() && t.actual_finish.is_some());

        // Back to 0% with both dates emptied (" " and "NA" count as empty): both are cleared.
        let mut info = TaskInfo::new(&p, id, false).unwrap();
        info.percent = "0".into();
        info.actual_start = "NA".into();
        info.actual_finish = "  ".into();
        info.apply(&mut p).unwrap();
        let t = p.task(id).unwrap();
        assert_eq!((t.actual_start, t.actual_finish), (None, None));
    }

    /// Regression (review): clearing Actual Start left a task "half done, never started". A task
    /// that shows progress keeps its start (with a message saying why); entering an Actual Finish
    /// on a task that never started starts it, as in the table.
    #[test]
    fn a_task_with_progress_keeps_its_actual_start() {
        let mut p = demo();
        let id = p.tasks.iter().find(|t| !t.sched.is_summary && !t.is_milestone() && t.percent_complete == 0).unwrap().id;
        p.set_percent_complete(id, 50).unwrap();
        plancraft_core::schedule::recalculate(&mut p);
        let mut info = TaskInfo::new(&p, id, false).unwrap();
        info.actual_start = String::new();
        let e = info.apply(&mut p).unwrap_err();
        assert!(e.contains("% complete to 0"), "{e}");

        // Regression (second review): clearing Actual Finish left a task "100% complete, not
        // finished"; it is refused while the task is 100% complete.
        let mut p = demo();
        let id = p.tasks.iter().find(|t| !t.sched.is_summary && !t.is_milestone() && t.percent_complete == 0).unwrap().id;
        p.set_percent_complete(id, 100).unwrap();
        plancraft_core::schedule::recalculate(&mut p);
        let mut info = TaskInfo::new(&p, id, false).unwrap();
        info.actual_finish = String::new();
        let e = info.apply(&mut p).unwrap_err();
        assert!(e.contains("lower % complete"), "{e}");

        let mut p = demo();
        let id = p.tasks.iter().find(|t| !t.sched.is_summary && !t.is_milestone() && t.percent_complete == 0).unwrap().id;
        let scheduled = p.task(id).unwrap().sched.start;
        let mut info = TaskInfo::new(&p, id, false).unwrap();
        info.actual_finish = "2026-12-01".into();
        info.apply(&mut p).unwrap();
        let t = p.task(id).unwrap();
        assert_eq!((t.actual_start, t.percent_complete), (scheduled, 100));
    }

    /// An actual date that was already empty when the dialog opened is left alone, so one an
    /// agent entered meanwhile is kept.
    #[test]
    fn untouched_empty_actual_dates_keep_what_an_agent_entered() {
        let mut p = demo();
        let id = p.tasks.iter().find(|t| !t.sched.is_summary && !t.is_milestone() && t.percent_complete == 0).unwrap().id;
        let mut info = TaskInfo::new(&p, id, false).unwrap();
        assert!(info.actual_start.is_empty() && info.actual_finish.is_empty());
        p.set_percent_complete(id, 100).unwrap();
        info.notes = "Done early".into();
        info.apply(&mut p).unwrap();
        let t = p.task(id).unwrap();
        assert!(t.actual_start.is_some() && t.actual_finish.is_some(), "{:?} {:?}", t.actual_start, t.actual_finish);
    }

    #[test]
    fn an_invalid_fixed_cost_is_refused() {
        let mut p = demo();
        let id = p.tasks.iter().find(|t| !t.sched.is_summary).unwrap().id;
        let mut info = TaskInfo::new(&p, id, false).unwrap();
        info.fixed_cost = "lots".into();
        assert!(info.apply(&mut p).is_err());
    }
}

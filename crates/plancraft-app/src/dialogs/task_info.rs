//! Task Information dialog.

use egui::{Context, RichText, Ui};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document};

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
    Notes,
}

#[derive(Clone)]
struct PredRow {
    pred: TaskRef,
    kind: LinkType,
    lag: String,
}

#[derive(Clone)]
struct ResRow {
    resource: ResourceId,
    units: String,
    work: String,
    existing: Option<AssignmentId>,
}

#[derive(Clone, Copy, PartialEq)]
enum EstKind {
    None,
    Complexity,
    ThreePoint,
    Work,
}

pub struct TaskInfo {
    id: TaskId,
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
    error: Option<String>,
    summary: bool,
}

impl TaskInfo {
    pub fn new(p: &Project, id: TaskId, notes_tab: bool) -> Option<Self> {
        let idx = p.task_index(id)?;
        let t = &p.tasks[idx];
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
        Some(Self {
            id,
            tab: if notes_tab { Tab::Notes } else { Tab::General },
            name: t.name.clone(),
            duration: p.format_duration(t.sched.duration, t.estimated),
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
                .map(|a| ResRow { resource: a.resource, units: format::units(a.units), work: format_work(a.work), existing: Some(a.id) })
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
            error: None,
            summary: t.sched.is_summary,
        })
    }

    fn apply(&self, p: &mut Project) -> Result<(), String> {
        let id = self.id;
        let idx = p.task_index(id).ok_or("task was deleted")?;
        let err = |e: PlanError| e.to_string();
        let opt_date = |p: &Project, s: &str, finish: bool| -> Result<Option<chrono::NaiveDateTime>, String> {
            if s.trim().is_empty() || s.trim().eq_ignore_ascii_case("na") {
                Ok(None)
            } else {
                p.parse_datetime(s, finish).map(Some).ok_or_else(|| format!("'{s}' is not a valid date"))
            }
        };
        if self.name.trim().is_empty() {
            return Err("The task name cannot be empty.".into());
        }
        {
            let t = &mut p.tasks[idx];
            t.name = self.name.trim().to_string();
            t.mode = self.mode;
            t.active = self.active;
            t.priority = self.priority.trim().parse::<u16>().map_err(|_| "Priority must be a number from 0 to 1000")?.min(1000);
            t.task_type = self.task_type;
            t.effort_driven = self.effort_driven;
            t.calendar = self.calendar;
            t.ignore_resource_calendars = self.ignore_res_cal;
            t.show_on_timeline = self.on_timeline;
            t.notes = self.notes.clone();
            t.deliverable = self.deliverable.clone();
            t.fixed_cost = format::parse_money(&self.fixed_cost).unwrap_or(0.0);
        }
        let cdate = opt_date(p, &self.constraint_date, !self.constraint.is_start_constraint())?;
        if self.constraint.needs_date() && cdate.is_none() {
            return Err(format!("'{}' needs a constraint date.", self.constraint.label()));
        }
        let deadline = opt_date(p, &self.deadline, true)?;
        let astart = opt_date(p, &self.actual_start, false)?;
        let afinish = opt_date(p, &self.actual_finish, true)?;
        let manual_start = opt_date(p, &self.start, false)?;
        let manual_finish = opt_date(p, &self.finish, true)?;
        {
            let t = &mut p.tasks[idx];
            t.constraint = Constraint { kind: self.constraint, date: if self.constraint.needs_date() { cdate } else { None } };
            t.deadline = deadline;
            if self.mode == ScheduleMode::Manual {
                t.manual_start = manual_start;
                t.manual_finish = manual_finish;
            }
            t.estimate = match self.est_kind {
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
        }
        if !self.summary {
            p.set_milestone(id, self.milestone).map_err(err)?;
            if !self.milestone {
                let (mins, est) = p.parse_duration(&self.duration).ok_or_else(|| format!("'{}' is not a valid duration", self.duration))?;
                let cur = &p.tasks[p.task_index(id).unwrap()];
                if mins != cur.duration || (est || self.estimated) != cur.estimated {
                    p.set_duration(id, mins, est || self.estimated).map_err(err)?;
                }
            }
        }
        // Predecessors.
        let mut deps = Vec::new();
        for r in &self.preds {
            let lag = Lag::parse(&r.lag, &p.settings.time).ok_or_else(|| format!("'{}' is not a valid lag", r.lag))?;
            deps.push(Dependency { predecessor: r.pred, kind: r.kind, lag });
        }
        p.set_predecessors(id, deps).map_err(err)?;
        // Milestone members.
        if self.milestone {
            let idx = p.task_index(id).unwrap();
            p.tasks[idx].members.clear();
            p.add_milestone_members(id, &self.members).map_err(err)?;
        }
        // Resources.
        if !self.summary {
            let keep: Vec<ResourceId> = self.resources.iter().map(|r| r.resource).collect();
            let remove: Vec<AssignmentId> = p.assignments_for_task(id).filter(|a| !keep.contains(&a.resource)).map(|a| a.id).collect();
            for a in remove {
                p.unassign(a).map_err(err)?;
            }
            for r in &self.resources {
                let units = format::parse_units(&r.units).ok_or_else(|| format!("'{}' is not valid units", r.units))?;
                let work = if r.work.trim().is_empty() {
                    None
                } else {
                    Some(parse_work(&r.work, &p.settings.time).ok_or_else(|| format!("'{}' is not valid work", r.work))?)
                };
                let existing = p.assignments.iter().find(|a| a.task == id && a.resource == r.resource).map(|a| (a.id, a.work, a.units));
                match existing {
                    Some((aid, old_work, old_units)) => {
                        if (old_units - units).abs() > 1e-6 {
                            p.set_assignment_units(aid, units).map_err(err)?;
                        }
                        if let Some(w) = work
                            && w != old_work
                        {
                            p.set_assignment_work(aid, w).map_err(err)?;
                        }
                    }
                    None => {
                        p.assign(id, r.resource, Some(units), work).map_err(err)?;
                    }
                }
            }
        }
        // Progress last so actual dates use the new schedule.
        plancraft_core::schedule::recalculate(p);
        let pct: u8 =
            self.percent.trim().trim_end_matches('%').parse::<f64>().map_err(|_| "% complete must be a number")?.clamp(0.0, 100.0) as u8;
        let idx = p.task_index(id).unwrap();
        if pct != p.tasks[idx].percent_complete {
            p.set_percent_complete(id, pct).map_err(err)?;
        }
        let t = &mut p.tasks[idx];
        if astart != t.actual_start && astart.is_some() {
            t.actual_start = astart;
        }
        if afinish != t.actual_finish && afinish.is_some() {
            t.actual_finish = afinish;
            t.percent_complete = 100;
        }
        Ok(())
    }

    /// Returns false when the dialog should close.
    pub fn show(&mut self, ctx: &Context, doc: &mut Document) -> bool {
        let mut open = true;
        let mut close = false;
        let title = if self.milestone {
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
                    ui.add_enabled(!self.milestone && !self.summary, egui::TextEdit::singleline(&mut self.duration).desired_width(80.0));
                    ui.checkbox(&mut self.estimated, "Estimated");
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let mut tabs = vec![
                        (Tab::General, "General"),
                        (Tab::Predecessors, "Predecessors"),
                        (Tab::Resources, "Resources"),
                        (Tab::Advanced, "Advanced"),
                        (Tab::Estimate, "Estimate"),
                    ];
                    if self.milestone {
                        tabs.push((Tab::Milestone, "Milestone"));
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
                    Tab::Notes => {
                        ui.add(egui::TextEdit::multiline(&mut self.notes).desired_rows(14).desired_width(f32::INFINITY));
                    }
                });
                if let Some(e) = &self.error {
                    ui.colored_label(theme::ERROR, e);
                }
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.add(egui::Button::new(RichText::new("  OK  ").color(egui::Color32::WHITE)).fill(theme::ACCENT)).clicked() {
                        let label = format!("Task Information: {}", self.name);
                        match doc.try_edit(&label, ChangeSource::User, |p| self.apply(p)) {
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
            ui.label("");
            ui.add_enabled(!self.summary, egui::Checkbox::new(&mut self.milestone, "Mark task as milestone"));
            ui.end_row();
        });
        if self.mode == ScheduleMode::Auto {
            ui.label(RichText::new("Start and finish of auto-scheduled tasks are calculated from links, constraints and calendars. Set a constraint on the Advanced tab to pin a date.").small().color(theme::TEXT_WEAK));
        }
        if let Some(t) = p.task(self.id) {
            ui.add_space(6.0);
            egui::Grid::new("ti_computed").num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new("Scheduled:").color(theme::TEXT_WEAK));
                ui.label(format!("{} – {}", format::date_long(t.sched.start), format::date_long(t.sched.finish)));
                ui.end_row();
                ui.label(RichText::new("Work:").color(theme::TEXT_WEAK));
                ui.label(format!("{} planned, {} actual", format_work(t.sched.work), format_work(t.sched.actual_work)));
                ui.end_row();
                ui.label(RichText::new("Slack:").color(theme::TEXT_WEAK));
                ui.label(format!(
                    "total {}, free {}{}",
                    p.format_duration(t.sched.total_slack, false),
                    p.format_duration(t.sched.free_slack, false),
                    if t.sched.critical { " · critical" } else { "" }
                ));
                ui.end_row();
                ui.label(RichText::new("Cost:").color(theme::TEXT_WEAK));
                ui.label(format::money(p, t.sched.cost));
                ui.end_row();
                for w in &t.sched.warnings {
                    ui.label(RichText::new("⚠").color(theme::ERROR));
                    ui.label(RichText::new(w).color(theme::ERROR));
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
            .color(theme::TEXT_WEAK),
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
                        self.resources.push(ResRow { resource: r.id, units: "100%".into(), work: String::new(), existing: None });
                    }
                }
            });
        });
        let _ = self.resources.iter().map(|r| r.existing).count();
        ui.label(
            RichText::new("Leave Work empty to derive it from the duration (or the estimate for a first assignment).")
                .small()
                .color(theme::TEXT_WEAK),
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

    fn milestone_tab(&mut self, ui: &mut Ui, p: &Project) {
        ui.label("Deliverable (the output achieved at this milestone):");
        ui.add(egui::TextEdit::singleline(&mut self.deliverable).desired_width(f32::INFINITY));
        ui.add_space(8.0);
        ui.label(RichText::new("Tasks that make up this milestone").strong());
        ui.label(
            RichText::new("The milestone is reached when all of these tasks finish (they act as finish-to-start predecessors).")
                .small()
                .color(theme::TEXT_WEAK),
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

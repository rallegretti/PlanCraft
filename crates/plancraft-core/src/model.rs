//! The PlanCraft project model.
//!
//! A [`Project`] contains an ordered outline of [`Task`]s (regular tasks,
//! milestones and summary tasks), the [`Resource`]s that perform the work,
//! [`Assignment`]s linking the two, working-time [`Calendar`]s, tracked
//! [`TimeEntry`]s and references to [`ExternalProject`]s whose tasks may be
//! used as predecessors.
//!
//! Fields grouped in [`TaskSchedule`] are *computed* by the scheduler; they
//! are persisted so that other documents (and AI agents) can read dates
//! without re-running the engine, but they are always overwritten on
//! recalculation.

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::calendar::{Calendar, CalendarException};
use crate::duration::{DurationUnit, Lag, Minutes, TimeSettings};
use crate::ids::*;

pub const FILE_FORMAT: &str = "plancraft";
pub const FILE_VERSION: u32 = 1;
pub const FILE_EXTENSION: &str = "plancraft";

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_true(b: &bool) -> bool {
    *b
}
fn yes() -> bool {
    true
}
fn default_priority() -> u16 {
    500
}
fn default_units() -> f64 {
    1.0
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// What a row in the task outline represents.
///
/// Summary tasks are not a kind: any task followed by tasks with a deeper
/// outline level is a summary task.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Task,
    /// A checkpoint in time that completes when its member tasks and predecessors finish.
    Milestone,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleMode {
    /// Dates are computed by the scheduling engine.
    #[default]
    Auto,
    /// Dates are entered by the user and only used as given.
    Manual,
}

/// Which of duration, units and work stays fixed when one of the others changes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    #[default]
    FixedUnits,
    FixedDuration,
    FixedWork,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    /// As soon as possible.
    #[default]
    Asap,
    /// As late as possible.
    Alap,
    /// Start no earlier than.
    Snet,
    /// Start no later than.
    Snlt,
    /// Finish no earlier than.
    Fnet,
    /// Finish no later than.
    Fnlt,
    /// Must start on.
    Mso,
    /// Must finish on.
    Mfo,
}

impl ConstraintKind {
    pub const ALL: [ConstraintKind; 8] = [
        ConstraintKind::Asap,
        ConstraintKind::Alap,
        ConstraintKind::Snet,
        ConstraintKind::Snlt,
        ConstraintKind::Fnet,
        ConstraintKind::Fnlt,
        ConstraintKind::Mso,
        ConstraintKind::Mfo,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConstraintKind::Asap => "As Soon As Possible",
            ConstraintKind::Alap => "As Late As Possible",
            ConstraintKind::Snet => "Start No Earlier Than",
            ConstraintKind::Snlt => "Start No Later Than",
            ConstraintKind::Fnet => "Finish No Earlier Than",
            ConstraintKind::Fnlt => "Finish No Later Than",
            ConstraintKind::Mso => "Must Start On",
            ConstraintKind::Mfo => "Must Finish On",
        }
    }

    pub fn needs_date(self) -> bool {
        !matches!(self, ConstraintKind::Asap | ConstraintKind::Alap)
    }

    /// Whether the constraint date refers to the start (otherwise the finish).
    pub fn is_start_constraint(self) -> bool {
        matches!(self, ConstraintKind::Snet | ConstraintKind::Snlt | ConstraintKind::Mso)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Constraint {
    pub kind: ConstraintKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<NaiveDateTime>,
}

/// Dependency type between a predecessor and a successor.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
pub enum LinkType {
    /// Finish-to-Start.
    #[default]
    FS,
    /// Start-to-Start.
    SS,
    /// Finish-to-Finish.
    FF,
    /// Start-to-Finish.
    SF,
}

impl LinkType {
    pub const ALL: [LinkType; 4] = [LinkType::FS, LinkType::SS, LinkType::FF, LinkType::SF];

    pub fn code(self) -> &'static str {
        match self {
            LinkType::FS => "FS",
            LinkType::SS => "SS",
            LinkType::FF => "FF",
            LinkType::SF => "SF",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LinkType::FS => "Finish-to-Start (FS)",
            LinkType::SS => "Start-to-Start (SS)",
            LinkType::FF => "Finish-to-Finish (FF)",
            LinkType::SF => "Start-to-Finish (SF)",
        }
    }

    pub fn parse(s: &str) -> Option<LinkType> {
        match s.trim().to_ascii_uppercase().as_str() {
            "FS" => Some(LinkType::FS),
            "SS" => Some(LinkType::SS),
            "FF" => Some(LinkType::FF),
            "SF" => Some(LinkType::SF),
            _ => None,
        }
    }
}

/// Reference to a task in this project or in an external project document.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum TaskRef {
    Local(TaskId),
    External { project: Uuid, task: TaskId },
}

impl TaskRef {
    pub fn local(&self) -> Option<TaskId> {
        match self {
            TaskRef::Local(id) => Some(*id),
            TaskRef::External { .. } => None,
        }
    }
}

impl From<TaskId> for TaskRef {
    fn from(id: TaskId) -> Self {
        TaskRef::Local(id)
    }
}

/// A predecessor link stored on the successor task.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Dependency {
    pub predecessor: TaskRef,
    #[serde(default)]
    pub kind: LinkType,
    #[serde(default, skip_serializing_if = "Lag::is_zero")]
    pub lag: Lag,
}

impl Dependency {
    pub fn fs(pred: impl Into<TaskRef>) -> Self {
        Self { predecessor: pred.into(), kind: LinkType::FS, lag: Lag::None }
    }
}

/// How much effort a task is expected to take before resources are assigned.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Estimate {
    /// Relative complexity in points; converted with `ProjectSettings::minutes_per_point`.
    Complexity { points: f64 },
    /// Three-point (PERT) estimate of work in minutes.
    ThreePoint { optimistic: Minutes, most_likely: Minutes, pessimistic: Minutes },
    /// Direct work estimate in minutes.
    Work { minutes: Minutes },
}

impl Estimate {
    /// Expected work in minutes.
    pub fn expected_work(&self, settings: &ProjectSettings) -> Minutes {
        match *self {
            Estimate::Complexity { points } => {
                crate::limits::clamp_minutes((points * settings.minutes_per_point as f64).round() as Minutes)
            }
            Estimate::ThreePoint { optimistic, most_likely, pessimistic } => {
                ((optimistic as f64 + 4.0 * most_likely as f64 + pessimistic as f64) / 6.0).round() as Minutes
            }
            Estimate::Work { minutes } => minutes,
        }
    }

    /// Standard deviation of a three-point estimate (0 for other methods).
    pub fn std_dev(&self) -> Minutes {
        match *self {
            Estimate::ThreePoint { optimistic, pessimistic, .. } => ((pessimistic as f64 - optimistic as f64) / 6.0).round() as Minutes,
            _ => 0,
        }
    }
}

/// A snapshot of planned values used to measure variance.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    pub start: NaiveDateTime,
    pub finish: NaiveDateTime,
    pub duration: Minutes,
    pub work: Minutes,
    pub cost: f64,
    pub saved: NaiveDateTime,
}

/// Values computed by the scheduler. Never edit directly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct TaskSchedule {
    pub start: Option<NaiveDateTime>,
    pub finish: Option<NaiveDateTime>,
    pub early_start: Option<NaiveDateTime>,
    pub early_finish: Option<NaiveDateTime>,
    pub late_start: Option<NaiveDateTime>,
    pub late_finish: Option<NaiveDateTime>,
    /// Working minutes the task can slip without delaying the project.
    pub total_slack: Minutes,
    /// Working minutes the task can slip without delaying any successor.
    pub free_slack: Minutes,
    pub critical: bool,
    pub is_summary: bool,
    /// Planned work (assignments, or the estimate when unassigned).
    pub work: Minutes,
    pub actual_work: Minutes,
    pub cost: f64,
    /// Duration of summary tasks (computed span); leaf tasks mirror `Task::duration`.
    pub duration: Minutes,
    pub percent_complete: f32,
    /// Human-readable scheduling problems (constraint conflicts, missed deadlines...).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub name: String,
    #[serde(default = "one_u16")]
    pub outline_level: u16,
    #[serde(default)]
    pub kind: TaskKind,
    #[serde(default)]
    pub mode: ScheduleMode,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub active: bool,
    /// Planned duration in working minutes.
    #[serde(default)]
    pub duration: Minutes,
    /// Duration is a placeholder estimate (shown with a trailing `?`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub estimated: bool,
    #[serde(default)]
    pub task_type: TaskType,
    #[serde(default, skip_serializing_if = "is_false")]
    pub effort_driven: bool,
    #[serde(default)]
    pub constraint: Constraint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<NaiveDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignore_resource_calendars: bool,
    #[serde(default = "default_priority")]
    pub priority: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predecessors: Vec<Dependency>,
    /// For milestones: tasks whose completion makes up this milestone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<TaskRef>,
    /// For milestones: the output achieved when the milestone is reached.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub deliverable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Estimate>,
    #[serde(default)]
    pub percent_complete: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_start: Option<NaiveDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_finish: Option<NaiveDateTime>,
    /// Manually scheduled start (manual mode) or user-typed start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_start: Option<NaiveDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_finish: Option<NaiveDateTime>,
    /// Delay added by resource leveling (working minutes).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub leveling_delay: Minutes,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub fixed_cost: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Baseline>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub show_on_timeline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar_color: Option<[u8; 3]>,
    #[serde(default)]
    pub sched: TaskSchedule,
}

fn one_u16() -> u16 {
    1
}
fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}
fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

impl Task {
    pub fn new(id: TaskId, name: impl Into<String>, minutes_per_day: Minutes) -> Self {
        Self {
            id,
            name: name.into(),
            outline_level: 1,
            kind: TaskKind::Task,
            mode: ScheduleMode::Auto,
            active: true,
            duration: minutes_per_day,
            estimated: true,
            task_type: TaskType::FixedUnits,
            effort_driven: false,
            constraint: Constraint::default(),
            deadline: None,
            calendar: None,
            ignore_resource_calendars: false,
            priority: 500,
            predecessors: Vec::new(),
            members: Vec::new(),
            deliverable: String::new(),
            estimate: None,
            percent_complete: 0,
            actual_start: None,
            actual_finish: None,
            manual_start: None,
            manual_finish: None,
            leveling_delay: 0,
            fixed_cost: 0.0,
            notes: String::new(),
            baseline: None,
            show_on_timeline: false,
            bar_color: None,
            sched: TaskSchedule::default(),
        }
    }

    pub fn milestone(id: TaskId, name: impl Into<String>) -> Self {
        let mut t = Self::new(id, name, 0);
        t.kind = TaskKind::Milestone;
        t.duration = 0;
        t.estimated = false;
        t
    }

    pub fn is_milestone(&self) -> bool {
        self.kind == TaskKind::Milestone
    }

    pub fn is_summary(&self) -> bool {
        self.sched.is_summary
    }

    pub fn is_complete(&self) -> bool {
        self.percent_complete >= 100
    }

    /// Whether adding or removing people keeps total work the same. Fixed Work tasks always are.
    pub fn is_effort_driven(&self) -> bool {
        self.effort_driven || self.task_type == TaskType::FixedWork
    }
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// How a resource is costed and consumed.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    /// People, teams, companies or equipment that spend working time.
    #[default]
    Work,
    /// Consumables measured in units (`material_label`).
    Material,
    /// A fixed cost item (travel, licences...).
    Cost,
}

/// Who or what a resource is.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCategory {
    #[default]
    TeamMember,
    Team,
    PartnerCompany,
    Vendor,
    Contractor,
    Equipment,
    Facility,
    Other,
}

impl ResourceCategory {
    pub const ALL: [ResourceCategory; 8] = [
        ResourceCategory::TeamMember,
        ResourceCategory::Team,
        ResourceCategory::PartnerCompany,
        ResourceCategory::Vendor,
        ResourceCategory::Contractor,
        ResourceCategory::Equipment,
        ResourceCategory::Facility,
        ResourceCategory::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ResourceCategory::TeamMember => "Team member",
            ResourceCategory::Team => "Team",
            ResourceCategory::PartnerCompany => "Partner company",
            ResourceCategory::Vendor => "Vendor",
            ResourceCategory::Contractor => "Contractor",
            ResourceCategory::Equipment => "Equipment",
            ResourceCategory::Facility => "Facility",
            ResourceCategory::Other => "Other",
        }
    }
}

/// A period during which a resource's maximum units differ from the default.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct AvailabilityPeriod {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<NaiveDate>,
    /// 1.0 = 100%.
    pub units: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Resource {
    pub id: ResourceId,
    pub name: String,
    #[serde(default)]
    pub kind: ResourceType,
    #[serde(default)]
    pub category: ResourceCategory,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub initials: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    /// Label of material units, e.g. "tons".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub material_label: String,
    /// Maximum units of work available (1.0 = one full-time person).
    #[serde(default = "default_units")]
    pub max_units: f64,
    /// Date-bound overrides of `max_units`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub availability: Vec<AvailabilityPeriod>,
    /// Cost per hour of work (or per unit for material resources).
    #[serde(default)]
    pub standard_rate: f64,
    #[serde(default)]
    pub overtime_rate: f64,
    #[serde(default)]
    pub cost_per_use: f64,
    /// Base calendar; `None` uses the project calendar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<CalendarId>,
    /// Personal non-working time / altered working time (vacations, part-time days).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions: Vec<CalendarException>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

impl Resource {
    pub fn new(id: ResourceId, name: impl Into<String>) -> Self {
        let name = name.into();
        let initials = name.split_whitespace().filter_map(|w| w.chars().next()).take(3).collect::<String>().to_uppercase();
        Self {
            id,
            name,
            kind: ResourceType::Work,
            category: ResourceCategory::TeamMember,
            initials,
            group: String::new(),
            email: String::new(),
            material_label: String::new(),
            max_units: 1.0,
            availability: Vec::new(),
            standard_rate: 0.0,
            overtime_rate: 0.0,
            cost_per_use: 0.0,
            calendar: None,
            exceptions: Vec::new(),
            notes: String::new(),
        }
    }

    /// Maximum units available on a date, honoring availability periods.
    pub fn units_on(&self, d: NaiveDate) -> f64 {
        self.availability
            .iter()
            .rev()
            .find(|p| p.from.is_none_or(|f| d >= f) && p.to.is_none_or(|t| d <= t))
            .map(|p| p.units)
            .unwrap_or(self.max_units)
    }
}

// ---------------------------------------------------------------------------
// Assignments & time tracking
// ---------------------------------------------------------------------------

/// Shape of how an assignment's work is spread across its span.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkContour {
    #[default]
    Flat,
    FrontLoaded,
    BackLoaded,
    Bell,
}

impl WorkContour {
    pub const ALL: [WorkContour; 4] = [WorkContour::Flat, WorkContour::FrontLoaded, WorkContour::BackLoaded, WorkContour::Bell];

    /// Relative intensity at position `x` in `[0, 1]` across the assignment.
    pub fn weight(self, x: f64) -> f64 {
        match self {
            WorkContour::Flat => 1.0,
            WorkContour::FrontLoaded => 1.75 - 1.5 * x,
            WorkContour::BackLoaded => 0.25 + 1.5 * x,
            WorkContour::Bell => 0.2 + 1.6 * (1.0 - (2.0 * x - 1.0).powi(2)),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WorkContour::Flat => "Flat",
            WorkContour::FrontLoaded => "Front loaded",
            WorkContour::BackLoaded => "Back loaded",
            WorkContour::Bell => "Bell",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Assignment {
    pub id: AssignmentId,
    pub task: TaskId,
    pub resource: ResourceId,
    /// Units of the resource applied (1.0 = 100%); quantity for material resources.
    #[serde(default = "default_units")]
    pub units: f64,
    /// Planned work in minutes (work resources).
    #[serde(default)]
    pub work: Minutes,
    /// Actual work entered directly, in addition to tracked time entries.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub actual_work: Minutes,
    /// Fixed cost of the assignment (cost resources).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub cost: f64,
    #[serde(default)]
    pub contour: WorkContour,
}

/// Time spent by a resource on a task, as entered on a timesheet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TimeEntry {
    pub id: TimeEntryId,
    pub task: TaskId,
    pub resource: ResourceId,
    pub date: NaiveDate,
    pub minutes: Minutes,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

// ---------------------------------------------------------------------------
// Cross-document links
// ---------------------------------------------------------------------------

/// Last known state of a task in another document.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExternalTaskSnapshot {
    pub id: TaskId,
    pub name: String,
    pub start: Option<NaiveDateTime>,
    pub finish: Option<NaiveDateTime>,
    #[serde(default)]
    pub percent_complete: u8,
    #[serde(default)]
    pub milestone: bool,
    #[serde(default)]
    pub duration: Minutes,
}

/// Another `.plancraft` document whose tasks are referenced by this project.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExternalProject {
    /// Project id stored inside the other document.
    pub id: Uuid,
    pub name: String,
    /// Path to the other document, relative to this document's directory when possible.
    pub path: String,
    /// Cached copies of referenced tasks so scheduling works when the file is unavailable.
    #[serde(default)]
    pub tasks: Vec<ExternalTaskSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<NaiveDateTime>,
}

impl ExternalProject {
    pub fn task(&self, id: TaskId) -> Option<&ExternalTaskSnapshot> {
        self.tasks.iter().find(|t| t.id == id)
    }
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProjectSettings {
    #[serde(flatten)]
    pub time: TimeSettings,
    pub duration_unit: DurationUnit,
    pub currency: String,
    /// Work represented by one complexity point.
    pub minutes_per_point: Minutes,
    /// Tasks with total slack at or below this are critical.
    pub critical_slack: Minutes,
    /// New tasks default to auto scheduling.
    pub new_tasks_auto: bool,
    /// Assigned tasks follow resource calendars (working time common to all assigned resources).
    pub use_resource_calendars: bool,
    /// Update task % complete automatically from tracked time.
    pub progress_from_time: bool,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            time: TimeSettings::default(),
            duration_unit: DurationUnit::Days,
            currency: "$".into(),
            minutes_per_point: 4 * 60,
            critical_slack: 0,
            new_tasks_auto: true,
            use_resource_calendars: true,
            progress_from_time: false,
        }
    }
}

/// Values computed for the whole project.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ProjectSchedule {
    pub start: Option<NaiveDateTime>,
    pub finish: Option<NaiveDateTime>,
    pub work: Minutes,
    pub actual_work: Minutes,
    pub cost: f64,
    pub percent_complete: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub calculated_at: Option<NaiveDateTime>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub format: String,
    pub version: u32,
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub manager: String,
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub notes: String,
    pub start: NaiveDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_date: Option<NaiveDate>,
    pub calendar: CalendarId,
    #[serde(default)]
    pub settings: ProjectSettings,
    pub calendars: Vec<Calendar>,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub resources: Vec<Resource>,
    #[serde(default)]
    pub assignments: Vec<Assignment>,
    #[serde(default)]
    pub time_entries: Vec<TimeEntry>,
    #[serde(default)]
    pub external_projects: Vec<ExternalProject>,
    #[serde(default)]
    pub ids: IdCounters,
    #[serde(default)]
    pub sched: ProjectSchedule,
}

impl Project {
    /// Creates an empty project starting at 08:00 on `start`.
    pub fn new(name: impl Into<String>, start: NaiveDate) -> Self {
        let mut ids = IdCounters::default();
        let std_id = ids.next_calendar();
        let h24 = ids.next_calendar();
        let night = ids.next_calendar();
        let settings = ProjectSettings::default();
        let start_time = crate::calendar::shift(start.and_time(chrono::NaiveTime::MIN), Minutes::from(settings.time.default_start_time));
        Self {
            format: FILE_FORMAT.into(),
            version: FILE_VERSION,
            id: Uuid::new_v4(),
            name: name.into(),
            author: String::new(),
            manager: String::new(),
            company: String::new(),
            notes: String::new(),
            start: start_time,
            status_date: None,
            calendar: std_id,
            settings,
            calendars: vec![Calendar::standard(std_id), Calendar::twenty_four_hours(h24), Calendar::night_shift(night)],
            tasks: Vec::new(),
            resources: Vec::new(),
            assignments: Vec::new(),
            time_entries: Vec::new(),
            external_projects: Vec::new(),
            ids,
            sched: ProjectSchedule::default(),
        }
    }

    pub fn calendar(&self, id: CalendarId) -> Option<&Calendar> {
        self.calendars.iter().find(|c| c.id == id)
    }

    /// The project's base calendar (falls back to the first calendar, then to a standard week).
    pub fn base_calendar(&self) -> &Calendar {
        static FALLBACK: std::sync::LazyLock<Calendar> = std::sync::LazyLock::new(|| Calendar::standard(CalendarId(0)));
        self.calendar(self.calendar).or_else(|| self.calendars.first()).unwrap_or(&FALLBACK)
    }

    pub fn task_index(&self, id: TaskId) -> Option<usize> {
        self.tasks.iter().position(|t| t.id == id)
    }

    pub fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn resource(&self, id: ResourceId) -> Option<&Resource> {
        self.resources.iter().find(|r| r.id == id)
    }

    pub fn resource_mut(&mut self, id: ResourceId) -> Option<&mut Resource> {
        self.resources.iter_mut().find(|r| r.id == id)
    }

    pub fn resource_by_name(&self, name: &str) -> Option<&Resource> {
        let n = name.trim();
        self.resources.iter().find(|r| r.name.eq_ignore_ascii_case(n) || (!r.initials.is_empty() && r.initials.eq_ignore_ascii_case(n)))
    }

    pub fn assignment(&self, id: AssignmentId) -> Option<&Assignment> {
        self.assignments.iter().find(|a| a.id == id)
    }

    pub fn assignments_for_task(&self, id: TaskId) -> impl Iterator<Item = &Assignment> {
        self.assignments.iter().filter(move |a| a.task == id)
    }

    pub fn assignments_for_resource(&self, id: ResourceId) -> impl Iterator<Item = &Assignment> {
        self.assignments.iter().filter(move |a| a.resource == id)
    }

    pub fn external(&self, id: Uuid) -> Option<&ExternalProject> {
        self.external_projects.iter().find(|e| e.id == id)
    }

    /// 1-based row number shown as "ID" in task tables.
    pub fn row_number(&self, id: TaskId) -> Option<usize> {
        self.task_index(id).map(|i| i + 1)
    }

    /// Index range of the descendants of the task at `idx` (exclusive of `idx`; empty when out of range).
    pub fn descendants_range(&self, idx: usize) -> std::ops::Range<usize> {
        let Some(level) = self.tasks.get(idx).map(|t| t.outline_level) else {
            return 0..0;
        };
        let first = idx + 1;
        let end = self.tasks.iter().skip(first).position(|t| t.outline_level <= level).map_or(self.tasks.len(), |k| first + k);
        first..end
    }

    /// Index of the parent summary of the task at `idx`.
    pub fn parent_index(&self, idx: usize) -> Option<usize> {
        let level = self.tasks.get(idx)?.outline_level;
        self.tasks.get(..idx)?.iter().rposition(|t| t.outline_level < level)
    }

    /// Whether the task at `idx` has children.
    pub fn has_children(&self, idx: usize) -> bool {
        match (self.tasks.get(idx), self.tasks.get(idx + 1)) {
            (Some(t), Some(next)) => next.outline_level > t.outline_level,
            _ => false,
        }
    }

    /// Outline number like "1.2.3" for the task at `idx`.
    pub fn wbs(&self, idx: usize) -> String {
        let mut counters: Vec<usize> = Vec::new();
        for t in self.tasks.iter().take(idx.saturating_add(1)) {
            let lvl = usize::from(t.outline_level.max(1));
            counters.resize(lvl, 0);
            if let Some(c) = counters.last_mut() {
                *c += 1;
            }
        }
        counters.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(".")
    }

    /// Display name of a task reference, e.g. `"Design"` or `"Website\Launch"`.
    pub fn task_ref_name(&self, r: &TaskRef) -> String {
        match r {
            TaskRef::Local(id) => self.task(*id).map(|t| t.name.clone()).unwrap_or_else(|| format!("<missing {id}>")),
            TaskRef::External { project, task } => match self.external(*project) {
                Some(e) => format!("{}\\{}", e.name, e.task(*task).map(|t| t.name.clone()).unwrap_or_else(|| task.to_string())),
                None => format!("<external {task}>"),
            },
        }
    }

    pub fn currency(&self) -> &str {
        &self.settings.currency
    }
}

//! UI state that is not part of the project document.

use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use plancraft_core::prelude::*;
use plancraft_core::workload::Granularity;

use crate::columns::TaskTable;
use crate::widgets::grid::GridState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewKind {
    Gantt,
    TrackingGantt,
    MultipleBaselines,
    NetworkDiagram,
    TaskUsage,
    Milestones,
    ResourceSheet,
    ResourceUsage,
    Workload,
    TeamPlanner,
    Timesheet,
    Calendar,
}

impl ViewKind {
    pub fn label(self) -> &'static str {
        match self {
            ViewKind::Gantt => "Gantt Chart",
            ViewKind::TrackingGantt => "Tracking Gantt",
            ViewKind::MultipleBaselines => "Multiple Baselines Gantt",
            ViewKind::NetworkDiagram => "Network Diagram",
            ViewKind::TaskUsage => "Task Usage",
            ViewKind::Milestones => "Milestones",
            ViewKind::ResourceSheet => "Resource Sheet",
            ViewKind::ResourceUsage => "Resource Usage",
            ViewKind::Workload => "Workload",
            ViewKind::TeamPlanner => "Team Planner",
            ViewKind::Timesheet => "Timesheet",
            ViewKind::Calendar => "Calendar",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            ViewKind::Gantt => "📊",
            ViewKind::TrackingGantt => "📈",
            ViewKind::MultipleBaselines => "📚",
            ViewKind::NetworkDiagram => "🔀",
            ViewKind::TaskUsage => "📝",
            ViewKind::Milestones => "♦",
            ViewKind::ResourceSheet => "👥",
            ViewKind::ResourceUsage => "📋",
            ViewKind::Workload => "📶",
            ViewKind::TeamPlanner => "👤",
            ViewKind::Timesheet => "⏱",
            ViewKind::Calendar => "📅",
        }
    }

    pub const ALL: [ViewKind; 12] = [
        ViewKind::Gantt,
        ViewKind::TrackingGantt,
        ViewKind::MultipleBaselines,
        ViewKind::NetworkDiagram,
        ViewKind::TaskUsage,
        ViewKind::Milestones,
        ViewKind::Calendar,
        ViewKind::ResourceSheet,
        ViewKind::ResourceUsage,
        ViewKind::Workload,
        ViewKind::TeamPlanner,
        ViewKind::Timesheet,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuTab {
    Task,
    Resource,
    Report,
    Project,
    View,
    Format,
}

impl MenuTab {
    pub const ALL: [MenuTab; 6] = [MenuTab::Task, MenuTab::Resource, MenuTab::Project, MenuTab::Report, MenuTab::View, MenuTab::Format];
    pub fn label(self) -> &'static str {
        match self {
            MenuTab::Task => "Task",
            MenuTab::Resource => "Resource",
            MenuTab::Report => "Report",
            MenuTab::Project => "Project",
            MenuTab::View => "View",
            MenuTab::Format => "Format",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TaskFilter {
    #[default]
    All,
    Critical,
    Incomplete,
    Completed,
    Milestones,
    Late,
    Unassigned,
    Summary,
}

impl TaskFilter {
    pub const ALL: [TaskFilter; 8] = [
        TaskFilter::All,
        TaskFilter::Critical,
        TaskFilter::Incomplete,
        TaskFilter::Completed,
        TaskFilter::Late,
        TaskFilter::Milestones,
        TaskFilter::Summary,
        TaskFilter::Unassigned,
    ];
    pub fn label(self) -> &'static str {
        match self {
            TaskFilter::All => "All Tasks",
            TaskFilter::Critical => "Critical",
            TaskFilter::Incomplete => "Incomplete Tasks",
            TaskFilter::Completed => "Completed Tasks",
            TaskFilter::Milestones => "Milestones",
            TaskFilter::Late => "Late Tasks",
            TaskFilter::Unassigned => "Unassigned Tasks",
            TaskFilter::Summary => "Summary Tasks",
        }
    }
}

/// Network Diagram drag in progress.
#[derive(Clone, Debug)]
pub enum NetDrag {
    /// Moving the view (diagram point under the pointer when the drag started).
    Pan { grab: egui::Pos2 },
    /// Drawing a link from a box.
    Link { from: TaskId },
}

/// Network Diagram view: where it is scrolled to, zoom, options and the cached layout.
pub struct NetworkState {
    /// Diagram point at the top-left corner of the view.
    pub offset: egui::Pos2,
    pub zoom: f32,
    pub opts: plancraft_core::network::NetworkOptions,
    pub show_summaries: bool,
    pub link_labels: bool,
    pub progress_marks: bool,
    pub drag: Option<NetDrag>,
    /// Zoom out to show the whole diagram on the next frame.
    pub fit_pending: bool,
    /// Scroll the selected task into view on the next frame.
    pub reveal_pending: bool,
    /// Zoom to the selected tasks on the next frame.
    pub fit_selected_pending: bool,
    /// Layout of the last frame, with the key it was made for.
    pub cache: Option<(u64, std::sync::Arc<plancraft_core::network::Diagram>)>,
    /// Document the view was last shown for (another one starts at the top left).
    pub shown_for: Option<u64>,
    /// Where the view was drawn on the last frame (for zooming about its centre).
    pub view_rect: egui::Rect,
}

impl Default for NetworkState {
    fn default() -> Self {
        Self {
            offset: egui::Pos2::ZERO,
            zoom: 1.0,
            opts: Default::default(),
            show_summaries: true,
            link_labels: false,
            progress_marks: true,
            drag: None,
            fit_pending: false,
            reveal_pending: true,
            fit_selected_pending: false,
            cache: None,
            shown_for: None,
            view_rect: egui::Rect::NOTHING,
        }
    }
}

/// Gantt-chart drag in progress.
#[derive(Clone, Debug)]
pub enum BarDrag {
    Move { task: TaskId, grab_dx: f32, origin_start: NaiveDateTime },
    Resize { task: TaskId },
    Progress { task: TaskId },
    Link { from: TaskId },
}

#[derive(Clone, Debug)]
pub struct GanttState {
    /// Time at the left edge of the chart.
    pub left: NaiveDateTime,
    pub px_per_day: f32,
    pub drag: Option<BarDrag>,
    pub show_critical: bool,
    pub show_baseline: bool,
    /// The baseline (0–10) the Gantt Chart and Tracking Gantt draw under each bar.
    pub baseline_slot: u8,
    /// Baselines shown in the Multiple Baselines Gantt (bit n = Baseline n).
    pub multi_baselines: u16,
    pub show_slack: bool,
    pub show_links: bool,
    pub show_resource_names: bool,
    pub show_progress: bool,
    pub show_project_summary: bool,
    pub initialized_for: Option<uuid::Uuid>,
}

impl Default for GanttState {
    fn default() -> Self {
        Self {
            left: chrono::Local::now().naive_local(),
            px_per_day: 24.0,
            drag: None,
            show_critical: false,
            show_baseline: false,
            baseline_slot: 0,
            multi_baselines: 0b111,
            show_slack: false,
            show_links: true,
            show_resource_names: true,
            show_progress: true,
            show_project_summary: false,
            initialized_for: None,
        }
    }
}

impl GanttState {
    pub fn x_of(&self, chart_left: f32, t: NaiveDateTime) -> f32 {
        let mins = (t - self.left).num_minutes() as f32;
        chart_left + mins / 1440.0 * self.px_per_day
    }

    pub fn time_at(&self, chart_left: f32, x: f32) -> NaiveDateTime {
        let days = (x - chart_left) / self.px_per_day;
        self.left + Duration::minutes((days * 1440.0) as i64)
    }

    /// Zoom presets (px per day), from years to hours.
    pub const ZOOMS: [f32; 9] = [1.0, 2.5, 5.0, 10.0, 16.0, 24.0, 40.0, 72.0, 140.0];

    pub fn zoom(&mut self, dir: i32, anchor_x: f32, chart_left: f32) {
        let anchor_t = self.time_at(chart_left, anchor_x);
        let idx = Self::ZOOMS.iter().position(|z| *z >= self.px_per_day - 0.01).unwrap_or(5) as i32;
        let n = (idx + dir).clamp(0, Self::ZOOMS.len() as i32 - 1) as usize;
        self.px_per_day = Self::ZOOMS.get(n).copied().unwrap_or(self.px_per_day);
        // Keep the anchor time under the cursor.
        let days = (anchor_x - chart_left) / self.px_per_day;
        self.left = anchor_t - Duration::minutes((days * 1440.0) as i64);
    }

    /// Continuous zoom by a factor, keeping the time under `anchor_x` fixed.
    pub fn zoom_by(&mut self, factor: f32, anchor_x: f32, chart_left: f32) {
        let anchor_t = self.time_at(chart_left, anchor_x);
        self.px_per_day = (self.px_per_day * factor).clamp(0.5, 160.0);
        let days = (anchor_x - chart_left) / self.px_per_day;
        self.left = anchor_t - Duration::minutes((days * 1440.0) as i64);
    }

    pub fn scroll_to(&mut self, t: NaiveDateTime) {
        self.left = t - Duration::days(2);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub at: Instant,
}

#[derive(Default)]
pub struct Selection {
    pub tasks: BTreeSet<TaskId>,
    pub anchor: Option<TaskId>,
    pub resources: BTreeSet<ResourceId>,
    pub resource_anchor: Option<ResourceId>,
}

impl Selection {
    pub fn single_task(&self) -> Option<TaskId> {
        if self.tasks.len() == 1 { self.tasks.iter().next().copied() } else { self.anchor.filter(|a| self.tasks.contains(a)) }
    }
    pub fn select_task(&mut self, id: TaskId) {
        self.tasks.clear();
        self.tasks.insert(id);
        self.anchor = Some(id);
    }
    pub fn single_resource(&self) -> Option<ResourceId> {
        if self.resources.len() == 1 {
            self.resources.iter().next().copied()
        } else {
            self.resource_anchor.filter(|a| self.resources.contains(a))
        }
    }
    pub fn select_resource(&mut self, id: ResourceId) {
        self.resources.clear();
        self.resources.insert(id);
        self.resource_anchor = Some(id);
    }
}

/// How a document's task views are sorted and grouped (kept per document).
#[derive(Clone, Debug, Default)]
pub struct ViewOrder {
    pub sort: Option<(String, bool)>,
    pub group: Option<String>,
    pub collapsed_groups: HashSet<String>,
}

pub struct UiState {
    pub view: ViewKind,
    pub menu_tab: MenuTab,
    /// The Start Page is shown instead of the views.
    pub start_page: bool,
    pub sel: Selection,
    pub task_grid: GridState,
    pub resource_grid: GridState,
    pub usage_grid: GridState,
    pub misc_grid: GridState,
    pub collapsed: HashSet<TaskId>,
    pub collapsed_resources: HashSet<ResourceId>,
    pub filter: TaskFilter,
    pub search: String,
    /// Sort of task views: field key and descending.
    pub sort: Option<(String, bool)>,
    /// Field key task views are grouped by.
    pub group: Option<String>,
    /// Collapsed group headers (by label).
    pub collapsed_groups: HashSet<String>,
    /// Sort, grouping and collapsed groups of the documents not active now (by handle): field
    /// keys like `custom:3` mean different fields in different plans.
    pub view_orders: std::collections::HashMap<u64, ViewOrder>,
    /// The task grid's cursor row at the end of the last frame and the task on it, so the cursor
    /// (and an open editor) follow that task when the rows re-sort or regroup.
    pub cursor_task: Option<(usize, TaskId)>,
    /// Column widths of task tables, by table and field key (survive column changes).
    pub column_widths: std::collections::HashMap<(TaskTable, String), f32>,
    pub table: TaskTable,
    pub gantt: GanttState,
    pub network: NetworkState,
    pub table_width: f32,
    pub timeline_visible: bool,
    pub granularity: Granularity,
    pub usage_from: Option<NaiveDate>,
    pub timesheet_week: NaiveDate,
    pub timesheet_resource: Option<ResourceId>,
    pub calendar_month: NaiveDate,
    pub toasts: Vec<Toast>,
    pub clipboard: Vec<Task>,
    pub show_history: bool,
}

impl Default for UiState {
    fn default() -> Self {
        let today = chrono::Local::now().date_naive();
        let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
        Self {
            view: ViewKind::Gantt,
            menu_tab: MenuTab::Task,
            start_page: false,
            sel: Selection::default(),
            task_grid: GridState::default(),
            resource_grid: GridState::default(),
            usage_grid: GridState::default(),
            misc_grid: GridState::default(),
            collapsed: HashSet::new(),
            collapsed_resources: HashSet::new(),
            filter: TaskFilter::All,
            search: String::new(),
            sort: None,
            group: None,
            collapsed_groups: HashSet::new(),
            view_orders: Default::default(),
            cursor_task: None,
            column_widths: std::collections::HashMap::new(),
            table: TaskTable::Entry,
            gantt: GanttState::default(),
            network: NetworkState::default(),
            table_width: 720.0,
            timeline_visible: true,
            granularity: Granularity::Day,
            usage_from: None,
            timesheet_week: monday,
            timesheet_resource: None,
            calendar_month: today.with_day(1).unwrap_or(today),
            toasts: Vec::new(),
            clipboard: Vec::new(),
            show_history: false,
        }
    }
}

impl UiState {
    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), kind, at: Instant::now() });
    }
}

//! User commands issued from the menu bar, toolbar, menus, keyboard shortcuts and views.

use plancraft_core::model::ScheduleMode;

use crate::columns::TaskTable;
use crate::state::{TaskFilter, ViewKind};

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    // File
    NewProject,
    NewDemo,
    Open,
    OpenPath(std::path::PathBuf),
    Save,
    SaveAs,
    Close,
    Undo,
    Redo,
    ExportCsv,
    Quit,
    StartPage,
    // Tasks
    InsertTask,
    InsertMilestone,
    InsertSummary,
    DeleteSelection,
    Indent,
    Outdent,
    MoveUp,
    MoveDown,
    LinkSelected,
    UnlinkSelected,
    SetPercent(u8),
    MarkOnTrack,
    ToggleActive,
    SetMode(ScheduleMode),
    TaskInfo,
    TaskNotes,
    ToggleTimeline,
    ScrollToTask,
    Cut,
    Copy,
    Paste,
    // Resources
    InsertResource,
    DeleteResources,
    ResourceInfo,
    AssignResources,
    LevelAll,
    ClearLeveling,
    // Project
    ProjectInfo,
    ChangeWorkingTime,
    SetBaseline,
    ClearBaseline,
    UpdateProgressFromTime,
    ExternalLinks,
    RefreshExternal,
    Statistics,
    McpPanel,
    // View
    SetView(ViewKind),
    ZoomIn,
    ZoomOut,
    ZoomEntire,
    ZoomSelected,
    Filter(TaskFilter),
    Table(TaskTable),
    CollapseAll,
    ExpandAll,
    History,
    SetTheme(crate::theme::ThemeChoice),
    About,
}

/// Context menu shared by the task table and the Gantt chart.
pub fn task_context_menu(ui: &mut egui::Ui, emit: &mut dyn FnMut(Command)) {
    let mut item = |ui: &mut egui::Ui, label: &str, cmd: Command| {
        if ui.button(label).clicked() {
            emit(cmd);
            ui.close();
        }
    };
    item(ui, "ℹ  Task Information…", Command::TaskInfo);
    item(ui, "📝  Notes…", Command::TaskNotes);
    item(ui, "👥  Assign Resources…", Command::AssignResources);
    ui.separator();
    item(ui, "➕  Insert Task", Command::InsertTask);
    item(ui, "♦  Insert Milestone", Command::InsertMilestone);
    item(ui, "🗑  Delete Task", Command::DeleteSelection);
    ui.separator();
    item(ui, "➡  Indent", Command::Indent);
    item(ui, "⬅  Outdent", Command::Outdent);
    item(ui, "🔗  Link Tasks", Command::LinkSelected);
    item(ui, "⛓  Unlink Tasks", Command::UnlinkSelected);
    ui.separator();
    item(ui, "✔  Mark 100% Complete", Command::SetPercent(100));
    item(ui, "📅  Add to Timeline", Command::ToggleTimeline);
    item(ui, "⏸  Inactivate / Activate", Command::ToggleActive);
    ui.separator();
    item(ui, "✂  Cut", Command::Cut);
    item(ui, "📋  Copy", Command::Copy);
    item(ui, "📥  Paste", Command::Paste);
}

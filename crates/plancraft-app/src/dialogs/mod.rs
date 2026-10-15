//! Modal-style windows.

pub mod mcp_panel;
pub mod misc;
pub mod project_info;
pub mod resource_info;
pub mod task_info;
pub mod working_time;

#[derive(Default)]
pub struct Dialogs {
    pub task_info: Option<task_info::TaskInfo>,
    pub resource_info: Option<resource_info::ResourceInfo>,
    pub project_info: Option<project_info::ProjectInfo>,
    pub working_time: Option<working_time::WorkingTime>,
    pub assign: Option<misc::AssignResources>,
    pub external: Option<misc::ExternalLinks>,
    pub stats: bool,
    pub about: bool,
    pub history: bool,
    pub mcp: bool,
}

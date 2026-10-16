//! Strongly-typed identifiers.
//!
//! Every entity in a project carries a *unique id* that never changes for the
//! lifetime of the entity (similar to Microsoft Project's "Unique ID").  Row
//! numbers shown in the UI are derived from position and are not stored.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }
    };
}

id_type!(
    /// Unique id of a task (including milestones and summary tasks).
    TaskId,
    "T"
);
id_type!(
    /// Unique id of a resource.
    ResourceId,
    "R"
);
id_type!(
    /// Unique id of a calendar.
    CalendarId,
    "C"
);
id_type!(
    /// Unique id of a resource assignment.
    AssignmentId,
    "A"
);
id_type!(
    /// Unique id of a tracked time entry.
    TimeEntryId,
    "E"
);

/// Monotonic id allocator persisted with the project so ids are never reused.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct IdCounters {
    pub task: u32,
    pub resource: u32,
    pub calendar: u32,
    pub assignment: u32,
    pub time_entry: u32,
}

impl IdCounters {
    pub fn next_task(&mut self) -> TaskId {
        self.task = self.task.saturating_add(1);
        TaskId(self.task)
    }
    pub fn next_resource(&mut self) -> ResourceId {
        self.resource = self.resource.saturating_add(1);
        ResourceId(self.resource)
    }
    pub fn next_calendar(&mut self) -> CalendarId {
        self.calendar = self.calendar.saturating_add(1);
        CalendarId(self.calendar)
    }
    pub fn next_assignment(&mut self) -> AssignmentId {
        self.assignment = self.assignment.saturating_add(1);
        AssignmentId(self.assignment)
    }
    pub fn next_time_entry(&mut self) -> TimeEntryId {
        self.time_entry = self.time_entry.saturating_add(1);
        TimeEntryId(self.time_entry)
    }
}

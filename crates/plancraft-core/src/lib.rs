//! # PlanCraft core
//!
//! Project model, working-time calendars, critical-path scheduling, workload
//! analysis, resource leveling, time tracking and document I/O for PlanCraft.
//!
//! ```
//! use plancraft_core::prelude::*;
//! use chrono::NaiveDate;
//!
//! let mut p = Project::new("Example", NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
//! let a = p.add_task("Design", None);
//! let b = p.add_task("Build", None);
//! p.link(a.into(), b, LinkType::FS, Lag::None).unwrap();
//! recalculate(&mut p);
//! assert!(p.task(b).unwrap().sched.start > p.task(a).unwrap().sched.start);
//! ```

pub mod calendar;
pub mod duration;
pub mod ids;
pub mod io;
pub mod leveling;
pub mod model;
pub mod ops;
pub mod report;
pub mod sample;
pub mod schedule;
pub mod workload;
pub mod workspace;

pub mod prelude {
    pub use crate::calendar::{Calendar, CalendarException, TimeRange, WorkCal, WorkWeek};
    pub use crate::duration::{
        DurationUnit, Lag, Minutes, TimeSettings, format_duration, format_work, parse_duration,
        parse_work,
    };
    pub use crate::ids::*;
    pub use crate::model::*;
    pub use crate::ops::{PlanError, PlanResult};
    pub use crate::schedule::{ScheduleReport, recalculate};
    pub use crate::workload::{Granularity, WorkloadReport, workload};
}

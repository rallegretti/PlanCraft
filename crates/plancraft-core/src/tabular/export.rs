//! A plan's task list as CSV, for spreadsheets. Reading the file back with this module's import
//! gives the same tasks, outline, durations, links, resources and progress.

use crate::dates::prelude::*;
use crate::model::Project;

/// The fixed columns, before the plan's custom fields.
const HEADINGS: &str =
    "ID,Unique ID,WBS,Outline Level,Name,Milestone,Duration,Start,Finish,% Complete,Work (h),Cost,Predecessors,Resources,Critical";

/// A cell, quoted. A cell a spreadsheet would run as a formula (starting with `=`, `+`, `-` or
/// `@`, or a tab or line break, also after leading spaces, and the full-width forms) gets an
/// apostrophe in front, so it stays text.
fn quote(v: &str) -> String {
    let risky = v.starts_with(['\t', '\r', '\n']) || v.trim_start().starts_with(['=', '+', '-', '@', '＝', '＋', '－', '＠']);
    let guarded = if risky { format!("'{v}") } else { v.to_string() };
    format!("\"{}\"", guarded.replace('"', "\"\""))
}

fn iso(dt: Option<DateTime>) -> String {
    dt.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// The task list as CSV text: a byte order mark (so Excel reads accented names correctly), a
/// heading row, and a row per task.
pub fn to_csv(p: &Project) -> String {
    let mut s = format!("\u{feff}{HEADINGS}");
    for f in &p.custom_fields {
        s.push(',');
        s.push_str(&quote(&f.name));
    }
    s.push('\n');
    for (i, t) in p.tasks.iter().enumerate() {
        s += &format!(
            "{},{},{},{},{},{},{},{},{},{:.0},{:.2},{:.2},{},{},{}",
            i.saturating_add(1),
            t.id.0,
            quote(&p.wbs(i)),
            t.outline_level,
            quote(&t.name),
            t.is_milestone(),
            quote(&p.format_task_duration(t)),
            iso(t.sched.start),
            iso(t.sched.finish),
            t.sched.percent_complete,
            t.sched.work as f64 / 60.0,
            t.sched.cost,
            quote(&p.format_predecessors(t.id)),
            quote(&p.format_resource_names(t.id)),
            t.sched.critical
        );
        for f in &p.custom_fields {
            let v = t.custom.get(&f.id).map(|v| p.custom_value_text(f.id, v)).unwrap_or_default();
            s.push(',');
            s.push_str(&quote(&v));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_stay_text() {
        assert_eq!(quote("=1+1"), "\"'=1+1\"");
        assert_eq!(quote("  @SUM(A1)"), "\"'  @SUM(A1)\"");
        assert_eq!(quote("Say \"hi\""), "\"Say \"\"hi\"\"\"");
        assert_eq!(quote("Plain"), "\"Plain\"");
    }
}

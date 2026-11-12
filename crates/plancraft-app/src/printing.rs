//! Printing support: turns the current view into a printable sheet (the same columns, rows,
//! sort, grouping and filter as on screen), and hands PDFs to the system's print service.
//!
//! Printing goes through the operating system's own command-line print service (CUPS `lp` on
//! Linux and macOS); on Windows the PDF opens in the default PDF viewer to print from there.
//! Nothing is linked in: PlanCraft writes the PDF itself.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use chrono::NaiveDate;

use plancraft_core::prelude::*;
use plancraft_core::print::layout::Palette;
use plancraft_core::print::{self, Align, ChartOptions, PrintCell, PrintColumn, PrintJob, PrintRow, RowKind, Sheet, font};

use crate::columns::{CellCtx, TaskCol};
use crate::fields::{self, Field};
use crate::state::{UiState, ViewKind};
use crate::theme;
use crate::views::{self, Row};

/// Screen pixels per printed point (columns and indents print at 3/4 of their on-screen size).
const PT_PER_PX: f32 = 0.75;

/// What to print.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrintView {
    Gantt,
    Tracking,
    TaskTable,
    Resources,
}

impl PrintView {
    pub const ALL: [PrintView; 4] = [PrintView::Gantt, PrintView::Tracking, PrintView::TaskTable, PrintView::Resources];

    pub fn label(self) -> &'static str {
        match self {
            PrintView::Gantt => "Gantt Chart",
            PrintView::Tracking => "Tracking Gantt",
            PrintView::TaskTable => "Task Table",
            PrintView::Resources => "Resource Sheet",
        }
    }

    /// The printable view closest to what is on screen.
    pub fn for_view(v: ViewKind) -> Self {
        match v {
            ViewKind::TrackingGantt => PrintView::Tracking,
            ViewKind::ResourceSheet | ViewKind::ResourceUsage | ViewKind::Workload | ViewKind::TeamPlanner => PrintView::Resources,
            _ => PrintView::Gantt,
        }
    }
}

/// A cell's text as it prints: icon-only cells print nothing; glyphs the print font lacks fall
/// back to the cell's plain text (the Mode column's symbol prints as "Auto Scheduled").
fn printable(cell: &crate::widgets::grid::Cell) -> String {
    let ok = |s: &str| s.chars().all(font::has_glyph);
    if ok(&cell.text) {
        return cell.text.clone();
    }
    [&cell.tooltip, &cell.edit_text].into_iter().flatten().find(|s| ok(s)).cloned().unwrap_or_default()
}

fn print_cell(cell: &crate::widgets::grid::Cell, pal: &Palette) -> PrintCell {
    PrintCell {
        text: printable(cell),
        bold: cell.bold,
        // Only meaningful colours print (paper is white whatever the screen theme).
        color: cell.color.filter(|c| *c == theme::pal().ERROR).map(|_| pal.error),
        indent: cell.indent * PT_PER_PX,
    }
}

/// The task table of the current view, with the row number first (as predecessors refer to it).
/// Unless `all_columns`, only the columns at least partly visible left of the view's divider
/// print (as in the view on screen).
fn task_sheet(p: &Project, st: &UiState, title: &str, pal: &Palette, all_columns: bool) -> Sheet {
    let mut fields: Vec<Field> = Vec::new();
    let mut x = crate::widgets::grid::ROW_NUM_W;
    for f in fields::table_fields(p, st.table) {
        let w = st.column_widths.get(&(st.table, f.key())).copied().unwrap_or_else(|| f.def(p).width);
        if !all_columns && x >= st.table_width && !fields.is_empty() {
            break;
        }
        x += w;
        if f != Field::Builtin(TaskCol::Indicators) {
            fields.push(f);
        }
    }
    let mut columns = vec![PrintColumn { title: "ID".into(), width: 26.0, align: Align::Right }];
    for f in &fields {
        let def = f.def(p);
        let w = st.column_widths.get(&(st.table, f.key())).copied().unwrap_or(def.width);
        let align = match def.align {
            egui::Align::Min => Align::Left,
            egui::Align::Center => Align::Center,
            egui::Align::Max => Align::Right,
        };
        columns.push(PrintColumn { title: f.title(p), width: w * PT_PER_PX, align });
    }
    let no_overallocation = |_: TaskId| false;
    let ctx = CellCtx { overallocated: &no_overallocation };
    let grouped = st.group.is_some();
    let rows = views::visible_rows(p, st)
        .iter()
        .filter_map(|row| match row {
            Row::Task(idx) => {
                let t = p.tasks.get(*idx)?;
                let mut cells = vec![PrintCell::text((idx + 1).to_string())];
                for f in &fields {
                    let mut c = print_cell(&f.cell(p, *idx, &ctx), pal);
                    if grouped {
                        c.indent = 0.0;
                    }
                    cells.push(c);
                }
                Some(PrintRow { kind: RowKind::Task(t.id), cells })
            }
            Row::Group(g) => {
                let tasks = g.tasks.iter().filter_map(|&i| p.tasks.get(i));
                let start = tasks.clone().filter_map(|t| t.sched.start).min();
                let finish = tasks.filter_map(|t| t.sched.finish).max();
                let mut cells = vec![PrintCell::default()];
                cells.extend(fields.iter().map(|f| print_cell(&views::gantt::group_cell(*f, p, g), pal)));
                Some(PrintRow { kind: RowKind::Group { start, finish }, cells })
            }
        })
        .collect();
    Sheet { title: title.into(), columns, rows }
}

/// The sheet and chart options to print `view`.
pub fn sheet(p: &Project, st: &UiState, view: PrintView, all_columns: bool) -> (Sheet, Option<ChartOptions>) {
    let pal = Palette::default();
    let g = &st.gantt;
    let chart = ChartOptions {
        critical: g.show_critical,
        baseline: g.show_baseline,
        progress: g.show_progress,
        links: g.show_links,
        slack: g.show_slack,
        resource_names: g.show_resource_names,
        per_day: g.px_per_day * PT_PER_PX,
    };
    match view {
        PrintView::Gantt => (task_sheet(p, st, view.label(), &pal, all_columns), Some(chart)),
        PrintView::Tracking => (
            task_sheet(p, st, view.label(), &pal, all_columns),
            Some(ChartOptions { critical: true, baseline: true, progress: true, ..chart }),
        ),
        // Without a chart there is no divider: the whole table prints.
        PrintView::TaskTable => (task_sheet(p, st, view.label(), &pal, true), None),
        PrintView::Resources => (views::resource_sheet::print_sheet(p, &pal), None),
    }
}

/// Lays out `view` with `setup` over `from..=to` (the whole project when `None`).
pub fn document(
    p: &Project,
    st: &UiState,
    view: PrintView,
    setup: &print::PageSetup,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> print::Document {
    let (sheet, chart) = sheet(p, st, view, setup.print_all_columns);
    print::layout(&PrintJob {
        project: p,
        setup,
        sheet: &sheet,
        chart,
        from,
        to,
        printed: chrono::Local::now().naive_local(),
        palette: Palette::default(),
    })
}

// ---------------------------------------------------------------------------------------------
// The system print service
// ---------------------------------------------------------------------------------------------

/// Runs a command, giving up after `limit` (a stuck print service must not hang PlanCraft).
fn run(cmd: &mut Command, limit: Duration) -> Result<String, String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return if status.success() { Ok(text) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) };
            }
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(30)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the print service did not answer".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Whether printing goes to a printer chosen in PlanCraft (CUPS) rather than a PDF viewer.
pub const HAS_PRINT_SERVICE: bool = cfg!(unix);

/// Printers known to the print service (the default first), found in the background.
pub fn list_printers() -> Receiver<Vec<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut printers = Vec::new();
        if HAS_PRINT_SERVICE {
            let default = run(Command::new("lpstat").arg("-d"), Duration::from_secs(5))
                .ok()
                .and_then(|s| s.rsplit(':').next().map(|n| n.trim().to_string()))
                .filter(|n| !n.is_empty() && !n.contains(' '));
            if let Ok(list) = run(Command::new("lpstat").arg("-e"), Duration::from_secs(5)) {
                printers = list.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
            }
            if let Some(d) = default
                && let Some(pos) = printers.iter().position(|p| *p == d)
            {
                let d = printers.remove(pos);
                printers.insert(0, d);
            }
        }
        let _ = tx.send(printers);
    });
    rx
}

/// Removes spooled PDFs older than a day (left behind by a crash or by the PDF viewer route).
fn clean_spool(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(86_400));
        if old && e.path().extension().is_some_and(|x| x == "pdf") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn spool_file(pdf: &[u8]) -> Result<PathBuf, String> {
    let dir = plancraft_core::paths::print_spool_dir().ok_or("no folder for print files (HOME is not set)")?;
    plancraft_core::io::create_private_dir(&dir).map_err(|e| e.to_string())?;
    clean_spool(&dir);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%3f");
    let path = dir.join(format!("print-{}-{stamp}.pdf", std::process::id()));
    plancraft_core::io::write_atomic(&path, pdf, plancraft_core::io::Privacy::OwnerOnly).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Sends a PDF to `printer` (or the default printer) in the background. The answer is a message
/// for the user: the print service's job id, or why it failed.
pub fn send(pdf: Vec<u8>, printer: Option<String>, copies: u8, title: String) -> Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = spool_file(&pdf).and_then(|path| {
            if HAS_PRINT_SERVICE {
                let mut cmd = Command::new("lp");
                cmd.arg("-n").arg(copies.clamp(1, 99).to_string()).arg("-t").arg(title.chars().take(100).collect::<String>());
                if let Some(p) = printer.filter(|p| !p.is_empty()) {
                    cmd.arg("-d").arg(p);
                }
                let r = run(cmd.arg(&path), Duration::from_secs(30)).map_err(|e| {
                    if e.contains("No such file") || e.contains("not found") {
                        "Printing needs the system print service (CUPS). Save as PDF and print it from a PDF viewer instead.".to_string()
                    } else {
                        format!("Could not print: {e}")
                    }
                });
                // lp has copied the file to the print queue (or failed).
                let _ = std::fs::remove_file(&path);
                r.map(|out| if out.is_empty() { "Sent to the printer.".to_string() } else { out })
            } else {
                // Windows: the default PDF viewer prints it (explorer's exit code means nothing).
                // The file is removed a day later.
                let _ = run(Command::new("explorer").arg(&path), Duration::from_secs(10));
                Ok("Opened in your PDF viewer: print it from there.".to_string())
            }
        });
        let _ = tx.send(result);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::grid::Cell;

    #[test]
    fn symbols_print_as_their_text() {
        let mode = Cell { text: "⟳".into(), tooltip: Some("Auto Scheduled".into()), ..Default::default() };
        assert_eq!(printable(&mode), "Auto Scheduled");
        assert_eq!(printable(&Cell::text("Design")), "Design");
        assert_eq!(printable(&Cell { text: "\u{1F916}".into(), ..Default::default() }), "");
    }

    #[test]
    fn the_printed_sheet_follows_the_view() {
        let mut p = plancraft_core::sample::demo_project(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        recalculate(&mut p);
        let mut st = UiState::default();
        let (sheet, chart) = sheet(&p, &st, PrintView::Gantt, true);
        assert!(chart.is_some());
        assert_eq!(sheet.rows.len(), p.tasks.len());
        assert_eq!(sheet.columns.first().map(|c| c.title.as_str()), Some("ID"));
        assert!(sheet.columns.iter().all(|c| c.title != "ℹ"), "no indicator column");
        // Filters apply.
        st.filter = crate::state::TaskFilter::Milestones;
        let (milestones, _) = super::sheet(&p, &st, PrintView::TaskTable, true);
        assert!(milestones.rows.len() < sheet.rows.len());
        // Groups print as group rows.
        st.filter = crate::state::TaskFilter::All;
        st.group = Some("duration".into());
        let (grouped, _) = super::sheet(&p, &st, PrintView::Gantt, true);
        assert!(grouped.rows.iter().any(|r| matches!(r.kind, RowKind::Group { .. })));
        let (res, chart) = super::sheet(&p, &st, PrintView::Resources, true);
        assert!(chart.is_none());
        assert_eq!(res.rows.len(), p.resources.len());
        // Without "all columns", only the columns left of the divider print.
        st.group = None;
        st.table_width = 200.0;
        let (narrow, _) = super::sheet(&p, &st, PrintView::Gantt, false);
        let (wide, _) = super::sheet(&p, &st, PrintView::Gantt, true);
        assert!(narrow.columns.len() < wide.columns.len() && narrow.columns.len() >= 2);
        // And the whole thing lays out and writes.
        let doc = document(&p, &st, PrintView::Gantt, &print::PageSetup::default(), None, None);
        assert!(!doc.pages.is_empty());
        assert!(print::pdf::write(&doc).starts_with(b"%PDF"));
    }
}

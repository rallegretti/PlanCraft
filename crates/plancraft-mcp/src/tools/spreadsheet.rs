//! Spreadsheet tools: import a task list from CSV or Excel, export one as CSV.

use std::path::PathBuf;

use plancraft_core::tabular::{self, DateOrder, Field, Grid, ImportOptions};
use serde_json::{Value, json};

use super::*;
use crate::args::*;
use crate::server::doc_at_mut;

pub fn tools() -> Vec<Tool> {
    let fields: Vec<&str> = Field::ALL.iter().map(|f| f.key()).collect();
    vec![
        Tool::new(
            "import_spreadsheet",
            "Import a task list from Excel or CSV",
            "Import a task list from a spreadsheet (.csv, .tsv, .xlsx, .xlsm or .xlsb; save .xls and .ods files as .xlsx or CSV first) as a new unsaved document. Each row with a task name becomes a task. Columns are matched to task fields by their headings (Name, Duration, Start, Finish, Predecessors, Resource Names, % Complete, Outline Level or WBS, Milestone, Notes, ID); pass 'columns' to correct the guesses. Call it first with preview: true to see the headings, the guessed fields and the first rows without importing anything. Predecessors name tasks by the ID column (or, without one, by row number among the imported tasks), like '3' or '3FS+2d'. A start date later than the schedule gives is kept as a Start No Earlier Than constraint. 'import_notes' lists what could not be read. Save it with save_project and a .plancraft 'path' to keep it.",
            schema(
                json!({
                    "path": string("Path of the spreadsheet file (.csv, .tsv, .xlsx, .xlsm or .xlsb)."),
                    "sheet": string("Worksheet to import, by name or number (1 = the first; default 1)."),
                    "preview": boolean("Only report the worksheets, headings, guessed fields and first rows; import nothing (default false)."),
                    "columns": {
                        "type": "object",
                        "description": format!("What columns hold, overriding the guesses: keys are column headings (or column numbers, 1 = the first), values are one of {} or null to leave the column out.", fields.join(", ")),
                        "additionalProperties": { "type": ["string", "null"] }
                    },
                    "date_order": { "type": "string", "enum": ["month_first", "day_first"], "description": "How dates like 03/04/2026 read: month_first (March 4) or day_first (3 April). Default: guessed from the dates." },
                    "name": string("Name of the new plan (default: the file name)."),
                }),
                &["path"],
            ),
            import_spreadsheet,
        ),
        Tool::new(
            "export_csv",
            "Export the task list as CSV",
            "Save the document's task list as a CSV file for spreadsheets: ID, WBS, outline level, name, milestone, duration, start, finish, % complete, work, cost, predecessors, resources, critical and custom fields. import_spreadsheet reads the file back. The document itself is not changed. A relative path is relative to the PlanCraft process's working folder.",
            schema(
                json!({
                    "path": string("File path ending in .csv (added when missing)."),
                    "overwrite": boolean("Replace an existing file at 'path' (default false). Check with the user first."),
                }),
                &["path"],
            ),
            export_csv,
        ),
    ]
}

/// The most characters of a cell or heading a reply shows.
const SHOWN_CHARS: usize = 200;
/// The most names an error lists.
const SHOWN_NAMES: usize = 20;

/// A cell or heading as a reply shows it: at most `SHOWN_CHARS` characters.
fn shown(text: &str) -> String {
    match text.char_indices().nth(SHOWN_CHARS) {
        Some((end, _)) => format!("{}…", text.get(..end).unwrap_or("")),
        None => text.to_string(),
    }
}

/// Names for an error message: the first `SHOWN_NAMES`, shortened.
fn name_list<'a>(names: impl ExactSizeIterator<Item = &'a str>) -> String {
    let total = names.len();
    let mut list: Vec<String> = names.take(SHOWN_NAMES).map(shown).collect();
    if total > SHOWN_NAMES {
        list.push(format!("and {} more", total.saturating_sub(SHOWN_NAMES)));
    }
    list.join(", ")
}

/// An absolute path (relative ones are relative to the working folder).
fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() { path } else { std::env::current_dir().map(|c| c.join(&path)).unwrap_or(path) }
}

/// The worksheet picked by the `sheet` argument: a name, or a number from 1.
fn pick_sheet(sheets: &tabular::Sheets, a: &Args) -> ToolResult<usize> {
    let Some(v) = a.get("sheet").filter(|v| !v.is_null()) else { return Ok(0) };
    let text = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return err("'sheet' must be a worksheet name or number"),
    };
    if let Some(i) = sheets.sheets.iter().position(|(name, _)| name.eq_ignore_ascii_case(&text)) {
        return Ok(i);
    }
    match text.parse::<usize>() {
        Ok(n) if (1..=sheets.sheets.len()).contains(&n) => Ok(n.saturating_sub(1)),
        _ => err(format!(
            "there is no worksheet '{}'; the file has: {}",
            shown(&text),
            name_list(sheets.sheets.iter().map(|(n, _)| n.as_str()))
        )),
    }
}

/// The column a `columns` key names: a heading (ignoring case) or a number from 1.
fn column_of(grid: &Grid, key: &str) -> ToolResult<usize> {
    if let Some(i) = grid.headers.iter().position(|h| h.trim().eq_ignore_ascii_case(key.trim())) {
        return Ok(i);
    }
    match key.trim().parse::<usize>() {
        Ok(n) if (1..=tabular::MAX_COLUMNS).contains(&n) => Ok(n.saturating_sub(1)),
        _ => err(format!("there is no column '{}'; the headings are: {}", shown(key), name_list(grid.headers.iter().map(String::as_str)))),
    }
}

/// Columns and their fields, as `{heading: field}` in column order.
fn columns_json(grid: &Grid, columns: &[Option<Field>]) -> Value {
    let count = grid.headers.len().max(columns.len());
    Value::Array(
        (0..count)
            .map(|c| {
                json!({
                    "column": c.saturating_add(1),
                    "heading": shown(grid.headers.get(c).map_or("", String::as_str)),
                    "field": columns.get(c).copied().flatten().map(Field::key),
                })
            })
            .collect(),
    )
}

fn import_spreadsheet(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.require_file_access("Importing files")?;
    let raw = a.req_str("path")?.trim();
    if raw.is_empty() {
        return err("'path' is empty");
    }
    let path = absolute(PathBuf::from(raw));
    let sheets = tabular::read_path(&path)?;
    let sheet = pick_sheet(&sheets, a)?;
    let (_, grid) = sheets.sheets.get(sheet).ok_or_else(|| missing("worksheet"))?;
    let name = match a.text("name")? {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => path.file_stem().map_or_else(|| "Imported plan".to_string(), |s| s.to_string_lossy().into_owned()),
    };
    let mut options = ImportOptions::guess(grid, name);
    if let Some(columns) = a.object("columns")? {
        for (key, value) in columns.map() {
            let c = column_of(grid, key)?;
            let field = match value {
                Value::Null => None,
                Value::String(s) if s.trim().is_empty() => None,
                Value::String(s) => Some(Field::from_key(s).ok_or_else(|| {
                    let keys: Vec<&str> = Field::ALL.iter().map(|f| f.key()).collect();
                    ToolError::from(format!("'{}' is not a field; use one of {}", shown(s), keys.join(", ")))
                })?),
                _ => return err(format!("columns.{key} must be a field name or null")),
            };
            if let Some(f) = field {
                for other in options.columns.iter_mut().filter(|o| **o == Some(f)) {
                    *other = None;
                }
            }
            if options.columns.len() <= c {
                options.columns.resize(c.saturating_add(1), None);
            }
            if let Some(slot) = options.columns.get_mut(c) {
                *slot = field;
            }
        }
    }
    match a.str("date_order")? {
        Some("month_first") => options.date_order = DateOrder::MonthFirst,
        Some("day_first") => options.date_order = DateOrder::DayFirst,
        Some(other) => return err(format!("'{}' is not a date order; use month_first or day_first", shown(other))),
        // Columns newly read as Start or Finish may tell the date order.
        None => options.date_order = tabular::guess_date_order(grid, &options.columns),
    }

    if a.bool("preview")?.unwrap_or(false) {
        let rows: Vec<Vec<String>> = grid.rows.iter().take(5).map(|r| r.iter().map(|c| shown(c)).collect()).collect();
        return Ok(json!({
            "worksheets": sheets.sheets.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            "worksheet": sheets.sheets.get(sheet).map(|(n, _)| n.as_str()),
            "rows": grid.rows.len(),
            "columns": columns_json(grid, &options.columns),
            "date_order": if options.date_order == DateOrder::DayFirst { "day_first" } else { "month_first" },
            "first_rows": rows,
            "notes": sheets.notes,
        }));
    }

    let (project, mut notes) = tabular::build(grid, &options).map_err(ToolError::from)?;
    let mut all = sheets.notes.clone();
    all.append(&mut notes);
    let columns = columns_json(grid, &options.columns);
    let out = ctx.with_workspace(|ws| {
        let from = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let handle = ws.add_imported(project, all, from);
        let i = ws.index_of(handle).ok_or_else(|| missing("imported"))?;
        let doc = doc_at_mut(ws, i)?;
        // An agent's import is the agent's unsaved work (it may save or close it).
        doc.user_changes = false;
        let mut out = super::documents::project_overview(doc);
        set(&mut out, "import_notes", json!(doc.load_notes));
        set(&mut out, "columns", columns);
        Ok(out)
    })?;
    ctx.mark_changed();
    Ok(out)
}

fn export_csv(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.require_file_access("Exporting files")?;
    let overwrite = a.bool("overwrite")?.unwrap_or(false);
    let raw = a.req_str("path")?.trim();
    if raw.is_empty() {
        return err("'path' is empty");
    }
    let path = PathBuf::from(raw);
    let path = match path.extension() {
        None => path.with_extension("csv"),
        Some(e) if e.eq_ignore_ascii_case("csv") => path,
        Some(_) => return err("'path' must end in .csv"),
    };
    let path = absolute(path);
    if path.exists() && !overwrite {
        return err(format!("{} already exists; pass overwrite: true to replace it (check with the user first)", path.display()));
    }
    // Make the text, then write without holding the workspace.
    let (csv, tasks) = ctx.read(a, |doc| Ok((tabular::to_csv(&doc.project), doc.project.tasks.len())))?;
    plancraft_core::io::write_atomic(&path, csv.as_bytes(), plancraft_core::io::Privacy::KeepExisting)
        .map_err(|e| ToolError::from(format!("could not write {}: {e}", path.display())))?;
    Ok(json!({ "path": path.display().to_string(), "tasks": tasks }))
}

//! Task lists from spreadsheets: importing CSV, tab-separated text and Excel workbooks (`.xlsx`,
//! `.xlsm`, `.xlsb`), and exporting a plan's tables (tasks, resources, assignments, logged time) as
//! CSV.
//!
//! Importing takes three steps, so a person (or an agent) can check the middle one:
//! 1. [`read_path`] / [`read_bytes`] read the file into worksheets of text cells.
//! 2. [`guess_columns`] matches each column to a task field by its heading; the user can change
//!    any of them ([`ImportOptions::columns`]).
//! 3. [`build`] makes the plan: tasks, outline, durations, links, resources, progress, notes,
//!    and the file's dates as Start No Earlier Than constraints where the schedule alone would
//!    put a task earlier.
//!
//! Files are untrusted: every count, cell and value is bounded (`limits`), and nothing here can
//! panic the app (`guard`).

mod build;
mod csv;
mod export;
mod workbook;

use std::io::Read as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::io::{FileError, FileResult};
use crate::limits::{MAX_FILE_BYTES, MAX_NOTES_CHARS, MAX_TASKS};

pub use build::build;
pub use export::{CsvTable, table_columns, table_to_csv, table_to_csv_columns, to_csv};

/// File extensions read as CSV text.
pub const TEXT_EXTENSIONS: [&str; 3] = ["csv", "tsv", "txt"];
/// File extensions read as workbooks.
pub const WORKBOOK_EXTENSIONS: [&str; 3] = ["xlsx", "xlsm", "xlsb"];

/// At most this many cells (including empty ones between them) are kept from a file.
pub const MAX_CELLS: usize = 5_000_000;
/// At most this much text is kept from a file, all cells together.
pub const MAX_TEXT_BYTES: usize = 128 * 1024 * 1024;

/// At most this many columns are read; more are dropped (with a note).
pub const MAX_COLUMNS: usize = 1_024;

/// Whether a path names a file this module imports (by its extension).
pub fn is_table_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        let e = e.to_ascii_lowercase();
        TEXT_EXTENSIONS.contains(&e.as_str()) || WORKBOOK_EXTENSIONS.contains(&e.as_str())
    })
}

/// A worksheet as text: the column headings (the first row with any text) and the rows below.
/// Rows can be shorter than the headings (missing cells are empty).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grid {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Grid {
    fn from_rows(mut rows: Vec<Vec<String>>) -> Grid {
        if rows.is_empty() {
            return Grid::default();
        }
        let headers = rows.remove(0).into_iter().map(|h| h.trim().to_string()).collect();
        Grid { headers, rows }
    }

    /// The cell at a row and column (empty when the row is short).
    pub fn cell(&self, row: usize, column: usize) -> &str {
        self.rows.get(row).and_then(|r| r.get(column)).map_or("", |c| c.as_str())
    }
}

/// A file's worksheets (one for CSV) and notes on what reading it left out.
#[derive(Clone, Debug, Default)]
pub struct Sheets {
    pub sheets: Vec<(String, Grid)>,
    pub notes: Vec<String>,
}

/// The bounds applied while reading, and the notes on what they cut.
#[derive(Default)]
pub(crate) struct Limits {
    notes: Vec<String>,
    rows_cut: bool,
    columns_cut: bool,
    cells_cut: bool,
    /// Cells and bytes of text kept so far, against `MAX_CELLS` and `MAX_TEXT_BYTES`.
    cells: usize,
    text: usize,
    full: bool,
}

impl Limits {
    /// The most bytes a cell keeps (about the longest notes a task can have).
    pub(crate) const MAX_CELL_BYTES: usize = MAX_NOTES_CHARS;

    /// Counts `cells` more cells holding `bytes` of text against the file's budget; false (and
    /// nothing more is read) once it is spent.
    fn spend(&mut self, cells: usize, bytes: usize) -> bool {
        self.cells = self.cells.saturating_add(cells.max(1));
        self.text = self.text.saturating_add(bytes);
        if self.cells > MAX_CELLS || self.text > MAX_TEXT_BYTES {
            self.full = true;
        }
        !self.full
    }

    fn max_rows(&self) -> usize {
        MAX_TASKS
    }

    fn max_columns(&self) -> usize {
        MAX_COLUMNS
    }

    fn rows_cut(&mut self) {
        self.rows_cut = true;
    }

    fn columns_cut(&mut self) {
        self.columns_cut = true;
    }

    fn note(&mut self, text: &str) {
        if !self.notes.iter().any(|n| n == text) {
            self.notes.push(text.to_string());
        }
    }

    /// Adds a character to a cell being read, unless the cell is full.
    fn push_char(&mut self, cell: &mut String, c: char) {
        if cell.len().saturating_add(c.len_utf8()) <= Self::MAX_CELL_BYTES {
            cell.push(c);
        } else {
            self.cells_cut = true;
        }
    }

    /// A finished cell, shortened (at a character boundary) when it is too long.
    fn cell(&mut self, mut text: String) -> String {
        if text.len() > Self::MAX_CELL_BYTES {
            let mut end = Self::MAX_CELL_BYTES;
            while !text.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            text.truncate(end);
            self.cells_cut = true;
        }
        text
    }

    fn into_notes(mut self) -> Vec<String> {
        if self.rows_cut {
            self.notes.push(format!("only the first {MAX_TASKS} rows were read"));
        }
        if self.columns_cut {
            self.notes.push(format!("only the first {MAX_COLUMNS} columns were read"));
        }
        if self.cells_cut {
            self.notes.push("very long cells were shortened".to_string());
        }
        if self.full {
            self.notes.push("the file holds more than PlanCraft reads; only its first part was read".to_string());
        }
        self.notes
    }
}

/// Reads a file's bytes; `extension` (without the dot) says whether it is text or a workbook.
pub fn read_bytes(bytes: &[u8], extension: &str) -> Result<Sheets, String> {
    let mut limits = Limits::default();
    let workbook = WORKBOOK_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str());
    let sheets = crate::guard::catch("reading a spreadsheet", || {
        if workbook {
            workbook::read(bytes, &extension.to_ascii_lowercase(), &mut limits)
        } else {
            Ok(vec![("Sheet 1".to_string(), csv::read(&csv::decode(bytes), &mut limits))])
        }
    })
    .map_err(|p| format!("PlanCraft hit an internal error reading the file ({})", p.message))??;
    if sheets.iter().all(|(_, g)| g.headers.is_empty()) {
        return Err("the file has no rows to import".to_string());
    }
    Ok(Sheets { sheets, notes: limits.into_notes() })
}

/// Reads a spreadsheet file (refusing ones over 64 MiB and anything but a regular file).
pub fn read_path(path: impl AsRef<Path>) -> FileResult<Sheets> {
    let path = path.as_ref();
    let io_err = |source| FileError::Io { path: path.into(), source };
    if !std::fs::metadata(path).map_err(io_err)?.is_file() {
        return Err(io_err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file")));
    }
    let file = std::fs::File::open(path).map_err(io_err)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(io_err)?;
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > MAX_FILE_BYTES {
        return Err(FileError::TooLarge { path: path.into(), size });
    }
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("csv");
    read_bytes(&bytes, extension).map_err(|message| FileError::Import { path: path.into(), message })
}

/// What a column holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// The task's number, which the Predecessors column refers to.
    Id,
    Name,
    /// 1 for top-level tasks, 2 for their subtasks, and so on.
    OutlineLevel,
    /// An outline number such as `1.2.3` (its depth gives the outline level).
    Wbs,
    Duration,
    Start,
    Finish,
    Predecessors,
    Resources,
    PercentComplete,
    Milestone,
    Notes,
}

impl Field {
    pub const ALL: [Field; 12] = [
        Field::Id,
        Field::Name,
        Field::OutlineLevel,
        Field::Wbs,
        Field::Duration,
        Field::Start,
        Field::Finish,
        Field::Predecessors,
        Field::Resources,
        Field::PercentComplete,
        Field::Milestone,
        Field::Notes,
    ];

    /// The name shown to people.
    pub fn label(self) -> &'static str {
        match self {
            Field::Id => "ID",
            Field::Name => "Name",
            Field::OutlineLevel => "Outline Level",
            Field::Wbs => "WBS",
            Field::Duration => "Duration",
            Field::Start => "Start",
            Field::Finish => "Finish",
            Field::Predecessors => "Predecessors",
            Field::Resources => "Resource Names",
            Field::PercentComplete => "% Complete",
            Field::Milestone => "Milestone",
            Field::Notes => "Notes",
        }
    }

    /// The name agents use (`"percent_complete"`).
    pub fn key(self) -> &'static str {
        match self {
            Field::Id => "id",
            Field::Name => "name",
            Field::OutlineLevel => "outline_level",
            Field::Wbs => "wbs",
            Field::Duration => "duration",
            Field::Start => "start",
            Field::Finish => "finish",
            Field::Predecessors => "predecessors",
            Field::Resources => "resources",
            Field::PercentComplete => "percent_complete",
            Field::Milestone => "milestone",
            Field::Notes => "notes",
        }
    }

    pub fn from_key(key: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.key().eq_ignore_ascii_case(key.trim()))
    }

    /// Headings this field is recognised by, lower-case with spaces and punctuation removed.
    fn headings(self) -> &'static [&'static str] {
        match self {
            Field::Id => &["id", "taskid", "taskno", "no", "number", "taskno", "tasknumber"],
            Field::Name => &["name", "taskname", "task", "title", "activity", "activityname", "item", "summary"],
            Field::OutlineLevel => &["outlinelevel", "level", "indent", "indentlevel"],
            Field::Wbs => &["wbs", "wbscode", "outlinenumber"],
            Field::Duration => &["duration", "dur", "length", "days", "durationdays"],
            Field::Start => &["start", "startdate", "begin", "begins", "plannedstart", "from", "startson"],
            Field::Finish => &["finish", "finishdate", "end", "enddate", "due", "duedate", "plannedfinish", "deadline", "ends"],
            Field::Predecessors => &["predecessors", "predecessor", "dependson", "dependencies", "dependency", "depends", "after"],
            Field::Resources => &["resourcenames", "resources", "resource", "assignedto", "assignee", "assignees", "owner", "who"],
            Field::PercentComplete => &["complete", "percentcomplete", "progress", "done", "percentdone", "pctcomplete"],
            Field::Milestone => &["milestone", "ismilestone"],
            Field::Notes => &["notes", "note", "comments", "comment", "description", "details", "remarks"],
        }
    }
}

/// A heading reduced for matching: lower-case letters and digits only (`"% Complete"` →
/// `"complete"`, `"Work (h)"` → `"workh"`).
fn heading_key(heading: &str) -> String {
    heading.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Matches columns to fields by their headings; each field goes to its first matching column.
/// `#` alone is read as the ID column.
pub fn guess_columns(headers: &[String]) -> Vec<Option<Field>> {
    let mut used = Vec::new();
    headers
        .iter()
        .map(|h| {
            let key = if h.trim() == "#" { "id".to_string() } else { heading_key(h) };
            let field = Field::ALL.into_iter().find(|f| !used.contains(f) && f.headings().contains(&key.as_str()));
            if let Some(f) = field {
                used.push(f);
            }
            field
        })
        .collect()
}

/// Which part of a date like `03/04/2026` is the month.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateOrder {
    /// `MM/DD/YYYY`, as in the United States.
    #[default]
    MonthFirst,
    /// `DD/MM/YYYY` (or `DD.MM.YYYY`), as in most other places.
    DayFirst,
}

/// Guesses the date order from the dates in the Start and Finish columns: a first number above
/// 12 can only be a day, a second one only a month. Month first when nothing tells.
pub fn guess_date_order(grid: &Grid, columns: &[Option<Field>]) -> DateOrder {
    let mut day_first = 0usize;
    let mut month_first = 0usize;
    for (c, field) in columns.iter().enumerate() {
        if !matches!(field, Some(Field::Start | Field::Finish)) {
            continue;
        }
        for r in 0..grid.rows.len() {
            if let Some((a, b, _)) = build::numeric_date_parts(grid.cell(r, c)) {
                if a > 12 && b <= 12 {
                    day_first = day_first.saturating_add(1);
                } else if b > 12 && a <= 12 {
                    month_first = month_first.saturating_add(1);
                }
            }
        }
    }
    if day_first > month_first { DateOrder::DayFirst } else { DateOrder::MonthFirst }
}

/// How to turn a worksheet into a plan.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// The field each column holds (`None`: not imported), in column order.
    pub columns: Vec<Option<Field>>,
    pub date_order: DateOrder,
    /// The plan's name.
    pub name: String,
}

impl ImportOptions {
    /// The guesses for a worksheet: columns by heading, the date order from the dates.
    pub fn guess(grid: &Grid, name: impl Into<String>) -> ImportOptions {
        let columns = guess_columns(&grid.headers);
        let date_order = guess_date_order(grid, &columns);
        ImportOptions { columns, date_order, name: name.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_columns_from_common_headings() {
        let headers: Vec<String> =
            ["#", "Task Name", "Duration", "Start Date", "Due", "Depends on", "Assigned To", "% Complete", "Comments", "Work (h)", "Name"]
                .map(String::from)
                .to_vec();
        let guess = guess_columns(&headers);
        assert_eq!(
            guess,
            [
                Some(Field::Id),
                Some(Field::Name),
                Some(Field::Duration),
                Some(Field::Start),
                Some(Field::Finish),
                Some(Field::Predecessors),
                Some(Field::Resources),
                Some(Field::PercentComplete),
                Some(Field::Notes),
                None,
                None, // Name was taken by the first matching column.
            ]
        );
    }

    #[test]
    fn plancrafts_own_export_headings_are_recognised() {
        let headers: Vec<String> =
            "ID,Unique ID,WBS,Outline Level,Name,Milestone,Duration,Start,Finish,% Complete,Work (h),Cost,Predecessors,Resources,Critical"
                .split(',')
                .map(String::from)
                .collect();
        let guess = guess_columns(&headers);
        let found: Vec<Field> = guess.iter().flatten().copied().collect();
        for f in [
            Field::Id,
            Field::Wbs,
            Field::OutlineLevel,
            Field::Name,
            Field::Milestone,
            Field::Duration,
            Field::Start,
            Field::Finish,
            Field::PercentComplete,
            Field::Predecessors,
            Field::Resources,
        ] {
            assert!(found.contains(&f), "{f:?} not recognised");
        }
        assert_eq!(guess.get(1), Some(&None), "Unique ID is not the ID the predecessors refer to");
    }

    #[test]
    fn date_order_comes_from_unambiguous_dates() {
        let grid = Grid {
            headers: vec!["Name".into(), "Start".into()],
            rows: vec![vec!["A".into(), "03/04/2026".into()], vec!["B".into(), "25/04/2026".into()]],
        };
        assert_eq!(ImportOptions::guess(&grid, "x").date_order, DateOrder::DayFirst);
        let grid = Grid { headers: vec!["Name".into(), "Start".into()], rows: vec![vec!["A".into(), "04/25/2026".into()]] };
        assert_eq!(ImportOptions::guess(&grid, "x").date_order, DateOrder::MonthFirst);
    }

    #[test]
    fn field_keys_round_trip() {
        for f in Field::ALL {
            assert_eq!(Field::from_key(f.key()), Some(f));
        }
        assert_eq!(Field::from_key("nonsense"), None);
    }
}

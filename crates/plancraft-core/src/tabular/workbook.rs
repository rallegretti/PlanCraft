//! Reading Excel workbooks (`.xlsx`, `.xlsm`, `.xlsb`) with `calamine` (pure Rust).
//!
//! Cells become text the rest of the import reads like CSV: dates as `YYYY-MM-DD` (with
//! `HH:MM` when the cell has a time of day), time spans (`[h]:mm` cells) as hours, numbers in
//! their shortest form.
//!
//! Workbooks are untrusted, so nothing is read in one piece:
//! - The archive's declared sizes must stay within `MAX_UNPACKED`, and the zip reader refuses to
//!   unpack an entry past its declared size, so a small file can't unpack into gigabytes.
//! - Cells are streamed one at a time (calamine's whole-sheet `Range` would allocate a grid
//!   spanning the outermost cells, 550 GB for a sheet with cells in A1 and XFD1048576), and each
//!   is shortened before it is copied, against the file's budget of cells and text (`Limits`).
//! - Legacy `.xls` and OpenDocument `.ods` are not read: calamine reads those only in one piece.

use std::collections::BTreeMap;
use std::io::Cursor;

use calamine::{DataRef, Reader, Xlsb, Xlsx};

use super::{Grid, Limits, MAX_COLUMNS};

/// How much a zipped workbook may unpack to, all entries together.
pub const MAX_UNPACKED: u64 = 256 * 1024 * 1024;

/// Checks a workbook's declared sizes.
fn check_unpacked_size(bytes: &[u8]) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("the file is not an Excel workbook ({e})"))?;
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(|e| format!("the workbook is damaged ({e})"))?;
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED {
            return Err(format!(
                "the workbook unpacks to more than {} MB, which is more than PlanCraft reads",
                MAX_UNPACKED / (1024 * 1024)
            ));
        }
    }
    Ok(())
}

/// Reads every worksheet of a workbook; `extension` (lower case) picks the format.
pub fn read(bytes: &[u8], extension: &str, limits: &mut Limits) -> Result<Vec<(String, Grid)>, String> {
    check_unpacked_size(bytes)?;
    if extension == "xlsb" {
        let mut book: Xlsb<_> =
            calamine::open_workbook_from_rs(Cursor::new(bytes)).map_err(|e| format!("the workbook could not be read ({e})"))?;
        let mut sheets = Vec::new();
        for name in book.sheet_names() {
            let grid = match book.worksheet_cells_reader(&name) {
                Ok(mut cells) => stream(|| cells.next_cell().map_err(|e| e.to_string()), limits),
                Err(_) => {
                    limits.note("a worksheet that could not be read was skipped");
                    continue;
                }
            };
            sheets.push((limits.cell(name), grid));
        }
        Ok(sheets)
    } else {
        let mut book: Xlsx<_> =
            calamine::open_workbook_from_rs(Cursor::new(bytes)).map_err(|e| format!("the workbook could not be read ({e})"))?;
        let mut sheets = Vec::new();
        for name in book.sheet_names() {
            let grid = match book.worksheet_cells_reader(&name) {
                Ok(mut cells) => stream(|| cells.next_cell().map_err(|e| e.to_string()), limits),
                Err(_) => {
                    limits.note("a worksheet that could not be read was skipped");
                    continue;
                }
            };
            sheets.push((limits.cell(name), grid));
        }
        Ok(sheets)
    }
}

/// Reads a worksheet's cells as they come, keeping what the limits allow.
fn stream<'a>(mut next: impl FnMut() -> Result<Option<calamine::Cell<DataRef<'a>>>, String>, limits: &mut Limits) -> Grid {
    let mut rows: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    loop {
        let cell = match next() {
            Ok(Some(cell)) => cell,
            Ok(None) => break,
            Err(_) => {
                limits.note("part of a worksheet could not be read");
                break;
            }
        };
        let (r, c) = cell.get_position();
        let Some(c) = usize::try_from(c).ok().filter(|c| *c < MAX_COLUMNS) else {
            limits.columns_cut();
            continue;
        };
        let text = text(cell.get_value());
        if text.trim().is_empty() {
            continue;
        }
        if !rows.contains_key(&r) && rows.len() > limits.max_rows() {
            limits.rows_cut();
            // Rows usually come in order: nothing after this one can be kept.
            if rows.keys().next_back().is_some_and(|last| r > *last) {
                break;
            }
            continue;
        }
        let row = rows.entry(r).or_default();
        let padding = c.saturating_add(1).saturating_sub(row.len());
        if !limits.spend(padding, text.len()) {
            break;
        }
        if row.len() <= c {
            row.resize(c.saturating_add(1), String::new());
        }
        if let Some(slot) = row.get_mut(c) {
            *slot = text;
        }
    }
    Grid::from_rows(rows.into_values().collect())
}

/// The start of a text no longer than a cell may be, cut at a character boundary.
fn clip(text: &str) -> &str {
    let mut end = text.len().min(Limits::MAX_CELL_BYTES);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or("")
}

/// A cell as text (shortened to the longest a cell may be).
fn text(cell: &DataRef<'_>) -> String {
    match cell {
        DataRef::Empty | DataRef::Error(_) => String::new(),
        DataRef::SharedString(s) => clip(s.trim()).to_string(),
        DataRef::String(s) | DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => clip(s.trim()).to_string(),
        DataRef::Bool(b) => if *b { "Yes" } else { "No" }.to_string(),
        DataRef::Int(i) => i.to_string(),
        DataRef::Float(f) => number(*f),
        DataRef::DateTime(dt) if dt.is_duration() => {
            // A time span: Excel stores it in days.
            let hours = dt.as_f64() * 24.0;
            if hours.is_finite() { format!("{}h", number((hours * 100.0).round() / 100.0)) } else { String::new() }
        }
        DataRef::DateTime(dt) => {
            // Only dates PlanCraft can hold (1900–2300 is about serials 0–146,000).
            let serial = dt.as_f64();
            if !serial.is_finite() || !(-1.0..=200_000.0).contains(&serial) {
                return String::new();
            }
            // Calamine's own conversion follows Excel's calendar (including its 1900 leap-year
            // quirk); a day that doesn't exist (Excel's 29 February 1900) reads as no date.
            let (y, mo, d, h, mi, s, ms) = dt.to_ymd_hms_milli();
            match crate::dates::ymd_hm(i32::from(y), u32::from(mo), u32::from(d), u32::from(h), u32::from(mi)) {
                Some(t) if h == 0 && mi == 0 && s == 0 && ms == 0 => crate::dates::format("%Y-%m-%d", t),
                Some(t) => crate::dates::format("%Y-%m-%d %H:%M", t),
                None => String::new(),
            }
        }
    }
}

/// A number in its shortest form (`3`, `2.5`), never in exponent notation for ordinary sizes.
fn number(f: f64) -> String {
    if !f.is_finite() {
        return String::new();
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    let s = format!("{f:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{ExcelDateTime, ExcelDateTimeType};

    #[test]
    fn cells_become_the_text_the_import_reads() {
        assert_eq!(text(&DataRef::Float(3.0)), "3");
        assert_eq!(text(&DataRef::Float(2.5)), "2.5");
        assert_eq!(text(&DataRef::Float(f64::NAN)), "");
        assert_eq!(text(&DataRef::Bool(true)), "Yes");
        // 2026-10-19 is serial 46314; with noon, 46314.5.
        assert_eq!(text(&DataRef::DateTime(ExcelDateTime::new(46314.0, ExcelDateTimeType::DateTime, false))), "2026-10-19");
        assert_eq!(text(&DataRef::DateTime(ExcelDateTime::new(46314.5, ExcelDateTimeType::DateTime, false))), "2026-10-19 12:00");
        assert_eq!(text(&DataRef::DateTime(ExcelDateTime::new(1.5, ExcelDateTimeType::TimeDelta, false))), "36h");
        assert_eq!(text(&DataRef::DateTime(ExcelDateTime::new(1e300, ExcelDateTimeType::DateTime, false))), "");
        let long = "é".repeat(Limits::MAX_CELL_BYTES);
        assert!(text(&DataRef::SharedString(&long)).len() <= Limits::MAX_CELL_BYTES);
    }

    #[test]
    fn rejects_files_that_are_not_workbooks() {
        assert!(read(b"not a workbook", "xlsx", &mut Limits::default()).is_err());
        assert!(read(b"PK\x03\x04garbage", "xlsx", &mut Limits::default()).is_err());
        assert!(read(b"PK\x03\x04garbage", "xlsb", &mut Limits::default()).is_err());
    }
}

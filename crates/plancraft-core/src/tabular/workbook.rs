//! Reading spreadsheet workbooks: Excel (`.xlsx`, `.xlsm`, `.xlsb`, `.xls`) and OpenDocument
//! (`.ods`), with `calamine` (pure Rust).
//!
//! Cells become text the rest of the import reads like CSV: dates as `YYYY-MM-DD` (with
//! `HH:MM` when the cell has a time of day), time spans (`[h]:mm` cells) as hours, numbers in
//! their shortest form.
//!
//! The zipped formats (everything but `.xls`) are checked before they are unpacked: the sizes
//! the archive declares must stay within `MAX_UNPACKED`, and the zip reader refuses to unpack
//! an entry past its declared size, so a small file can't unpack into gigabytes in memory.

use std::io::Cursor;

use calamine::{Data, Reader};

use super::{Grid, Limits};

/// How much a zipped workbook may unpack to, all entries together.
pub const MAX_UNPACKED: u64 = 256 * 1024 * 1024;

/// Checks a zipped workbook's declared sizes. Not a zip at all (`.xls`) is fine: calamine reads
/// it as is, and it is no bigger than the file.
fn check_unpacked_size(bytes: &[u8]) -> Result<(), String> {
    if !bytes.starts_with(b"PK") {
        return Ok(());
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("the workbook is damaged ({e})"))?;
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

/// Reads every worksheet of a workbook.
pub fn read(bytes: &[u8], limits: &mut Limits) -> Result<Vec<(String, Grid)>, String> {
    check_unpacked_size(bytes)?;
    let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|e| format!("the workbook could not be read ({e})"))?;
    let mut sheets = Vec::new();
    for name in book.sheet_names() {
        let Ok(range) = book.worksheet_range(&name) else {
            limits.note("a worksheet that could not be read was skipped");
            continue;
        };
        let mut rows: Vec<Vec<String>> = Vec::new();
        for row in range.rows() {
            let mut cells: Vec<String> = Vec::new();
            for cell in row.iter().take(limits.max_columns()) {
                cells.push(limits.cell(text(cell)));
            }
            if row.len() > limits.max_columns() {
                limits.columns_cut();
            }
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            if rows.len() > limits.max_rows() {
                limits.rows_cut();
                break;
            }
            rows.push(cells);
        }
        sheets.push((limits.cell(name), Grid::from_rows(rows)));
    }
    Ok(sheets)
}

/// A cell as text.
fn text(cell: &Data) -> String {
    match cell {
        Data::Empty | Data::Error(_) => String::new(),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => s.trim().to_string(),
        Data::Bool(b) => if *b { "Yes" } else { "No" }.to_string(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => number(*f),
        Data::DateTime(dt) if dt.is_duration() => {
            // A time span: Excel stores it in days.
            let hours = dt.as_f64() * 24.0;
            if hours.is_finite() { format!("{}h", number((hours * 100.0).round() / 100.0)) } else { String::new() }
        }
        Data::DateTime(dt) => {
            // Only dates PlanCraft can hold (1900–2300 is about serials 0–146,000).
            let serial = dt.as_f64();
            if !serial.is_finite() || !(-1.0..=200_000.0).contains(&serial) {
                return String::new();
            }
            match dt.as_datetime() {
                Some(t) if t.time() == chrono::NaiveTime::MIN => t.format("%Y-%m-%d").to_string(),
                Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
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
        assert_eq!(text(&Data::Float(3.0)), "3");
        assert_eq!(text(&Data::Float(2.5)), "2.5");
        assert_eq!(text(&Data::Float(f64::NAN)), "");
        assert_eq!(text(&Data::Bool(true)), "Yes");
        // 2026-10-19 is serial 46314; with noon, 46314.5.
        assert_eq!(text(&Data::DateTime(ExcelDateTime::new(46314.0, ExcelDateTimeType::DateTime, false))), "2026-10-19");
        assert_eq!(text(&Data::DateTime(ExcelDateTime::new(46314.5, ExcelDateTimeType::DateTime, false))), "2026-10-19 12:00");
        assert_eq!(text(&Data::DateTime(ExcelDateTime::new(1.5, ExcelDateTimeType::TimeDelta, false))), "36h");
        assert_eq!(text(&Data::DateTime(ExcelDateTime::new(1e300, ExcelDateTimeType::DateTime, false))), "");
    }

    #[test]
    fn rejects_files_that_are_not_workbooks() {
        assert!(read(b"not a workbook", &mut Limits::default()).is_err());
        assert!(read(b"PK\x03\x04garbage", &mut Limits::default()).is_err());
    }
}

//! Spreadsheet import and CSV export, end to end: real files, a round trip, hostile workbooks.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::Write as _;

use chrono::NaiveDate;
use plancraft_core::model::{Project, TaskRef};
use plancraft_core::tabular::{self, Field, ImportOptions};

fn import_csv(text: &str) -> (Project, Vec<String>) {
    let sheets = tabular::read_bytes(text.as_bytes(), "csv").unwrap();
    let (_, grid) = &sheets.sheets[0];
    tabular::build(grid, &ImportOptions::guess(grid, "Imported")).unwrap()
}

/// The predecessors of each task, by name.
fn links_by_name(p: &Project) -> Vec<(String, Vec<String>)> {
    p.tasks
        .iter()
        .map(|t| {
            let preds = t
                .predecessors
                .iter()
                .filter_map(|d| match d.predecessor {
                    TaskRef::Local(id) => p.task(id).map(|x| format!("{}{:?}{:?}", x.name, d.kind, d.lag)),
                    TaskRef::External { .. } => None,
                })
                .collect();
            (t.name.clone(), preds)
        })
        .collect()
}

/// Exporting the demo plan and importing the CSV gives the same plan structure back.
#[test]
fn a_plan_exported_to_csv_imports_back_the_same() {
    let original = plancraft_core::sample::demo_project(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    let mut original = original;
    plancraft_core::schedule::recalculate(&mut original);
    let (back, notes) = import_csv(&tabular::to_csv(&original));

    assert_eq!(back.tasks.len(), original.tasks.len(), "{notes:?}");
    for (a, b) in original.tasks.iter().zip(&back.tasks) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.outline_level, b.outline_level, "{}", a.name);
        assert_eq!(a.is_milestone(), b.is_milestone(), "{}", a.name);
        if !a.is_summary() && a.recurrence.is_none() {
            assert_eq!(a.sched.duration, b.duration, "{}", a.name);
        }
    }
    assert_eq!(links_by_name(&original), links_by_name(&back));
    for (i, t) in original.tasks.iter().enumerate() {
        assert_eq!(original.format_resource_names(t.id), back.format_resource_names(back.tasks[i].id), "{}", t.name);
    }
    assert!(notes.iter().any(|n| n.contains("not imported: Unique ID")), "the columns it can't use are named: {notes:?}");
}

/// A minimal workbook: the parts Excel needs, one sheet, inline strings, a number and a date.
fn xlsx(sheet_rows: &[&[&str]]) -> Vec<u8> {
    let cell = |r: usize, c: usize, v: &str| {
        let col = char::from(b'A' + u8::try_from(c).unwrap());
        let at = format!("{col}{}", r + 1);
        if let Some(n) = v.strip_prefix('#') {
            format!(r#"<c r="{at}"><v>{n}</v></c>"#)
        } else if let Some(n) = v.strip_prefix('@') {
            // A date: style 1 is a date format (see styles.xml below).
            format!(r#"<c r="{at}" s="1"><v>{n}</v></c>"#)
        } else {
            format!(r#"<c r="{at}" t="inlineStr"><is><t>{v}</t></is></c>"#)
        }
    };
    let rows: String = sheet_rows
        .iter()
        .enumerate()
        .map(|(r, cells)| {
            format!(r#"<row r="{}">{}</row>"#, r + 1, cells.iter().enumerate().map(|(c, v)| cell(r, c, v)).collect::<String>())
        })
        .collect();
    let files = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#.to_string(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_string(),
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Plan" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_string(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_string(),
        ),
        (
            "xl/styles.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"#.to_string(),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{rows}</sheetData></worksheet>"#),
        ),
    ];
    let mut out = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut out);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in files {
        zip.start_file(name, options).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    out.into_inner()
}

#[test]
fn an_excel_workbook_imports_with_its_dates_and_numbers() {
    // 46314 is 2026-10-19 and 46321 is 2026-10-26 as Excel date serials.
    let bytes = xlsx(&[
        &["Task Name", "Duration", "Start", "Predecessors"],
        &["Design", "#3", "@46314", ""],
        &["Build", "5 days", "", "1"],
        &["Review", "#1", "@46321", ""],
    ]);
    let sheets = tabular::read_bytes(&bytes, "xlsx").unwrap();
    assert_eq!(sheets.sheets.len(), 1);
    let (name, grid) = &sheets.sheets[0];
    assert_eq!(name, "Plan");
    assert_eq!(grid.cell(0, 2), "2026-10-19");
    let options = ImportOptions::guess(grid, "From Excel");
    assert_eq!(options.columns[..4], [Some(Field::Name), Some(Field::Duration), Some(Field::Start), Some(Field::Predecessors)]);
    let (p, notes) = tabular::build(grid, &options).unwrap();
    assert_eq!(p.tasks.len(), 3, "{notes:?}");
    assert_eq!(p.start.date(), NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    assert_eq!(p.tasks[0].duration, 3 * 480);
    assert_eq!(p.tasks[1].sched.start.unwrap().date(), NaiveDate::from_ymd_opt(2026, 10, 22).unwrap());
    assert_eq!(p.tasks[2].sched.start.unwrap().date(), NaiveDate::from_ymd_opt(2026, 10, 26).unwrap());
}

/// A workbook whose archive claims to unpack to gigabytes is refused before anything is
/// unpacked (a "zip bomb").
#[test]
fn a_workbook_that_would_unpack_to_gigabytes_is_refused() {
    let mut bytes = xlsx(&[&["Name"], &["A"]]);
    // Raise the declared size of the first entry in the central directory to 2 GB.
    let at = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    bytes[at + 24..at + 28].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
    let err = tabular::read_bytes(&bytes, "xlsx").unwrap_err();
    assert!(err.contains("unpacks to more than"), "{err}");
}

#[test]
fn a_file_with_only_a_heading_row_or_nothing_is_reported() {
    assert!(tabular::read_bytes(b"", "csv").is_err());
    let sheets = tabular::read_bytes(b"Name,Duration\n", "csv").unwrap();
    let (_, grid) = &sheets.sheets[0];
    assert!(tabular::build(grid, &ImportOptions::guess(grid, "x")).is_err());
}

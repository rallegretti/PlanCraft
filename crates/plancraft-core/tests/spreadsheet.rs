//! Spreadsheet import and CSV export, end to end: real files, a round trip, hostile workbooks.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::Write as _;

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
    let original = plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap());
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
    // The dates follow from the links again: only a milestone whose date came from its member
    // tasks (which a CSV has no column for) keeps it as a constraint. Regression (review): every
    // task with a predecessor was pinned.
    let made_of_members = original.tasks.iter().filter(|t| !t.members.is_empty()).count();
    let constrained = back.tasks.iter().filter(|t| t.constraint.kind != plancraft_core::model::ConstraintKind::Asap).count();
    assert!(constrained <= made_of_members, "{constrained} constraints for {made_of_members} milestones: {notes:?}");
    for (a, b) in original.tasks.iter().zip(&back.tasks) {
        if a.members.is_empty() && !a.is_summary() && a.recurrence.is_none() {
            assert_eq!(a.sched.start, b.sched.start, "{}", a.name);
        }
    }
}

/// A minimal workbook around a sheet's `<sheetData>` (and an optional shared-string table).
fn workbook(sheet_data: &str, shared: Option<&str>) -> Vec<u8> {
    let mut files = vec![
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>"#.to_string(),
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
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#.to_string(),
        ),
        (
            "xl/styles.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"#.to_string(),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{sheet_data}</sheetData></worksheet>"#),
        ),
    ];
    if let Some(sst) = shared {
        files.push(("xl/sharedStrings.xml", format!(r#"<?xml version="1.0" encoding="UTF-8"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">{sst}</sst>"#)));
    }
    let mut out = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut out);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in files {
        zip.start_file(name, options).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    out.into_inner()
}

/// A minimal workbook: one sheet, inline strings, a number and a date.
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
    workbook(&rows, None)
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
    assert_eq!(p.start.date(), plancraft_core::dates::ymd(2026, 10, 19).unwrap());
    assert_eq!(p.tasks[0].duration, 3 * 480);
    assert_eq!(p.tasks[1].sched.start.unwrap().date(), plancraft_core::dates::ymd(2026, 10, 22).unwrap());
    assert_eq!(p.tasks[2].sched.start.unwrap().date(), plancraft_core::dates::ymd(2026, 10, 26).unwrap());
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

/// Regression (review): a sheet with cells only in A1 and XFD1048576 made calamine allocate a
/// grid spanning them (550 GB) and abort the process; cells are now read one at a time.
#[test]
fn a_sheet_with_cells_far_apart_reads_in_little_memory() {
    let bytes = workbook(
        r#"<row r="1"><c r="A1" t="inlineStr"><is><t>Name</t></is></c></row><row r="1048576"><c r="XFD1048576" t="inlineStr"><is><t>far</t></is></c></row>"#,
        None,
    );
    let sheets = tabular::read_bytes(&bytes, "xlsx").unwrap();
    let (_, grid) = &sheets.sheets[0];
    assert_eq!(grid.headers, ["Name"]);
    assert!(grid.rows.iter().all(|r| r.len() <= tabular::MAX_COLUMNS));
    assert!(sheets.notes.iter().any(|n| n.contains("columns")), "{:?}", sheets.notes);
}

/// Regression (review): one 1 MB shared string used by thousands of cells was copied into each
/// (gigabytes); each copy is now shortened and counted against the file's budget.
#[test]
fn a_shared_string_used_by_many_cells_stays_within_the_budget() {
    let big = "x".repeat(1024 * 1024);
    let cells: String = (1..=4000).map(|r| format!(r#"<row r="{r}"><c r="A{r}" t="s"><v>0</v></c></row>"#)).collect();
    let bytes = workbook(&cells, Some(&format!("<si><t>{big}</t></si>")));
    let sheets = tabular::read_bytes(&bytes, "xlsx").unwrap();
    let kept: usize = sheets.sheets.iter().flat_map(|(_, g)| g.rows.iter().flatten()).map(String::len).sum();
    assert!(kept <= tabular::MAX_TEXT_BYTES, "{kept} bytes kept");
    assert!(sheets.notes.iter().any(|n| n.contains("only its first part") || n.contains("shortened")), "{:?}", sheets.notes);
}

#[test]
fn legacy_xls_and_ods_files_are_not_read() {
    assert!(!tabular::is_table_path(std::path::Path::new("old.xls")));
    assert!(!tabular::is_table_path(std::path::Path::new("sheet.ods")));
    assert!(tabular::is_table_path(std::path::Path::new("plan.XLSX")));
}

#[test]
fn a_file_with_only_a_heading_row_or_nothing_is_reported() {
    assert!(tabular::read_bytes(b"", "csv").is_err());
    let sheets = tabular::read_bytes(b"Name,Duration\n", "csv").unwrap();
    let (_, grid) = &sheets.sheets[0];
    assert!(tabular::build(grid, &ImportOptions::guess(grid, "x")).is_err());
}

/// Review (Files, CSV export): resources, assignments and logged time export as CSV too, each with a row per
/// item, work in hours, the same costs as the plan, and formulas kept as text.
#[test]
fn resources_assignments_and_logged_time_export_as_csv() {
    use plancraft_core::tabular::CsvTable;
    let mut p = plancraft_core::sample::demo_project(plancraft_core::dates::ymd(2026, 10, 19).unwrap());
    plancraft_core::schedule::recalculate(&mut p);
    let a = p.assignments[0].clone();
    let day = plancraft_core::dates::ymd(2026, 10, 20).unwrap();
    p.log_time(a.task, a.resource, day, 90, "=HYPERLINK(\"x\")").unwrap();
    plancraft_core::schedule::recalculate(&mut p);
    let read = |table: CsvTable| {
        let csv = tabular::table_to_csv(&p, table);
        assert!(csv.starts_with('\u{feff}'), "{table:?} starts with a byte order mark");
        let sheets = tabular::read_bytes(csv.as_bytes(), "csv").unwrap();
        sheets.sheets[0].1.clone()
    };
    let col =
        |g: &tabular::Grid, name: &str| g.headers.iter().position(|h| h == name).unwrap_or_else(|| panic!("no {name} in {:?}", g.headers));

    // Resources: one row each; work and cost add up to the plan's.
    let g = read(CsvTable::Resources);
    assert_eq!(g.rows.len(), p.resources.len());
    let r = &p.resources[0];
    let row = g.rows.iter().position(|row| row[col(&g, "Name")] == r.name).unwrap();
    assert_eq!(g.cell(row, col(&g, "Unique ID")), r.id.0.to_string());
    let work: f64 = (0..g.rows.len()).map(|i| g.cell(i, col(&g, "Work (h)")).parse::<f64>().unwrap()).sum();
    let task_work: i64 = p.tasks.iter().filter(|t| !t.sched.is_summary).map(|t| t.sched.work).sum();
    assert!((work - task_work as f64 / 60.0).abs() < 0.1, "{work} h against {task_work} min");
    let cost: f64 = (0..g.rows.len()).map(|i| g.cell(i, col(&g, "Cost")).parse::<f64>().unwrap()).sum();
    let task_cost: f64 = p.tasks.iter().filter(|t| !t.sched.is_summary).map(|t| t.sched.cost - t.fixed_cost).sum();
    assert!((cost - task_cost).abs() < 1.0, "{cost} against {task_cost}");
    let row = g.rows.iter().position(|row| row[col(&g, "Unique ID")] == a.resource.0.to_string()).unwrap();
    assert!(g.cell(row, col(&g, "Actual Work (h)")).parse::<f64>().unwrap() >= 1.5);

    // Assignments: one row each, with the task and resource by name and the logged time.
    let g = read(CsvTable::Assignments);
    assert_eq!(g.rows.len(), p.assignments.len());
    let task = p.task(a.task).unwrap();
    let row = (0..g.rows.len())
        .find(|&i| {
            g.cell(i, col(&g, "Task Unique ID")) == a.task.0.to_string()
                && g.cell(i, col(&g, "Resource Unique ID")) == a.resource.0.to_string()
        })
        .unwrap();
    assert_eq!(g.cell(row, col(&g, "Task")), task.name);
    assert_eq!(g.cell(row, col(&g, "Resource")), p.resource(a.resource).unwrap().name);
    assert_eq!(g.cell(row, col(&g, "Work (h)")), format!("{:.2}", a.work as f64 / 60.0));
    let logged: i64 = p.time_entries.iter().filter(|e| (e.task, e.resource) == (a.task, a.resource)).map(|e| e.minutes).sum();
    assert_eq!(g.cell(row, col(&g, "Actual Work (h)")), format!("{:.2}", (a.actual_work + logged) as f64 / 60.0));
    let id = p.task_index(a.task).unwrap() + 1;
    assert_eq!(g.cell(row, col(&g, "Task ID")), id.to_string());

    // Logged time: the entry, its hours, and the note kept as text.
    let g = read(CsvTable::Time);
    assert_eq!(g.rows.len(), p.time_entries.len());
    let dates: Vec<&str> = (0..g.rows.len()).map(|i| g.cell(i, col(&g, "Date"))).collect();
    assert!(dates.windows(2).all(|w| w[0] <= w[1]), "by date: {dates:?}");
    let row = (0..g.rows.len()).find(|&i| g.cell(i, col(&g, "Hours")) == "1.50").unwrap();
    assert_eq!(g.cell(row, col(&g, "Date")), "2026-10-20");
    assert_eq!(g.cell(row, col(&g, "Task")), task.name);
    assert_eq!(g.cell(row, col(&g, "Note")), "=HYPERLINK(\"x\")", "reads back as the note");
    assert!(tabular::table_to_csv(&p, CsvTable::Time).contains("\"'=HYPERLINK(\"\"x\"\")\""), "a spreadsheet sees text, not a formula");

    // An empty plan exports just the headings.
    let empty = Project::new("Empty", plancraft_core::dates::ymd(2026, 10, 19).unwrap());
    for table in CsvTable::ALL {
        let csv = tabular::table_to_csv(&empty, table);
        assert_eq!(csv.lines().count(), 1, "{table:?}: {csv}");
    }
    assert_eq!(CsvTable::from_key(" Time "), Some(CsvTable::Time));
    assert_eq!(CsvTable::from_key("people"), None);
}

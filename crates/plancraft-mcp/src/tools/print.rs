//! Printing tools: Page Setup (saved with the plan) and PDF export of the Gantt chart or task
//! table.

use std::path::PathBuf;

use plancraft_core::print::{self, LegendPlacement, Orientation, PageSetup, Paper, TimescaleFit};
use serde_json::{Value, json};

use super::*;
use crate::args::*;

const PAPERS: [&str; 5] = ["letter", "legal", "tabloid", "a4", "a3"];
const ORIENTATIONS: [&str; 2] = ["landscape", "portrait"];
const LEGENDS: [&str; 3] = ["every_page", "legend_page", "none"];
const VIEWS: [&str; 3] = ["gantt", "tracking", "table"];

fn setup_props() -> serde_json::Map<String, Value> {
    let Value::Object(m) = json!({
        "paper": string_enum("Paper size.", &PAPERS),
        "orientation": string_enum("Page orientation.", &ORIENTATIONS),
        "margin_mm": number("Margin on every side in millimetres (0-50)."),
        "scale_percent": integer("Print at this percentage of normal size (25-400)."),
        "pages_wide": integer("Spread the timescale over this many pages across (1-20); 0 = the view's zoom."),
        "repeat_columns": integer("Repeat the first N table columns on pages to the right."),
        "legend": string_enum("Where the bar legend prints.", &LEGENDS),
        "header": { "type": "array", "items": { "type": "string" }, "description": "Up to 3 strings: left, centre, right. Codes: [Project] [Manager] [Company] [View] [Date] [Page] [Pages] [Start] [Finish]." },
        "footer": { "type": "array", "items": { "type": "string" }, "description": "Up to 3 strings: left, centre, right (same codes as header)." },
        "print_blank_pages": boolean("Print timescale pages that have no bars."),
    }) else {
        return serde_json::Map::new();
    };
    m
}

pub fn tools() -> Vec<Tool> {
    let mut export = setup_props();
    for (k, v) in [
        ("path", string("Where to write the PDF (.pdf is added when missing).")),
        (
            "view",
            string_enum(
                "What to print (default gantt): the Gantt chart, the Tracking Gantt (with baselines), or the task table alone.",
                &VIEWS,
            ),
        ),
        ("from", date("First date of the timescale (default: the project start).")),
        ("to", date("Last date of the timescale (default: the project finish).")),
        ("overwrite", boolean("Replace an existing file at 'path' (default false).")),
    ] {
        export.insert(k.into(), v);
    }
    vec![
        Tool::new(
            "set_page_setup",
            "Set Page Setup",
            "Change the plan's Page Setup (paper, orientation, margins, scale, timescale width, legend, header and footer), saved with the plan. Omitted settings keep their values. Returns the whole setup.",
            schema(Value::Object(setup_props()), &[]),
            set_page_setup,
        ),
        Tool::new(
            "export_pdf",
            "Export a PDF",
            "Print the plan to a PDF file: the Gantt chart (task table with bars, links, timescale, legend, header and footer) or the task table, paginated with the plan's Page Setup. Page Setup arguments given here apply to this export only.",
            schema(Value::Object(export), &["path"]),
            export_pdf,
        ),
    ]
}

fn setup_json(s: &PageSetup) -> Value {
    serde_json::to_value(s).unwrap_or(Value::Null)
}

fn texts(a: &Args, key: &str) -> ToolResult<Option<[String; 3]>> {
    let Some(items) = a.array(key)? else { return Ok(None) };
    if items.len() > 3 {
        return err(format!("'{key}' takes at most 3 strings (left, centre, right)"));
    }
    let mut out: [String; 3] = Default::default();
    for (slot, v) in out.iter_mut().zip(items) {
        *slot = v.as_str().ok_or_else(|| ToolError(format!("'{key}' must be an array of strings")))?.to_string();
    }
    Ok(Some(out))
}

fn small(a: &Args, key: &str, max: u64) -> ToolResult<Option<u64>> {
    match a.u64(key)? {
        Some(v) if v > max => err(format!("'{key}' must be at most {max}")),
        v => Ok(v),
    }
}

/// Applies the Page Setup arguments present in `a` to `s`.
fn apply(s: &mut PageSetup, a: &Args) -> ToolResult<()> {
    if let Some(p) = a.str("paper")? {
        s.paper = match p.trim().to_ascii_lowercase().as_str() {
            "letter" => Paper::Letter,
            "legal" => Paper::Legal,
            "tabloid" => Paper::Tabloid,
            "a4" => Paper::A4,
            "a3" => Paper::A3,
            _ => return err(format!("'paper' must be one of {}", PAPERS.join(", "))),
        };
    }
    if let Some(o) = a.str("orientation")? {
        s.orientation = match o.trim().to_ascii_lowercase().as_str() {
            "landscape" => Orientation::Landscape,
            "portrait" => Orientation::Portrait,
            _ => return err("'orientation' must be landscape or portrait"),
        };
    }
    if let Some(m) = a.f64("margin_mm")? {
        if !m.is_finite() || !(0.0..=f64::from(print::MAX_MARGIN_MM)).contains(&m) {
            return err(format!("'margin_mm' must be between 0 and {}", print::MAX_MARGIN_MM));
        }
        s.margin_mm = m as f32;
    }
    if let Some(v) = small(a, "scale_percent", u64::from(print::MAX_SCALE))? {
        if v < u64::from(print::MIN_SCALE) {
            return err(format!("'scale_percent' must be at least {}", print::MIN_SCALE));
        }
        s.scale_percent = v as u16;
    }
    if let Some(v) = small(a, "pages_wide", u64::from(print::MAX_PAGES_WIDE))? {
        s.timescale = if v == 0 { TimescaleFit::AsShown } else { TimescaleFit::PagesWide(v as u8) };
    }
    if let Some(v) = small(a, "repeat_columns", 20)? {
        s.repeat_columns = v as u8;
    }
    if let Some(l) = a.str("legend")? {
        s.legend = match l.trim().to_ascii_lowercase().as_str() {
            "every_page" => LegendPlacement::EveryPage,
            "legend_page" => LegendPlacement::LegendPage,
            "none" => LegendPlacement::None,
            _ => return err(format!("'legend' must be one of {}", LEGENDS.join(", "))),
        };
    }
    if let Some(h) = texts(a, "header")? {
        s.header = h;
    }
    if let Some(f) = texts(a, "footer")? {
        s.footer = f;
    }
    if let Some(b) = a.bool("print_blank_pages")? {
        s.print_blank_pages = b;
    }
    s.sanitize();
    Ok(())
}

fn set_page_setup(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Page Setup",
        |p, _| {
            let mut s = p.page_setup.clone();
            apply(&mut s, a)?;
            p.set_page_setup(s);
            Ok(())
        },
        |doc, ()| Ok(json!({ "page_setup": setup_json(&doc.project.page_setup) })),
    )
}

fn export_pdf(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.require_file_access("Exporting files")?;
    let raw = a.req_str("path")?.trim();
    if raw.is_empty() {
        return err("'path' is empty");
    }
    let path = PathBuf::from(raw);
    let path = match path.extension() {
        None => path.with_extension("pdf"),
        Some(e) if e.eq_ignore_ascii_case("pdf") => path,
        Some(_) => return err("'path' must end in .pdf"),
    };
    let path = if path.is_absolute() { path } else { std::env::current_dir().map(|c| c.join(&path)).unwrap_or(path) };
    if path.exists() && !a.bool("overwrite")?.unwrap_or(false) {
        return err(format!("{} already exists; pass overwrite=true to replace it", path.display()));
    }
    let view = a.str("view")?.unwrap_or("gantt").trim().to_ascii_lowercase();
    if !VIEWS.contains(&view.as_str()) {
        return err(format!("'view' must be one of {}", VIEWS.join(", ")));
    }
    let from = a.get("from").filter(|v| !v.is_null()).map(|v| parse_date(v, "from")).transpose()?;
    let to = a.get("to").filter(|v| !v.is_null()).map(|v| parse_date(v, "to")).transpose()?;
    if let (Some(f), Some(t)) = (from, to)
        && t < f
    {
        return err("'to' is before 'from'");
    }
    let (bytes, pages, truncated) = ctx.read(a, |doc| {
        let p = &doc.project;
        let mut setup = p.page_setup.clone();
        apply(&mut setup, a)?;
        let chart = match view.as_str() {
            "gantt" => Some(print::ChartOptions::default()),
            "tracking" => Some(print::ChartOptions { critical: true, baseline: true, ..Default::default() }),
            _ => None,
        };
        let mut sheet = print::task_sheet(p, chart.is_some());
        if view == "tracking" {
            sheet.title = "Tracking Gantt".into();
        }
        let out = print::layout(&print::PrintJob {
            project: p,
            setup: &setup,
            sheet: &sheet,
            chart,
            from,
            to,
            printed: chrono::Local::now().naive_local(),
            palette: print::layout::Palette::default(),
        });
        Ok((print::pdf::write(&out), out.pages.len(), out.truncated))
    })?;
    plancraft_core::io::write_atomic(&path, &bytes, plancraft_core::io::Privacy::KeepExisting)
        .map_err(|e| ToolError(format!("could not write {}: {e}", path.display())))?;
    Ok(json!({
        "path": path.display().to_string(),
        "pages": pages,
        "bytes": bytes.len(),
        "truncated": truncated,
    }))
}

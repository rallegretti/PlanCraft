//! Printing: page setup, page layout of task and resource tables with a Gantt chart, of the
//! Network Diagram and the Task Board, and PDF output.
//!
//! Layout produces [`Page`]s of simple drawing items (rectangles, lines, polygons, text) in
//! points with the origin at the top-left. The same pages are written to PDF ([`pdf::write`])
//! and drawn on screen by the app's Print Preview, so what you preview is what prints.

pub mod board;
pub mod canvas;
pub mod font;
pub mod layout;
pub mod network;
pub mod pdf;
mod subset;

use serde::{Deserialize, Serialize};

pub use canvas::{CanvasJob, layout_canvas};
pub use layout::{Align, ChartOptions, PrintCell, PrintColumn, PrintJob, PrintRow, RowKind, Sheet, layout, task_sheet};

/// An RGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

pub const fn pt(x: f32, y: f32) -> Pt {
    Pt { x, y }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    pub width: f32,
    pub color: Rgb,
    /// Dash length (gaps are as long), or none for a solid line.
    pub dash: Option<f32>,
}

impl Stroke {
    pub const fn new(width: f32, color: Rgb) -> Self {
        Self { width, color, dash: None }
    }

    pub const fn dashed(width: f32, color: Rgb, dash: f32) -> Self {
        Self { width, color, dash: Some(dash) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// One drawing item on a page.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Rect {
        rect: Rect,
        fill: Option<Rgb>,
        stroke: Option<Stroke>,
    },
    Line {
        points: Vec<Pt>,
        stroke: Stroke,
    },
    Polygon {
        points: Vec<Pt>,
        fill: Rgb,
    },
    /// Text with its baseline starting at `at`.
    Text {
        at: Pt,
        size: f32,
        color: Rgb,
        bold: bool,
        text: String,
    },
    /// Items clipped to a rectangle.
    Clip {
        rect: Rect,
        items: Vec<Item>,
    },
}

/// One printed page, sizes in points (1/72 inch).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    pub items: Vec<Item>,
}

/// A printed document.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Document {
    pub title: String,
    pub pages: Vec<Page>,
    /// More pages were needed than [`layout::MAX_PAGES`]; the rest were left out.
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Paper {
    #[default]
    Letter,
    Legal,
    Tabloid,
    A4,
    A3,
}

impl Paper {
    pub const ALL: [Paper; 5] = [Paper::Letter, Paper::Legal, Paper::Tabloid, Paper::A4, Paper::A3];

    /// Portrait width and height in points.
    pub fn size(self) -> (f32, f32) {
        match self {
            Paper::Letter => (612.0, 792.0),
            Paper::Legal => (612.0, 1008.0),
            Paper::Tabloid => (792.0, 1224.0),
            Paper::A4 => (595.28, 841.89),
            Paper::A3 => (841.89, 1190.55),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Paper::Letter => "Letter (8.5 × 11 in)",
            Paper::Legal => "Legal (8.5 × 14 in)",
            Paper::Tabloid => "Tabloid (11 × 17 in)",
            Paper::A4 => "A4 (210 × 297 mm)",
            Paper::A3 => "A3 (297 × 420 mm)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Orientation {
    Portrait,
    #[default]
    Landscape,
}

/// Where the chart legend prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LegendPlacement {
    /// At the bottom of every page.
    #[default]
    EveryPage,
    /// On a page of its own after the others.
    LegendPage,
    None,
}

impl LegendPlacement {
    pub const ALL: [LegendPlacement; 3] = [LegendPlacement::EveryPage, LegendPlacement::LegendPage, LegendPlacement::None];

    pub fn label(self) -> &'static str {
        match self {
            LegendPlacement::EveryPage => "On every page",
            LegendPlacement::LegendPage => "On a legend page",
            LegendPlacement::None => "None",
        }
    }
}

/// How wide the timescale prints.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum TimescaleFit {
    /// The printed dates fill this many pages across.
    PagesWide(u8),
    /// The view's zoom on screen (pixels per day print as points per day).
    AsShown,
}

impl Default for TimescaleFit {
    fn default() -> Self {
        TimescaleFit::PagesWide(1)
    }
}

pub const MAX_PAGES_WIDE: u8 = 20;
pub const MIN_SCALE: u16 = 25;
pub const MAX_SCALE: u16 = 400;
pub const MAX_MARGIN_MM: f32 = 50.0;
pub const MAX_HEADER_CHARS: usize = 200;

/// Page Setup, saved with the plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageSetup {
    pub paper: Paper,
    pub orientation: Orientation,
    /// Margin on every side, in millimetres.
    pub margin_mm: f32,
    /// Everything prints at this percentage of its normal size.
    pub scale_percent: u16,
    pub timescale: TimescaleFit,
    /// The first this many table columns repeat on pages to the right.
    pub repeat_columns: u8,
    pub legend: LegendPlacement,
    /// Left, centre and right header text. Codes: [Project] [Manager] [Company] [View] [Date]
    /// [Page] [Pages] [Start] [Finish].
    pub header: [String; 3],
    pub footer: [String; 3],
    /// Print pages to the right of the table that have no bars on them.
    pub print_blank_pages: bool,
    /// Print every column of the table, not only those left of the view's divider.
    pub print_all_columns: bool,
}

/// Reads a plan's Page Setup without ever refusing the plan: values of the wrong type or out of
/// their type's range (a scale of 70000, a paper size from a newer version) fall back to the
/// default or are clamped, and [`PageSetup::sanitize`] then brings them into range.
pub fn lenient_page_setup<'de, D: serde::Deserializer<'de>>(d: D) -> Result<PageSetup, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(PageSetup::from_json(&v))
}

impl PageSetup {
    /// Page Setup from JSON, field by field (see [`lenient_page_setup`]).
    pub fn from_json(v: &serde_json::Value) -> Self {
        use serde_json::Value;
        let mut s = PageSetup::default();
        let Some(m) = v.as_object() else { return s };
        fn get<T: serde::de::DeserializeOwned>(m: &serde_json::Map<String, Value>, k: &str) -> Option<T> {
            m.get(k).and_then(|v| T::deserialize(v).ok())
        }
        let num = |k: &str| m.get(k).and_then(Value::as_f64).filter(|n| n.is_finite());
        let texts = |k: &str| -> Option<[String; 3]> {
            let items = m.get(k)?.as_array()?;
            let mut out: [String; 3] = Default::default();
            for (slot, item) in out.iter_mut().zip(items) {
                *slot = item.as_str().unwrap_or_default().to_string();
            }
            Some(out)
        };
        if let Some(x) = get(m, "paper") {
            s.paper = x;
        }
        if let Some(x) = get(m, "orientation") {
            s.orientation = x;
        }
        if let Some(x) = get(m, "legend") {
            s.legend = x;
        }
        if let Some(x) = num("margin_mm") {
            s.margin_mm = x as f32;
        }
        if let Some(x) = num("scale_percent") {
            s.scale_percent = x.clamp(0.0, f64::from(u16::MAX)) as u16;
        }
        if let Some(x) = num("repeat_columns") {
            s.repeat_columns = x.clamp(0.0, f64::from(u8::MAX)) as u8;
        }
        match m.get("timescale") {
            Some(Value::String(t)) if t == "AsShown" => s.timescale = TimescaleFit::AsShown,
            Some(Value::Object(t)) => {
                if let Some(n) = t.get("PagesWide").and_then(Value::as_f64).filter(|n| n.is_finite()) {
                    s.timescale = TimescaleFit::PagesWide(n.clamp(0.0, f64::from(u8::MAX)) as u8);
                }
            }
            _ => {}
        }
        if let Some(x) = texts("header") {
            s.header = x;
        }
        if let Some(x) = texts("footer") {
            s.footer = x;
        }
        if let Some(x) = get(m, "print_blank_pages") {
            s.print_blank_pages = x;
        }
        if let Some(x) = get(m, "print_all_columns") {
            s.print_all_columns = x;
        }
        s
    }
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            paper: Paper::Letter,
            orientation: Orientation::Landscape,
            margin_mm: 12.7,
            scale_percent: 100,
            timescale: TimescaleFit::PagesWide(1),
            repeat_columns: 0,
            legend: LegendPlacement::EveryPage,
            header: [String::new(), "[Project]".into(), String::new()],
            footer: ["[View]".into(), "Page [Page] of [Pages]".into(), "Printed [Date]".into()],
            print_blank_pages: true,
            print_all_columns: false,
        }
    }
}

impl PageSetup {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Page width and height in points.
    pub fn page_size(&self) -> (f32, f32) {
        let (w, h) = self.paper.size();
        match self.orientation {
            Orientation::Portrait => (w, h),
            Orientation::Landscape => (h, w),
        }
    }

    pub fn margin_pt(&self) -> f32 {
        let m = if self.margin_mm.is_finite() { self.margin_mm.clamp(0.0, MAX_MARGIN_MM) } else { 12.7 };
        m * 72.0 / 25.4
    }

    pub fn scale(&self) -> f32 {
        f32::from(self.scale_percent.clamp(MIN_SCALE, MAX_SCALE)) / 100.0
    }

    /// Brings every value into range (files and agents are not trusted). Returns whether anything changed.
    pub fn sanitize(&mut self) -> bool {
        let before = self.clone();
        if !self.margin_mm.is_finite() {
            self.margin_mm = 12.7;
        }
        self.margin_mm = self.margin_mm.clamp(0.0, MAX_MARGIN_MM);
        self.scale_percent = self.scale_percent.clamp(MIN_SCALE, MAX_SCALE);
        if let TimescaleFit::PagesWide(n) = &mut self.timescale {
            *n = (*n).clamp(1, MAX_PAGES_WIDE);
        }
        self.repeat_columns = self.repeat_columns.min(20);
        for s in self.header.iter_mut().chain(self.footer.iter_mut()) {
            if s.chars().count() > MAX_HEADER_CHARS {
                *s = s.chars().take(MAX_HEADER_CHARS).collect();
            }
            if s.contains(['\n', '\r']) {
                *s = s.replace(['\n', '\r'], " ");
            }
        }
        *self != before
    }
}

impl crate::model::Project {
    /// Replaces the Page Setup (values are brought into range).
    pub fn set_page_setup(&mut self, mut setup: PageSetup) {
        setup.sanitize();
        self.page_setup = setup;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_setup_sanitizes_hostile_values() {
        let mut s = PageSetup {
            margin_mm: f32::NAN,
            scale_percent: 0,
            timescale: TimescaleFit::PagesWide(255),
            repeat_columns: 200,
            header: ["x".repeat(5000), "a\nb".into(), String::new()],
            ..Default::default()
        };
        assert!(s.sanitize());
        assert_eq!(s.margin_mm, 12.7);
        assert_eq!(s.scale_percent, MIN_SCALE);
        assert_eq!(s.timescale, TimescaleFit::PagesWide(MAX_PAGES_WIDE));
        assert_eq!(s.header[0].chars().count(), MAX_HEADER_CHARS);
        assert_eq!(s.header[1], "a b");
        assert!(!s.sanitize());
        assert!(PageSetup::default().is_default());
    }
}

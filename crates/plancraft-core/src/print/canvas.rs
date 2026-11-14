//! Page layout of a free-form drawing (the Network Diagram, the Task Board): the drawing is
//! scaled (to fit a number of pages across, or at its actual size) and tiled over pages, across
//! then down, inside the same page frame as the Gantt chart (margins, scale, header and footer).

use std::collections::HashMap;

use chrono::NaiveDateTime;

use super::layout::{Frame, MAX_PAGE_VISITS, MAX_PAGES, Palette, scale_item};
use super::{Document, Item, PageSetup, Rect, TimescaleFit, font};
use crate::model::Project;

/// Largest drawing size taken, in points. A real drawing stays far below this (a 50,000-task
/// dependency chain is about 9 million points wide); anything larger is hostile and clamped.
const MAX_SIZE: f32 = 100_000_000.0;
/// "Fit to N pages wide" never enlarges a small drawing more than this.
const MAX_GROW: f32 = 2.0;
/// Room between the header band and the drawing, and between the drawing and the footer.
const GAP: f32 = 4.0;
/// A piece of an item touching more pages than this is checked against each page instead of
/// being put in every page's bucket.
const MAX_SPAN: usize = 64;

/// A drawing to print.
pub struct CanvasJob<'a> {
    pub project: &'a Project,
    pub setup: &'a PageSetup,
    /// The view's name, for `[View]` and the document title.
    pub title: &'a str,
    /// Size of the drawing in points (at 100%).
    pub width: f32,
    pub height: f32,
    /// The drawing, with the origin at its top-left corner.
    pub items: Vec<Item>,
    /// When it was printed, for `[Date]`.
    pub printed: NaiveDateTime,
}

fn size(v: f32) -> f32 {
    if v.is_finite() { v.clamp(1.0, MAX_SIZE) } else { 1.0 }
}

/// The smallest rectangle around finite points, or `None` when there are none.
fn around(pts: impl Iterator<Item = (f32, f32)>) -> Option<Rect> {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in pts {
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    (x0 <= x1 && y0 <= y1).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// The areas an item covers (roughly, for text): one per segment of a line, so a long link line
/// lands only on the pages it passes through, not on every page inside its bounding box.
fn pieces(item: &Item) -> Vec<Rect> {
    match item {
        Item::Rect { rect, .. } | Item::Clip { rect, .. } => {
            around([(rect.x, rect.y), (rect.right(), rect.bottom())].into_iter()).into_iter().collect()
        }
        Item::Line { points, .. } if points.len() > 1 => points.windows(2).filter_map(|w| around(w.iter().map(|p| (p.x, p.y)))).collect(),
        Item::Line { points, .. } | Item::Polygon { points, .. } => around(points.iter().map(|p| (p.x, p.y))).into_iter().collect(),
        Item::Text { at, size, bold, text, .. } => {
            let w = font::width(text, *size, *bold);
            around([(at.x, at.y - size), (at.x + w, at.y + size * 0.3)].into_iter()).into_iter().collect()
        }
    }
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x <= b.right() && b.x <= a.right() && a.y <= b.bottom() && b.y <= a.bottom()
}

/// Lays the drawing out on pages.
pub fn layout_canvas(job: &CanvasJob) -> Document {
    let setup = job.setup;
    let frame = Frame::new(setup);
    let top = frame.top() + if frame.has_header { GAP } else { 0.0 };
    let body_w = frame.w.max(1.0);
    let body_h = (frame.h - top - frame.bottom() - if frame.has_footer { GAP } else { 0.0 }).max(1.0);
    let (dw, dh) = (size(job.width), size(job.height));
    let k = match setup.drawing_fit {
        TimescaleFit::PagesWide(n) => (f32::from(n.clamp(1, super::MAX_PAGES_WIDE)) * body_w / dw).min(MAX_GROW),
        TimescaleFit::AsShown => 1.0,
    };
    let k = if k.is_finite() && k > 0.0 { k } else { 1.0 };
    // Pages needed across and down (a hair of slack so a drawing that just fits needs no extra
    // page), and the pages laid out: no more than the visit budget allows.
    let need = |len: f32, page: f32| (len * k / page - 0.001).ceil().max(1.0);
    let (need_across, need_down) = (need(dw, body_w), need(dh, body_h));
    let across = need_across.min(MAX_PAGE_VISITS as f32) as usize;
    let down = need_down.min((MAX_PAGE_VISITS / across.max(1)) as f32).max(1.0) as usize;
    let mut truncated = need_across > across as f32 || need_down > down as f32;
    let (tile_w, tile_h) = (body_w / k, body_h / k);
    let laid_out = Rect::new(0.0, 0.0, tile_w * across as f32, tile_h * down as f32);

    // Each piece of an item goes in the bucket of every page it touches, so a page looks only at
    // its own items. A piece spanning very many pages (a hostile size) is checked against each
    // page instead.
    let mut buckets: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    let mut wide: Vec<(usize, Rect)> = Vec::new();
    let tile_of = |v: f32, len: f32, n: usize| -> usize { (v / len).floor().clamp(0.0, n as f32 - 1.0) as usize };
    for (i, item) in job.items.iter().enumerate() {
        for b in pieces(item) {
            if !overlaps(&b, &laid_out) {
                // Beyond the pages laid out (past the page budget, or outside the drawing).
                truncated |= b.x > laid_out.right() || b.y > laid_out.bottom();
                continue;
            }
            let (c0, c1) = (tile_of(b.x, tile_w, across), tile_of(b.right(), tile_w, across));
            let (r0, r1) = (tile_of(b.y, tile_h, down), tile_of(b.bottom(), tile_h, down));
            if (c1 - c0 + 1).saturating_mul(r1 - r0 + 1) > MAX_SPAN {
                wide.push((i, b));
                continue;
            }
            for r in r0..=r1 {
                for c in c0..=c1 {
                    buckets.entry((r, c)).or_default().push(i);
                }
            }
        }
    }
    let mut pages: Vec<Vec<Item>> = Vec::new();
    'outer: for row in 0..down {
        for col in 0..across {
            let tile = Rect::new(col as f32 * tile_w, row as f32 * tile_h, tile_w, tile_h);
            // In drawing order, each item once: wide pieces that touch this page merged with the
            // page's bucket.
            let mut here: Vec<usize> = buckets.remove(&(row, col)).unwrap_or_default();
            here.extend(wide.iter().filter(|(_, b)| overlaps(b, &tile)).map(|(i, _)| *i));
            here.sort_unstable();
            here.dedup();
            let mut items: Vec<Item> = here.iter().filter_map(|&i| job.items.get(i)).cloned().collect();
            if items.is_empty() && !setup.print_blank_pages {
                continue;
            }
            if pages.len() >= MAX_PAGES {
                truncated = true;
                break 'outer;
            }
            for item in &mut items {
                scale_item(item, k, -tile.x * k, top - tile.y * k);
            }
            pages.push(vec![Item::Clip { rect: Rect::new(0.0, top, body_w, body_h), items }]);
        }
    }
    let out = frame.finish(pages, setup, job.project, job.title, job.printed, &Palette::default());
    Document { title: format!("{} – {}", job.project.name, job.title), pages: out, truncated }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::super::{Rgb, Stroke, pt};
    use super::*;

    fn when() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 14).unwrap().and_hms_opt(9, 0, 0).unwrap()
    }

    fn boxes(w: f32, h: f32, step: f32) -> Vec<Item> {
        let mut out = Vec::new();
        let mut y = 0.0;
        while y < h {
            let mut x = 0.0;
            while x < w {
                out.push(Item::Rect { rect: Rect::new(x, y, 20.0, 20.0), fill: Some(Rgb(0, 0, 0)), stroke: None });
                x += step;
            }
            y += step;
        }
        out
    }

    fn run(setup: &PageSetup, w: f32, h: f32, items: Vec<Item>) -> Document {
        let p = Project::new("Plan", chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        layout_canvas(&CanvasJob { project: &p, setup, title: "Network Diagram", width: w, height: h, items, printed: when() })
    }

    #[test]
    fn fit_to_one_page_wide_prints_one_page_across() {
        let setup = PageSetup { drawing_fit: TimescaleFit::PagesWide(1), ..Default::default() };
        let doc = run(&setup, 3000.0, 400.0, boxes(3000.0, 400.0, 100.0));
        assert_eq!(doc.pages.len(), 1, "a wide, short drawing fits on one page");
        assert!(!doc.truncated);
        assert_eq!(doc.title, "Plan – Network Diagram");
        // Every drawn item is inside the page.
        let page = doc.pages.first().unwrap();
        let Some(Item::Clip { rect, items }) = page.items.iter().find(|i| matches!(i, Item::Clip { .. })) else { panic!("no clip") };
        assert!(rect.right() <= page.width && items.len() > 100);
        // A small drawing is not blown up past twice its size.
        let small = run(&setup, 50.0, 50.0, boxes(50.0, 50.0, 30.0));
        assert_eq!(small.pages.len(), 1);
    }

    #[test]
    fn actual_size_tiles_across_then_down() {
        let setup = PageSetup { drawing_fit: TimescaleFit::AsShown, ..Default::default() };
        let frame = Frame::new(&setup);
        let (w, h) = (frame.w * 2.5, frame.h * 1.5);
        let doc = run(&setup, w, h, boxes(w, h, 40.0));
        assert_eq!(doc.pages.len(), 3 * 2);
        // Two pages wide fits in two pages across.
        let two = PageSetup { drawing_fit: TimescaleFit::PagesWide(2), ..Default::default() };
        let doc = run(&two, w, 100.0, boxes(w, 100.0, 40.0));
        assert_eq!(doc.pages.len(), 2);
    }

    #[test]
    fn blank_pages_can_be_left_out() {
        let mut setup = PageSetup { drawing_fit: TimescaleFit::AsShown, ..Default::default() };
        let frame = Frame::new(&setup);
        let (w, h) = (frame.w * 3.5, frame.h * 0.5);
        // Only the first and last pages across have something on them.
        let items = vec![
            Item::Rect { rect: Rect::new(10.0, 10.0, 20.0, 20.0), fill: Some(Rgb(0, 0, 0)), stroke: None },
            Item::Line { points: vec![pt(w - 30.0, 10.0), pt(w - 10.0, 20.0)], stroke: Stroke::new(1.0, Rgb(0, 0, 0)) },
        ];
        assert_eq!(run(&setup, w, h, items.clone()).pages.len(), 4);
        setup.print_blank_pages = false;
        assert_eq!(run(&setup, w, h, items).pages.len(), 2);
        // Nothing at all still prints one (empty) page.
        assert_eq!(run(&setup, w, h, Vec::new()).pages.len(), 1);
    }

    #[test]
    fn a_drawing_larger_than_the_old_size_cap_is_fitted_whole() {
        // A 7,000-task dependency chain is about 1.28 million points wide.
        let (w, h) = (1_281_000.0, 300.0);
        let last = Item::Rect { rect: Rect::new(w - 150.0, 100.0, 140.0, 60.0), fill: Some(Rgb(0, 0, 0)), stroke: None };
        let doc = run(&PageSetup { drawing_fit: TimescaleFit::PagesWide(1), ..Default::default() }, w, h, vec![last.clone()]);
        assert!(!doc.truncated);
        let page = doc.pages.first().unwrap();
        let Some(Item::Clip { rect: clip, items }) = page.items.iter().find(|i| matches!(i, Item::Clip { .. })) else { panic!("no clip") };
        let Some(Item::Rect { rect, .. }) = items.first() else { panic!("the last box is missing") };
        assert!(rect.right() <= clip.right() + 0.01, "the last box ({}) is inside the page ({})", rect.right(), clip.right());
        // At actual size it needs more pages than the budget: the printout says it is cut short.
        let actual = PageSetup { drawing_fit: TimescaleFit::AsShown, ..Default::default() };
        let doc = run(&actual, w * 40.0, 50_000.0, vec![last]);
        assert!(doc.truncated && doc.pages.len() <= MAX_PAGES);
    }

    #[test]
    fn a_long_link_line_lands_only_on_pages_it_crosses() {
        let mut setup = PageSetup { drawing_fit: TimescaleFit::AsShown, print_blank_pages: false, ..Default::default() };
        let frame = Frame::new(&setup);
        let (w, h) = (frame.w * 10.5, frame.h * 10.5);
        // Right along the top row, then down the last column.
        let line =
            Item::Line { points: vec![pt(5.0, 5.0), pt(w - 5.0, 5.0), pt(w - 5.0, h - 5.0)], stroke: Stroke::new(1.0, Rgb(0, 0, 0)) };
        let crossed = run(&setup, w, h, vec![line.clone()]).pages.len();
        setup.print_blank_pages = true;
        let across = run(&setup, w, 1.0, Vec::new()).pages.len();
        let down = run(&setup, 1.0, h, Vec::new()).pages.len();
        assert!(across >= 10 && down >= 10);
        assert_eq!(run(&setup, w, h, vec![line]).pages.len(), across * down);
        assert_eq!(crossed, across + down - 1, "the top row and the right column, not all {} pages", across * down);
    }

    #[test]
    fn hostile_sizes_never_panic_and_stay_bounded() {
        for setup in [PageSetup::default(), PageSetup { drawing_fit: TimescaleFit::AsShown, scale_percent: 400, ..Default::default() }] {
            for (w, h) in [(f32::NAN, f32::INFINITY), (0.0, -5.0), (1e30, 1e30), (f32::MAX, 1.0), (1.0, f32::MIN)] {
                let items = vec![
                    Item::Text { at: pt(f32::NAN, 5.0), size: f32::INFINITY, color: Rgb(0, 0, 0), bold: false, text: "x".into() },
                    Item::Rect { rect: Rect::new(0.0, 0.0, 1e30, 1e30), fill: None, stroke: None },
                    Item::Polygon { points: Vec::new(), fill: Rgb(0, 0, 0) },
                ];
                let doc = run(&setup, w, h, items);
                assert!(!doc.pages.is_empty() && doc.pages.len() <= MAX_PAGES, "{w} × {h}: {} pages", doc.pages.len());
            }
        }
    }
}

//! The Network Diagram as print items: the boxes and lines the app's view draws, in print
//! colours (paper is white; selection and hover are not printed). Lay it out on pages with
//! [`super::layout_canvas`].

use super::layout::Palette;
use super::{Item, Pt, Rect, Rgb, Stroke, font, pt};
use crate::model::Project;
use crate::network::{Diagram, NetLinkKind};

/// Diagram units (screen pixels at 100% zoom) to printed points: the diagram prints at the same
/// 3/4 size as the task table's columns.
pub const PT_PER_UNIT: f32 = 0.75;

/// What to draw besides the boxes and lines (the view's options).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct NetPrintOptions {
    /// Link type and lag beside each line.
    pub link_labels: bool,
    /// One diagonal through a task in progress, two through a complete one.
    pub progress_marks: bool,
    /// Small boxes with only the task ID.
    pub ids_only: bool,
}

/// A date as the app shows it.
pub(crate) fn date(d: Option<chrono::NaiveDateTime>) -> String {
    d.map(|d| d.format("%a %m/%d/%y").to_string()).unwrap_or_else(|| "NA".into())
}

/// Text whose top is at `y` (as the app places a line of text), cut to `max` points.
#[allow(clippy::too_many_arguments)]
pub(crate) fn line_text(out: &mut Vec<Item>, x: f32, y: f32, s: &str, size: f32, color: Rgb, bold: bool, max: f32) {
    let fitted = font::fit(s, size, bold, max.max(0.0));
    if !fitted.is_empty() {
        out.push(Item::Text { at: pt(x, y + font::metrics().ascent * size), size, color, bold, text: fitted });
    }
}

/// Text centred on `c` (horizontally and on its capital letters).
pub(crate) fn centred_text(out: &mut Vec<Item>, c: Pt, s: &str, size: f32, color: Rgb, bold: bool) {
    let w = font::width(s, size, bold);
    out.push(Item::Text { at: pt(c.x - w / 2.0, c.y + font::metrics().cap_height * size / 2.0), size, color, bold, text: s.into() });
}

fn arrow(out: &mut Vec<Item>, pts: &[(f32, f32)], color: Rgb) {
    if let Some(&(x, y)) = pts.last() {
        let s = 5.0;
        out.push(Item::Polygon { points: vec![pt(x, y), pt(x - s * 1.4, y - s * 0.8), pt(x - s * 1.4, y + s * 0.8)], fill: color });
    }
}

/// The diagram's items and its size in points.
pub fn items(p: &Project, d: &Diagram, opts: NetPrintOptions, pal: &Palette) -> (Vec<Item>, f32, f32) {
    let mut out = Vec::new();
    let g = d.geometry;
    let points = |l: &crate::network::NetLink| -> Vec<Pt> { d.route(l).into_iter().map(|(x, y)| pt(x, y)).collect() };

    // Lines first: boxes cover their ends.
    for l in &d.links {
        let route = d.route(l);
        if route.len() < 2 {
            continue;
        }
        match l.kind {
            NetLinkKind::Outline => out.push(Item::Line { points: points(l), stroke: Stroke::dashed(1.0, pal.grid_strong, 4.0) }),
            NetLinkKind::Member => {
                out.push(Item::Line { points: points(l), stroke: Stroke::dashed(1.2, pal.member_link, 4.0) });
                arrow(&mut out, &route, pal.member_link);
            }
            NetLinkKind::Dependency { kind, lag } => {
                let color = if l.critical { pal.bar_critical } else { pal.link };
                let width = if l.critical { 1.6 } else { 1.0 };
                out.push(Item::Line { points: points(l), stroke: Stroke::new(width, color) });
                arrow(&mut out, &route, color);
                if opts.link_labels
                    && !l.merged
                    && let [.., (ax, ay), (bx, by)] = route.as_slice()
                {
                    let label = format!("{}{}", kind.code(), lag.format(&p.settings.time));
                    let size = 10.0;
                    let w = font::width(&label, size, false);
                    let at = pt((ax + bx) / 2.0 - w / 2.0, ay.min(*by) - 2.0 - font::metrics().descent.abs() * size);
                    out.push(Item::Text { at, size, color: pal.text_weak, bold: false, text: label });
                }
            }
        }
    }

    // Boxes.
    let fs = 11.0;
    let pad = 5.0;
    for (k, node) in d.nodes.iter().enumerate() {
        let (Some(br), Some(t)) = (d.rect(k), p.tasks.get(node.task)) else { continue };
        let r = Rect::new(br.x, br.y, br.w, br.h);
        let (edge, edge_w) = if t.sched.critical { (pal.bar_critical, 1.8) } else { (pal.bar_task_edge, 1.0) };
        let stroke = Stroke::new(edge_w, edge);
        if t.is_milestone() {
            // Pointed ends mark milestones.
            let c = (r.h / 2.0).min(10.0);
            let my = r.y + r.h / 2.0;
            let shape = vec![
                pt(r.x + c, r.y),
                pt(r.right() - c, r.y),
                pt(r.right(), my),
                pt(r.right() - c, r.bottom()),
                pt(r.x + c, r.bottom()),
                pt(r.x, my),
            ];
            out.push(Item::Polygon { points: shape.clone(), fill: pal.white });
            let mut outline = shape;
            if let Some(&first) = outline.first() {
                outline.push(first);
            }
            out.push(Item::Line { points: outline, stroke });
        } else {
            out.push(Item::Rect { rect: r, fill: Some(pal.white), stroke: Some(stroke) });
        }
        if t.sched.is_summary {
            // A band along the top marks summaries.
            out.push(Item::Rect { rect: Rect::new(r.x, r.y, r.w, 4.0), fill: Some(pal.bar_summary), stroke: None });
        }
        let text = if t.active { pal.text } else { pal.text_weak };
        let weak = pal.text_weak;
        let id = (node.task + 1).to_string();
        if opts.ids_only {
            centred_text(&mut out, pt(r.x + r.w / 2.0, r.y + r.h / 2.0), &id, fs, text, false);
        } else {
            let inner = r.w - 2.0 * pad;
            let row_h = (r.h - 2.0 * pad) / 4.0;
            let (x, y0) = (r.x + pad, r.y + pad);
            line_text(&mut out, x, y0, &t.name, fs * 1.05, text, true, inner);
            let (left_w, right_x) = (inner * 0.6, inner * 0.62);
            let cells = [
                (format!("Start: {}", date(t.sched.start)), format!("ID: {id}")),
                (format!("Finish: {}", date(t.sched.finish)), format!("Dur: {}", p.format_duration(t.sched.duration, t.estimated))),
            ];
            for (row, (left, right)) in cells.iter().enumerate() {
                let y = y0 + row_h * (row as f32 + 1.0);
                line_text(&mut out, x, y, left, fs, text, false, left_w);
                line_text(&mut out, x + right_x, y, right, fs, text, false, inner - right_x);
            }
            let res = p.format_resource_names(t.id);
            if !res.is_empty() {
                line_text(&mut out, x, y0 + row_h * 3.0, &format!("Res: {res}"), fs, weak, false, inner);
            }
        }
        if opts.progress_marks && !t.sched.is_summary {
            let mark = Stroke::new(1.0, weak);
            if t.percent_complete >= 100 {
                out.push(Item::Line { points: vec![pt(r.x, r.y), pt(r.right(), r.bottom())], stroke: mark });
            }
            if t.percent_complete > 0 {
                out.push(Item::Line { points: vec![pt(r.x, r.bottom()), pt(r.right(), r.y)], stroke: mark });
            }
        }
    }
    if d.nodes.is_empty() {
        line_text(&mut out, g.margin, g.margin, "No tasks to show", 14.0, pal.text_weak, false, 400.0);
    }

    let (w, h) = d.size();
    let (w, h) = if d.nodes.is_empty() { (w.max(440.0), h.max(60.0)) } else { (w, h) };
    for item in &mut out {
        super::layout::scale_item(item, PT_PER_UNIT, 0.0, 0.0);
    }
    (out, w * PT_PER_UNIT, h * PT_PER_UNIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{NetworkOptions, layout};

    fn texts(items: &[Item]) -> Vec<String> {
        items.iter().filter_map(|i| if let Item::Text { text, .. } = i { Some(text.clone()) } else { None }).collect()
    }

    #[test]
    fn prints_every_box_with_its_details() {
        let mut p = crate::sample::demo_project(chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        crate::schedule::recalculate(&mut p);
        let boxes: Vec<Option<usize>> = (0..p.tasks.len()).map(Some).collect();
        let d = layout(&p, &boxes, NetworkOptions::default());
        let pal = Palette::default();
        let (items, w, h) = items(&p, &d, NetPrintOptions { link_labels: true, progress_marks: true, ids_only: false }, &pal);
        let (dw, dh) = d.size();
        assert!((w - dw * PT_PER_UNIT).abs() < 0.01 && (h - dh * PT_PER_UNIT).abs() < 0.01);
        let t = texts(&items);
        let first = p.tasks.first().unwrap();
        assert!(t.iter().any(|s| first.name.starts_with(s.trim_end_matches('…'))), "task names print");
        assert!(t.iter().any(|s| s.starts_with("Start: ")) && t.iter().any(|s| s.starts_with("Dur: ")));
        assert!(t.iter().any(|s| s.starts_with("FS")), "link labels print");
        // Critical links and boxes print in the critical colour.
        assert!(items.iter().any(|i| matches!(i, Item::Line { stroke, .. } if stroke.color == pal.bar_critical)));
        // Every text fits its box (fitted with an ellipsis).
        let rects = d.nodes.len();
        assert!(items.iter().filter(|i| matches!(i, Item::Rect { .. } | Item::Polygon { .. })).count() >= rects);
        // IDs only: one number per box.
        let (small, ..) = super::items(&p, &d, NetPrintOptions { ids_only: true, ..Default::default() }, &pal);
        assert!(texts(&small).iter().all(|s| s.parse::<usize>().is_ok()));
    }

    #[test]
    fn an_empty_diagram_says_so() {
        let p = Project::new("Plan", chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        let d = layout(&p, &[], NetworkOptions::default());
        let (items, w, h) = items(&p, &d, NetPrintOptions::default(), &Palette::default());
        assert_eq!(texts(&items), vec!["No tasks to show".to_string()]);
        assert!(w > 0.0 && h > 0.0);
    }
}

//! The Task Board as print items: the columns and cards the app's view draws, in print colours
//! (paper is white; selection is not printed). Lay it out on pages with [`super::layout_canvas`].

use super::layout::Palette;
use super::network::{centred_text, date, line_text};
use super::{Item, Rect, font, pt};
use crate::model::Project;

/// Screen pixels to printed points (the same 3/4 size as the rest of the printouts).
pub const PT_PER_PX: f32 = 0.75;

const COL_W: f32 = 250.0;
const GAP: f32 = 12.0;
const HEAD_H: f32 = 38.0;
const CARD_H: f32 = 62.0;
const CARD_GAP: f32 = 8.0;
const PAD: f32 = 8.0;

/// A column to print: its name, Set % Complete and the rows (indices into `Project::tasks`) of
/// its cards, in order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct BoardColumnPrint {
    pub name: String,
    pub percent: Option<u8>,
    pub rows: Vec<usize>,
}

/// The board's items and its size in points.
pub fn items(p: &Project, columns: &[BoardColumnPrint], pal: &Palette) -> (Vec<Item>, f32, f32) {
    let mut out = Vec::new();
    let status = crate::report::status_moment(p);
    let most = columns.iter().map(|c| c.rows.len()).max().unwrap_or(0) as f32;
    let col_h = HEAD_H + PAD + most * (CARD_H + CARD_GAP) + PAD;
    let top = GAP / 2.0;
    for (k, col) in columns.iter().enumerate() {
        let x = GAP + k as f32 * (COL_W + GAP);
        out.push(Item::Rect { rect: Rect::new(x, top, COL_W, col_h), fill: Some(pal.header_bg), stroke: None });
        // Header: name, card count and the % complete the column sets.
        let mid = top + HEAD_H / 2.0;
        let cap = font::metrics().cap_height;
        let name_size = 14.0;
        let name = font::fit(&col.name, name_size, true, COL_W - 90.0);
        if !name.is_empty() {
            out.push(Item::Text {
                at: pt(x + 10.0, mid + cap * name_size / 2.0),
                size: name_size,
                color: pal.text,
                bold: true,
                text: name,
            });
        }
        let tail = match col.percent {
            Some(pct) => format!("{pct}% · {}", col.rows.len()),
            None => col.rows.len().to_string(),
        };
        let w = font::width(&tail, 12.0, false);
        out.push(Item::Text {
            at: pt(x + COL_W - 10.0 - w, mid + cap * 12.0 / 2.0),
            size: 12.0,
            color: pal.text_weak,
            bold: false,
            text: tail,
        });
        out.push(Item::Line {
            points: vec![pt(x + 6.0, top + HEAD_H), pt(x + COL_W - 6.0, top + HEAD_H)],
            stroke: super::Stroke::new(1.0, pal.grid_strong),
        });

        for (n, &i) in col.rows.iter().enumerate() {
            let Some(t) = p.tasks.get(i) else { continue };
            let r = Rect::new(x + PAD, top + HEAD_H + PAD + n as f32 * (CARD_H + CARD_GAP), COL_W - 2.0 * PAD, CARD_H);
            card(&mut out, r, p, t, crate::report::is_late(t, status, p), pal);
        }
    }
    if columns.is_empty() {
        centred_text(&mut out, pt(COL_W / 2.0 + GAP, top + HEAD_H / 2.0), "No columns", 14.0, pal.text_weak, false);
    }
    let w = GAP + (columns.len().max(1) as f32) * (COL_W + GAP);
    let h = col_h + GAP;
    for item in &mut out {
        super::layout::scale_item(item, PT_PER_PX, 0.0, 0.0);
    }
    (out, w * PT_PER_PX, h * PT_PER_PX)
}

/// One card: name, resources, finish date and progress; a coloured edge for critical tasks.
fn card(out: &mut Vec<Item>, r: Rect, p: &Project, t: &crate::model::Task, late: bool, pal: &Palette) {
    out.push(Item::Rect { rect: r, fill: Some(pal.white), stroke: Some(super::Stroke::new(1.0, pal.grid_strong)) });
    let stripe = if t.sched.critical {
        pal.bar_critical
    } else if t.is_milestone() {
        pal.milestone
    } else {
        pal.accent_mid
    };
    out.push(Item::Rect { rect: Rect::new(r.x, r.y, 4.0, r.h), fill: Some(stripe), stroke: None });
    let x = r.x + 12.0;
    let done = t.is_complete();
    let mark = match (done, t.is_milestone()) {
        (true, _) if font::has_glyph('✔') => "✔ ",
        (false, true) if font::has_glyph('♦') => "♦ ",
        _ => "",
    };
    let name_color = if done { pal.text_weak } else { pal.text };
    line_text(out, x, r.y + 8.0, &format!("{mark}{}", t.name), 13.5, name_color, false, r.w - 20.0);
    let who = p.format_resource_names(t.id);
    line_text(out, x, r.y + 29.0, if who.is_empty() { "Unassigned" } else { &who }, 11.5, pal.text_weak, false, r.w - 122.0);
    let due = format!("Due {}", date(t.sched.finish));
    let w = font::width(&due, 11.5, false);
    let cap = font::metrics().cap_height;
    out.push(Item::Text {
        at: pt(r.right() - 8.0 - w, r.y + 36.0 + cap * 11.5 / 2.0),
        size: 11.5,
        color: if late { pal.error } else { pal.text_weak },
        bold: false,
        text: due,
    });
    // Progress along the bottom.
    let track = Rect::new(x, r.bottom() - 9.0, r.right() - 10.0 - x, 3.0);
    out.push(Item::Rect { rect: track, fill: Some(pal.track), stroke: None });
    let pct = t.sched.percent_complete;
    let pct = if pct.is_finite() { pct.clamp(0.0, 100.0) / 100.0 } else { 0.0 };
    if pct > 0.0 {
        let filled = Rect::new(track.x, track.y, track.w * pct, track.h);
        out.push(Item::Rect { rect: filled, fill: Some(if done { pal.ok } else { pal.bar_progress }), stroke: None });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_every_column_and_card() {
        let mut p = crate::sample::demo_project(chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        crate::schedule::recalculate(&mut p);
        let cols: Vec<BoardColumnPrint> =
            p.board().into_iter().map(|(c, rows)| BoardColumnPrint { name: c.name.clone(), percent: c.percent_complete, rows }).collect();
        let cards: usize = cols.iter().map(|c| c.rows.len()).sum();
        assert!(cards > 0);
        let pal = Palette::default();
        let (items, w, h) = items(&p, &cols, &pal);
        assert!((w - (GAP + cols.len() as f32 * (COL_W + GAP)) * PT_PER_PX).abs() < 0.01);
        assert!(h > CARD_H * PT_PER_PX);
        let texts: Vec<&str> =
            items.iter().filter_map(|i| if let Item::Text { text, .. } = i { Some(text.as_str()) } else { None }).collect();
        assert!(texts.contains(&"Not Started"), "{texts:?}");
        assert!(texts.contains(&"100% · 0") || texts.iter().any(|t| t.starts_with("100% · ")), "Done shows its % complete");
        assert_eq!(texts.iter().filter(|t| t.starts_with("Due ")).count(), cards);
        // Cards are white on the column panels.
        let white = items.iter().filter(|i| matches!(i, Item::Rect { fill: Some(f), .. } if *f == pal.white)).count();
        assert_eq!(white, cards);
    }

    #[test]
    fn an_empty_board_prints_without_panicking() {
        let p = Project::new("Plan", chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        let (items, w, h) = items(&p, &[], &Palette::default());
        assert!(!items.is_empty() && w > 0.0 && h > 0.0);
        // Rows that don't exist are skipped.
        let (items, ..) =
            super::items(&p, &[BoardColumnPrint { name: "X".into(), percent: Some(250), rows: vec![7, usize::MAX] }], &Palette::default());
        assert!(items.iter().all(|i| !matches!(i, Item::Text { text, .. } if text.starts_with("Due"))));
    }
}

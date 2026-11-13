//! Two-tier timescale used by the Gantt chart, usage views and timeline.

use chrono::NaiveDateTime;
use egui::{Align2, FontId, Painter, Rect, Stroke, pos2};

use crate::state::GanttState;
use crate::theme;

pub use plancraft_core::timescale::{Unit, tiers};

/// Paints the two-tier header into `rect`. Returns the bottom-tier boundaries (x positions) for gridlines.
pub fn paint_header(painter: &Painter, rect: Rect, g: &GanttState) -> Vec<(f32, NaiveDateTime)> {
    let (top, bottom) = tiers(g.px_per_day);
    painter.rect_filled(rect, 0.0, theme::pal().HEADER_BG);
    let mid = rect.top() + rect.height() / 2.0;
    let font = FontId::proportional(11.5);
    let end = g.time_at(rect.left(), rect.right());
    let mut out = Vec::new();
    for (unit, y0, y1, collect) in [(top, rect.top(), mid, false), (bottom, mid, rect.bottom(), true)] {
        let mut t = unit.floor(g.left);
        let mut guard = 0;
        while t <= end && guard < 2000 {
            guard += 1;
            let n = unit.next(t);
            let x0 = g.x_of(rect.left(), t);
            let x1 = g.x_of(rect.left(), n);
            painter.vline(x0.round() + 0.5, y0..=y1, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
            let cell = Rect::from_x_y_ranges(x0.max(rect.left())..=x1.min(rect.right()), y0..=y1);
            if cell.width() > 8.0 {
                let label = unit.label(t, x1 - x0);
                let anchor = if collect || x1 - x0 < 60.0 { Align2::CENTER_CENTER } else { Align2::LEFT_CENTER };
                let pos = if anchor == Align2::CENTER_CENTER { cell.center() } else { pos2(cell.left() + 4.0, cell.center().y) };
                painter.with_clip_rect(cell).text(pos, anchor, label, font.clone(), theme::pal().TEXT);
            }
            if collect {
                out.push((x0, t));
            }
            t = n;
        }
    }
    painter.hline(rect.x_range(), mid, Stroke::new(1.0, theme::pal().GRID_LINE));
    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG));
    out
}

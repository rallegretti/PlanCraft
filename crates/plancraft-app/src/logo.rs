//! The PlanCraft mark: a deep-blue tile with a cascade of schedule bars
//! ending in a gold milestone diamond.
//!
//! The mark is defined once as simple geometry in a unit square and is both
//! painted with egui (in the UI) and rasterized in software (window icon).

use egui::{Color32, Painter, Rect, Shape, Stroke, pos2, vec2};

use crate::theme;

/// Bars as (x0, y0, x1, y1) in unit coordinates.
const BARS: [(f32, f32, f32, f32); 3] = [(0.17, 0.23, 0.53, 0.34), (0.31, 0.445, 0.71, 0.555), (0.45, 0.66, 0.64, 0.77)];
const DIAMOND: (f32, f32, f32) = (0.785, 0.715, 0.105);
const TILE_RADIUS: f32 = 0.22;
const BAR_RADIUS: f32 = 0.045;

/// Paints the mark into `rect` (should be square).
pub fn paint(painter: &Painter, rect: Rect) {
    let s = rect.width().min(rect.height());
    let o = rect.min;
    let p = |x: f32, y: f32| pos2(o.x + x * s, o.y + y * s);
    painter.rect_filled(Rect::from_min_size(o, vec2(s, s)), s * TILE_RADIUS, theme::pal().ACCENT);
    // Subtle highlight in the upper half.
    painter.rect_filled(
        Rect::from_min_max(p(0.0, 0.0), p(1.0, 0.5)),
        egui::CornerRadius { nw: (s * TILE_RADIUS) as u8, ne: (s * TILE_RADIUS) as u8, sw: 0, se: 0 },
        Color32::from_white_alpha(14),
    );
    let link = Stroke::new((s * 0.025).max(1.0), Color32::from_rgba_unmultiplied(255, 255, 255, 170));
    // Dependency elbows.
    painter.line_segment([p(0.53, 0.285), p(0.58, 0.285)], link);
    painter.line_segment([p(0.58, 0.285), p(0.58, 0.445)], link);
    painter.line_segment([p(0.71, 0.5), p(0.75, 0.5)], link);
    for (x0, y0, x1, y1) in BARS {
        painter.rect_filled(Rect::from_min_max(p(x0, y0), p(x1, y1)), s * BAR_RADIUS, Color32::WHITE);
    }
    let (cx, cy, r) = DIAMOND;
    painter.add(Shape::convex_polygon(vec![p(cx, cy - r), p(cx + r, cy), p(cx, cy + r), p(cx - r, cy)], theme::pal().GOLD, Stroke::NONE));
}

fn rounded_rect_contains(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// Rasterizes the mark to straight-alpha RGBA pixels (`size × size`).
pub fn rasterize(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = vec![0u8; (size * size * 4) as usize];
    let accent = theme::pal().ACCENT;
    let gold = theme::pal().GOLD;
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if !rounded_rect_contains(x, y, 0.0, 0.0, 1.0, 1.0, TILE_RADIUS) {
                        continue;
                    }
                    let (cx, cy, dr) = DIAMOND;
                    let color = if (x - cx).abs() + (y - cy).abs() <= dr {
                        gold
                    } else if BARS.iter().any(|&(x0, y0, x1, y1)| rounded_rect_contains(x, y, x0, y0, x1, y1, BAR_RADIUS)) {
                        Color32::WHITE
                    } else {
                        accent
                    };
                    r += color.r() as f32;
                    g += color.g() as f32;
                    b += color.b() as f32;
                    a += 1.0;
                }
            }
            if a > 0.0 {
                let i = ((py * size + px) * 4) as usize;
                out[i] = (r / a) as u8;
                out[i + 1] = (g / a) as u8;
                out[i + 2] = (b / a) as u8;
                out[i + 3] = (a / (SS * SS) as f32 * 255.0) as u8;
            }
        }
    }
    out
}

/// Window icon data for eframe.
pub fn icon_data() -> egui::IconData {
    let size = 256;
    egui::IconData { rgba: rasterize(size), width: size, height: size }
}

/// Paints the mark on a dark/accent background, with a light outline so the tile stays visible.
pub fn paint_on_dark(painter: &Painter, rect: Rect) {
    let s = rect.width().min(rect.height());
    painter.rect_filled(rect.expand(1.5), (s + 3.0) * TILE_RADIUS, Color32::from_white_alpha(235));
    paint(painter, rect);
}

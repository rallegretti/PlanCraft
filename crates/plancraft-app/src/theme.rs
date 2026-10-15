//! Colors and egui style for PlanCraft's "deep blue" look.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

/// Primary accent: deep blue.
pub const ACCENT: Color32 = Color32::from_rgb(0x1B, 0x3A, 0x8C);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(0x12, 0x28, 0x66);
pub const ACCENT_MID: Color32 = Color32::from_rgb(0x2F, 0x5B, 0xC4);
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(0xDC, 0xE6, 0xF8);
pub const ACCENT_PALE: Color32 = Color32::from_rgb(0xEE, 0xF3, 0xFC);
pub const GOLD: Color32 = Color32::from_rgb(0xF2, 0xB1, 0x1C);

pub const TEXT: Color32 = Color32::from_rgb(0x26, 0x26, 0x2B);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x6B, 0x70, 0x7A);
pub const BG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const PANEL_BG: Color32 = Color32::from_rgb(0xF6, 0xF7, 0xF9);
pub const HEADER_BG: Color32 = Color32::from_rgb(0xF0, 0xF1, 0xF4);
pub const GRID_LINE: Color32 = Color32::from_rgb(0xDD, 0xDF, 0xE4);
pub const GRID_LINE_STRONG: Color32 = Color32::from_rgb(0xC4, 0xC8, 0xD0);
pub const SELECTION: Color32 = Color32::from_rgb(0xD5, 0xE2, 0xFA);
pub const SELECTION_CELL: Color32 = Color32::from_rgb(0xB9, 0xCE, 0xF5);
pub const NONWORKING: Color32 = Color32::from_rgb(0xEE, 0xF0, 0xF4);

// Gantt bars.
pub const BAR_TASK: Color32 = Color32::from_rgb(0x5A, 0x86, 0xD6);
pub const BAR_TASK_EDGE: Color32 = Color32::from_rgb(0x2F, 0x5B, 0xC4);
pub const BAR_PROGRESS: Color32 = Color32::from_rgb(0x15, 0x2E, 0x73);
pub const BAR_CRITICAL: Color32 = Color32::from_rgb(0xD9, 0x4F, 0x4F);
pub const BAR_CRITICAL_EDGE: Color32 = Color32::from_rgb(0xA8, 0x2E, 0x2E);
pub const BAR_SUMMARY: Color32 = Color32::from_rgb(0x33, 0x38, 0x45);
pub const BAR_MANUAL: Color32 = Color32::from_rgb(0x9B, 0xBC, 0xEB);
pub const BAR_BASELINE: Color32 = Color32::from_rgb(0xA9, 0xAE, 0xB8);
pub const BAR_SLACK: Color32 = Color32::from_rgb(0x2C, 0x9C, 0x8F);
pub const MILESTONE: Color32 = Color32::from_rgb(0x1B, 0x3A, 0x8C);
pub const LINK: Color32 = Color32::from_rgb(0x3C, 0x4A, 0x66);
pub const TODAY: Color32 = Color32::from_rgb(0xE0, 0x7B, 0x00);
pub const STATUS_DATE: Color32 = Color32::from_rgb(0x8E, 0x44, 0xAD);
pub const DEADLINE: Color32 = Color32::from_rgb(0x2E, 0x8B, 0x57);

pub const OK: Color32 = Color32::from_rgb(0x2E, 0x8B, 0x57);
pub const WARN: Color32 = Color32::from_rgb(0xE0, 0x7B, 0x00);
pub const ERROR: Color32 = Color32::from_rgb(0xC6, 0x28, 0x28);

pub const ROW_HEIGHT: f32 = 22.0;
pub const HEADER_HEIGHT: f32 = 40.0;

/// Heat-map color for a utilization ratio (0 = idle, 1 = fully booked, >1 = overallocated).
pub fn utilization_color(u: f64) -> Color32 {
    if u <= 0.0 {
        return BG;
    }
    if u > 1.0001 {
        let t = ((u - 1.0) / 1.0).clamp(0.0, 1.0) as f32;
        return lerp(Color32::from_rgb(0xF6, 0xC6, 0xC6), Color32::from_rgb(0xE0, 0x6A, 0x6A), t);
    }
    lerp(Color32::from_rgb(0xEC, 0xF2, 0xFC), Color32::from_rgb(0x8F, 0xB0, 0xE8), u as f32)
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Installs PlanCraft's light theme.
pub fn install(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.interact_size.y = 22.0;

        let mut v = Visuals::light();
        v.panel_fill = BG;
        v.window_fill = BG;
        v.faint_bg_color = PANEL_BG;
        v.extreme_bg_color = BG;
        v.hyperlink_color = ACCENT_MID;
        v.selection.bg_fill = ACCENT_LIGHT;
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.window_corner_radius = CornerRadius::same(6);
        v.menu_corner_radius = CornerRadius::same(4);
        v.window_stroke = Stroke::new(1.0, GRID_LINE_STRONG);
        v.override_text_color = None;
        let r = CornerRadius::same(3);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, GRID_LINE);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0xF3, 0xF4, 0xF7);
        v.widgets.inactive.bg_fill = Color32::from_rgb(0xF3, 0xF4, 0xF7);
        v.widgets.inactive.corner_radius = r;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, GRID_LINE_STRONG);
        v.text_edit_bg_color = Some(BG);
        v.widgets.hovered.weak_bg_fill = ACCENT_PALE;
        v.widgets.hovered.bg_fill = ACCENT_PALE;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_MID);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, ACCENT_DARK);
        v.widgets.hovered.corner_radius = r;
        v.widgets.active.weak_bg_fill = ACCENT_LIGHT;
        v.widgets.active.bg_fill = ACCENT_LIGHT;
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT_DARK);
        v.widgets.active.corner_radius = r;
        v.widgets.open.weak_bg_fill = ACCENT_LIGHT;
        v.widgets.open.corner_radius = r;
        style.visuals = v;
    });
}

//! Colors and egui styles for PlanCraft's light and dark themes.
//!
//! All drawing code reads colors from [`pal()`], the palette of the theme that
//! is currently shown. The deep-blue brand accent is shared by both themes; the
//! dark palette uses lighter tints of it where it is drawn on dark surfaces.

use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, ThemePreference, Visuals};
use serde::{Deserialize, Serialize};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Every color the UI paints with. Field names follow the old constant names.
#[allow(non_snake_case)]
pub struct Palette {
    // Brand
    pub ACCENT: Color32,
    pub ACCENT_DARK: Color32,
    pub ACCENT_MID: Color32,
    pub ACCENT_LIGHT: Color32,
    pub ACCENT_PALE: Color32,
    pub TITLE_BAR: Color32,
    pub GOLD: Color32,
    // Surfaces & text
    pub TEXT: Color32,
    pub TEXT_WEAK: Color32,
    pub BG: Color32,
    pub PANEL_BG: Color32,
    pub HEADER_BG: Color32,
    pub INPUT_BG: Color32,
    pub TAB_HOVER: Color32,
    pub TRACK: Color32,
    pub SCROLL_THUMB: Color32,
    pub GRID_LINE: Color32,
    pub GRID_LINE_STRONG: Color32,
    pub SELECTION: Color32,
    pub SELECTION_CELL: Color32,
    pub SELECTION_TINT: Color32,
    pub NONWORKING: Color32,
    // Charts
    pub CHART_GRID: Color32,
    pub PROJECT_START: Color32,
    pub BAR_TASK: Color32,
    pub BAR_TASK_EDGE: Color32,
    pub BAR_PROGRESS: Color32,
    pub BAR_CRITICAL: Color32,
    pub BAR_CRITICAL_EDGE: Color32,
    pub BAR_SUMMARY: Color32,
    pub BAR_MANUAL: Color32,
    pub BAR_BASELINE: Color32,
    pub BAR_SLACK: Color32,
    pub BAR_INACTIVE: Color32,
    pub BAR_COMPLETE: Color32,
    pub MILESTONE: Color32,
    pub MEMBER_LINK: Color32,
    pub LINK: Color32,
    pub TODAY: Color32,
    pub STATUS_DATE: Color32,
    pub DEADLINE: Color32,
    pub TIMELINE_BAND: Color32,
    pub TIMELINE_TICK: Color32,
    pub TIMELINE_WINDOW: Color32,
    pub HEAT_LOW: Color32,
    pub HEAT_HIGH: Color32,
    pub OVER_LOW: Color32,
    pub OVER_HIGH: Color32,
    pub EXCEPTION_WORKING: Color32,
    pub EXCEPTION_OFF: Color32,
    // Status
    pub OK: Color32,
    pub WARN: Color32,
    pub ERROR: Color32,
}

pub const LIGHT: Palette = Palette {
    ACCENT: rgb(0x1B, 0x3A, 0x8C),
    ACCENT_DARK: rgb(0x12, 0x28, 0x66),
    ACCENT_MID: rgb(0x2F, 0x5B, 0xC4),
    ACCENT_LIGHT: rgb(0xDC, 0xE6, 0xF8),
    ACCENT_PALE: rgb(0xEE, 0xF3, 0xFC),
    TITLE_BAR: rgb(0x1B, 0x3A, 0x8C),
    GOLD: rgb(0xF2, 0xB1, 0x1C),
    TEXT: rgb(0x26, 0x26, 0x2B),
    TEXT_WEAK: rgb(0x58, 0x5D, 0x67),
    BG: rgb(0xFF, 0xFF, 0xFF),
    PANEL_BG: rgb(0xF6, 0xF7, 0xF9),
    HEADER_BG: rgb(0xF0, 0xF1, 0xF4),
    INPUT_BG: rgb(0xF3, 0xF4, 0xF7),
    TAB_HOVER: rgb(0xEB, 0xED, 0xF2),
    TRACK: rgb(0xE6, 0xEA, 0xF1),
    SCROLL_THUMB: rgb(0xD0, 0xD4, 0xDB),
    GRID_LINE: rgb(0xDD, 0xDF, 0xE4),
    GRID_LINE_STRONG: rgb(0xC4, 0xC8, 0xD0),
    SELECTION: rgb(0xD5, 0xE2, 0xFA),
    SELECTION_CELL: rgb(0xB9, 0xCE, 0xF5),
    SELECTION_TINT: Color32::from_rgba_premultiplied(0x5C, 0x62, 0x6C, 110),
    NONWORKING: rgb(0xEE, 0xF0, 0xF4),
    CHART_GRID: rgb(0xEC, 0xEE, 0xF2),
    PROJECT_START: rgb(0xB8, 0xC4, 0xDE),
    BAR_TASK: rgb(0x5A, 0x86, 0xD6),
    BAR_TASK_EDGE: rgb(0x2F, 0x5B, 0xC4),
    BAR_PROGRESS: rgb(0x15, 0x2E, 0x73),
    BAR_CRITICAL: rgb(0xD9, 0x4F, 0x4F),
    BAR_CRITICAL_EDGE: rgb(0xA8, 0x2E, 0x2E),
    BAR_SUMMARY: rgb(0x33, 0x38, 0x45),
    BAR_MANUAL: rgb(0x9B, 0xBC, 0xEB),
    BAR_BASELINE: rgb(0xA9, 0xAE, 0xB8),
    BAR_SLACK: rgb(0x2C, 0x9C, 0x8F),
    BAR_INACTIVE: rgb(0xE6, 0xE8, 0xEC),
    BAR_COMPLETE: rgb(0x8F, 0xA3, 0xC8),
    MILESTONE: rgb(0x1B, 0x3A, 0x8C),
    MEMBER_LINK: rgb(0xC8, 0x96, 0x1E),
    LINK: rgb(0x3C, 0x4A, 0x66),
    TODAY: rgb(0xE0, 0x7B, 0x00),
    STATUS_DATE: rgb(0x8E, 0x44, 0xAD),
    DEADLINE: rgb(0x2E, 0x8B, 0x57),
    TIMELINE_BAND: rgb(0xE3, 0xE9, 0xF5),
    TIMELINE_TICK: rgb(0xC9, 0xD3, 0xE6),
    TIMELINE_WINDOW: Color32::from_rgba_premultiplied(0x03, 0x06, 0x0F, 28),
    HEAT_LOW: rgb(0xEC, 0xF2, 0xFC),
    HEAT_HIGH: rgb(0x8F, 0xB0, 0xE8),
    OVER_LOW: rgb(0xF6, 0xC6, 0xC6),
    OVER_HIGH: rgb(0xE0, 0x6A, 0x6A),
    EXCEPTION_WORKING: rgb(0xC9, 0xE8, 0xD8),
    EXCEPTION_OFF: rgb(0xF4, 0xC7, 0xC7),
    OK: rgb(0x2E, 0x8B, 0x57),
    WARN: rgb(0xE0, 0x7B, 0x00),
    ERROR: rgb(0xC6, 0x28, 0x28),
};

pub const DARK: Palette = Palette {
    ACCENT: rgb(0x3F, 0x66, 0xC8),
    ACCENT_DARK: rgb(0xA9, 0xC1, 0xF5),
    ACCENT_MID: rgb(0x7C, 0xA0, 0xEC),
    ACCENT_LIGHT: rgb(0x2B, 0x3C, 0x66),
    ACCENT_PALE: rgb(0x24, 0x2F, 0x4A),
    TITLE_BAR: rgb(0x17, 0x30, 0x73),
    GOLD: rgb(0xF2, 0xB1, 0x1C),
    TEXT: rgb(0xE4, 0xE7, 0xEE),
    TEXT_WEAK: rgb(0x9A, 0xA1, 0xAE),
    BG: rgb(0x1E, 0x20, 0x27),
    PANEL_BG: rgb(0x17, 0x19, 0x1F),
    HEADER_BG: rgb(0x26, 0x29, 0x32),
    INPUT_BG: rgb(0x2A, 0x2D, 0x37),
    TAB_HOVER: rgb(0x22, 0x25, 0x2E),
    TRACK: rgb(0x33, 0x37, 0x42),
    SCROLL_THUMB: rgb(0x45, 0x4A, 0x57),
    GRID_LINE: rgb(0x2E, 0x32, 0x3C),
    GRID_LINE_STRONG: rgb(0x40, 0x45, 0x52),
    SELECTION: rgb(0x25, 0x35, 0x5A),
    SELECTION_CELL: rgb(0x2F, 0x46, 0x78),
    SELECTION_TINT: Color32::from_rgba_premultiplied(0x10, 0x19, 0x2C, 110),
    NONWORKING: rgb(0x24, 0x27, 0x2F),
    CHART_GRID: rgb(0x29, 0x2C, 0x35),
    PROJECT_START: rgb(0x4A, 0x58, 0x78),
    BAR_TASK: rgb(0x4F, 0x7C, 0xD0),
    BAR_TASK_EDGE: rgb(0x86, 0xA8, 0xEC),
    BAR_PROGRESS: rgb(0x10, 0x22, 0x55),
    BAR_CRITICAL: rgb(0xD9, 0x55, 0x55),
    BAR_CRITICAL_EDGE: rgb(0xF0, 0x8A, 0x8A),
    BAR_SUMMARY: rgb(0xC9, 0xCE, 0xDA),
    BAR_MANUAL: rgb(0x6F, 0x92, 0xD0),
    BAR_BASELINE: rgb(0x6B, 0x71, 0x80),
    BAR_SLACK: rgb(0x3F, 0xB8, 0xA8),
    BAR_INACTIVE: rgb(0x33, 0x37, 0x42),
    BAR_COMPLETE: rgb(0x5A, 0x6B, 0x8E),
    MILESTONE: rgb(0x86, 0xA8, 0xEC),
    MEMBER_LINK: rgb(0xD9, 0xA8, 0x3A),
    LINK: rgb(0x8C, 0x96, 0xAC),
    TODAY: rgb(0xF0, 0x93, 0x1E),
    STATUS_DATE: rgb(0xB3, 0x7F, 0xD6),
    DEADLINE: rgb(0x4C, 0xC3, 0x8A),
    TIMELINE_BAND: rgb(0x26, 0x2F, 0x45),
    TIMELINE_TICK: rgb(0x3A, 0x46, 0x62),
    TIMELINE_WINDOW: Color32::from_rgba_premultiplied(0x0D, 0x16, 0x2F, 40),
    HEAT_LOW: rgb(0x24, 0x2E, 0x45),
    HEAT_HIGH: rgb(0x3B, 0x5F, 0xAE),
    OVER_LOW: rgb(0x5A, 0x2C, 0x2E),
    OVER_HIGH: rgb(0xA3, 0x3F, 0x42),
    EXCEPTION_WORKING: rgb(0x24, 0x4A, 0x3A),
    EXCEPTION_OFF: rgb(0x5A, 0x2C, 0x2E),
    OK: rgb(0x4C, 0xC3, 0x8A),
    WARN: rgb(0xF0, 0x93, 0x1E),
    ERROR: rgb(0xEF, 0x6B, 0x6B),
};

static DARK_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The palette of the theme currently on screen.
pub fn pal() -> &'static Palette {
    if DARK_ACTIVE.load(Ordering::Relaxed) { &DARK } else { &LIGHT }
}

/// The user's theme choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Follow the operating system.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "System",
            ThemeChoice::Light => "Light",
            ThemeChoice::Dark => "Dark",
        }
    }

    pub fn parse(s: &str) -> Option<ThemeChoice> {
        ThemeChoice::ALL.into_iter().find(|t| t.label().eq_ignore_ascii_case(s.trim()))
    }

    fn preference(self) -> ThemePreference {
        match self {
            ThemeChoice::System => ThemePreference::System,
            ThemeChoice::Light => ThemePreference::Light,
            ThemeChoice::Dark => ThemePreference::Dark,
        }
    }
}

pub const ROW_HEIGHT: f32 = 22.0;
pub const HEADER_HEIGHT: f32 = 40.0;

/// Heat-map color for a utilization ratio (0 = idle, 1 = fully booked, >1 = overallocated).
pub fn utilization_color(u: f64) -> Color32 {
    let p = pal();
    if u <= 0.0 {
        return p.BG;
    }
    if u > 1.0001 {
        let t = ((u - 1.0) / 1.0).clamp(0.0, 1.0) as f32;
        return lerp(p.OVER_LOW, p.OVER_HIGH, t);
    }
    lerp(p.HEAT_LOW, p.HEAT_HIGH, u as f32)
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn visuals(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.BG;
    v.window_fill = p.BG;
    v.faint_bg_color = p.PANEL_BG;
    v.extreme_bg_color = p.BG;
    v.code_bg_color = p.HEADER_BG;
    v.hyperlink_color = p.ACCENT_MID;
    v.selection.bg_fill = if dark { p.SELECTION_CELL } else { p.ACCENT_LIGHT };
    v.selection.stroke = Stroke::new(1.0, if dark { p.TEXT } else { p.ACCENT });
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(4);
    v.window_stroke = Stroke::new(1.0, p.GRID_LINE_STRONG);
    v.override_text_color = None;
    v.warn_fg_color = p.WARN;
    v.error_fg_color = p.ERROR;
    let r = CornerRadius::same(3);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.GRID_LINE);
    v.widgets.noninteractive.bg_fill = p.BG;
    v.widgets.noninteractive.weak_bg_fill = p.BG;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.TEXT);
    v.widgets.inactive.weak_bg_fill = p.INPUT_BG;
    v.widgets.inactive.bg_fill = p.INPUT_BG;
    v.widgets.inactive.corner_radius = r;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.GRID_LINE_STRONG);
    v.text_edit_bg_color = Some(if dark { p.INPUT_BG } else { p.BG });
    v.widgets.hovered.weak_bg_fill = p.ACCENT_PALE;
    v.widgets.hovered.bg_fill = p.ACCENT_PALE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.ACCENT_MID);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.ACCENT_DARK);
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.weak_bg_fill = p.ACCENT_LIGHT;
    v.widgets.active.bg_fill = p.ACCENT_LIGHT;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.ACCENT_DARK);
    v.widgets.active.corner_radius = r;
    v.widgets.open.weak_bg_fill = p.ACCENT_LIGHT;
    v.widgets.open.bg_fill = p.ACCENT_LIGHT;
    v.widgets.open.fg_stroke = Stroke::new(1.0, p.TEXT);
    v.widgets.open.corner_radius = r;
    v
}

/// Installs PlanCraft's styles for both themes and applies the user's choice.
pub fn install(ctx: &egui::Context, choice: ThemeChoice) {
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
    });
    ctx.style_mut_of(Theme::Light, |s| s.visuals = visuals(&LIGHT, false));
    ctx.style_mut_of(Theme::Dark, |s| s.visuals = visuals(&DARK, true));
    set_choice(ctx, choice);
}

/// Switches between system, light and dark.
pub fn set_choice(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(choice.preference());
    sync(ctx);
}

/// Points [`pal()`] at the theme egui is currently showing (call once per frame,
/// so following the system theme updates live).
pub fn sync(ctx: &egui::Context) {
    DARK_ACTIVE.store(ctx.theme() == Theme::Dark, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::{DARK, LIGHT, Palette};
    use egui::Color32;

    /// WCAG relative luminance.
    fn luminance(c: Color32) -> f64 {
        let ch = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn both_themes_keep_text_readable() {
        for (name, p) in [("light", &LIGHT), ("dark", &DARK)] {
            let p: &Palette = p;
            for (what, bg) in [("BG", p.BG), ("PANEL_BG", p.PANEL_BG), ("HEADER_BG", p.HEADER_BG), ("SELECTION", p.SELECTION)] {
                assert!(contrast(p.TEXT, bg) >= 7.0, "{name}: TEXT on {what} is {:.1}:1", contrast(p.TEXT, bg));
                assert!(contrast(p.TEXT_WEAK, bg) >= 4.5, "{name}: TEXT_WEAK on {what} is {:.1}:1", contrast(p.TEXT_WEAK, bg));
            }
            assert!(contrast(Color32::WHITE, p.TITLE_BAR) >= 7.0, "{name}: title bar text");
            assert!(contrast(Color32::WHITE, p.ACCENT) >= 4.5, "{name}: text on accent buttons");
            assert!(contrast(p.ERROR, p.BG) >= 4.5, "{name}: error text");
        }
    }
    /// Glyphs the bundled egui fonts cannot render (they show as empty boxes).
    /// Found by rendering a glyph sheet; extend when a new icon turns out to be missing.
    const MISSING_GLYPHS: &str = "→↶↷⊘▤▸◆●⮜🗂🗒🤖ⓘ⊕⊖⇄✎⬚▦◯🗓✓✗✘⬤▢◈◇❖✜🧭🧮🗃🧑⏩▬";

    fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                sources(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    #[test]
    fn ui_text_avoids_glyphs_missing_from_bundled_fonts() {
        let mut files = Vec::new();
        sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut problems = Vec::new();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            for (n, line) in text.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") || code.starts_with("const MISSING_GLYPHS") {
                    continue;
                }
                for c in line.chars().filter(|c| MISSING_GLYPHS.contains(*c)) {
                    problems.push(format!("{}:{}: '{c}'", f.display(), n + 1));
                }
            }
        }
        assert!(problems.is_empty(), "glyphs that render as boxes:\n{}", problems.join("\n"));
    }
}

//! The print font: egui's bundled Ubuntu Light (the same face the app draws with), used to
//! measure text for layout and embedded (subset) in PDFs.

use std::sync::OnceLock;

use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// Font file bytes (Ubuntu Font Licence; embedding in documents is allowed).
pub const DATA: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;

/// PostScript name used in PDFs.
pub const NAME: &str = "Ubuntu-Light";

/// Extra width of synthetic bold (stroke outline), as a fraction of the font size per glyph.
pub const BOLD_EXTRA: f32 = 0.03;

/// Metrics in font units divided by units-per-em (1.0 = the font size).
#[derive(Clone, Copy, Debug)]
pub struct FontMetrics {
    pub units_per_em: f32,
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    pub bbox: [f32; 4],
    pub italic_angle: f32,
}

fn font() -> Option<FontRef<'static>> {
    FontRef::new(DATA).ok()
}

pub fn metrics() -> FontMetrics {
    static M: OnceLock<FontMetrics> = OnceLock::new();
    *M.get_or_init(|| {
        let fallback = FontMetrics {
            units_per_em: 1000.0,
            ascent: 0.93,
            descent: -0.19,
            cap_height: 0.69,
            bbox: [-0.2, -0.2, 1.0, 1.0],
            italic_angle: 0.0,
        };
        let Some(f) = font() else { return fallback };
        let m = f.metrics(Size::unscaled(), LocationRef::default());
        let upm = f32::from(m.units_per_em.max(1));
        let b = m.bounds.map_or(fallback.bbox, |b| [b.x_min / upm, b.y_min / upm, b.x_max / upm, b.y_max / upm]);
        FontMetrics {
            units_per_em: upm,
            ascent: m.ascent / upm,
            descent: m.descent / upm,
            cap_height: m.cap_height.map_or(fallback.cap_height, |c| c / upm),
            bbox: b,
            italic_angle: m.italic_angle,
        }
    })
}

/// Glyph id for a character (0, the "missing glyph" box, when the font has none).
pub fn glyph(ch: char) -> u16 {
    font().and_then(|f| f.charmap().map(ch)).map_or(0, |g| u16::try_from(g.to_u32()).unwrap_or(0))
}

/// Whether the font can draw `ch`.
pub fn has_glyph(ch: char) -> bool {
    ch == ' ' || glyph(ch) != 0
}

/// Advance width of a glyph as a fraction of the font size.
pub fn advance(gid: u16) -> f32 {
    let Some(f) = font() else { return 0.5 };
    let upm = metrics().units_per_em;
    f.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(GlyphId::new(u32::from(gid))).map_or(0.5, |w| w / upm)
}

/// Width of `text` at `size` points.
pub fn width(text: &str, size: f32, bold: bool) -> f32 {
    let Some(f) = font() else { return text.chars().count() as f32 * size * 0.5 };
    let upm = metrics().units_per_em;
    let gm = f.glyph_metrics(Size::unscaled(), LocationRef::default());
    let cmap = f.charmap();
    let mut w = 0.0;
    for ch in text.chars() {
        let gid = cmap.map(ch).unwrap_or(GlyphId::new(0));
        w += gm.advance_width(gid).map_or(0.5, |a| a / upm);
        if bold {
            w += BOLD_EXTRA;
        }
    }
    w * size
}

/// `text` shortened with an ellipsis so it fits in `max` points (empty if not even that fits).
pub fn fit(text: &str, size: f32, bold: bool, max: f32) -> String {
    if width(text, size, bold) <= max {
        return text.to_string();
    }
    let ell = width("…", size, bold);
    let mut out = String::new();
    let mut w = 0.0;
    for ch in text.chars() {
        let cw = width(ch.encode_utf8(&mut [0; 4]), size, bold);
        if w + cw + ell > max {
            break;
        }
        w += cw;
        out.push(ch);
    }
    if out.is_empty() && ell > max {
        return String::new();
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_print_font_measures_and_fits_text() {
        assert!(glyph('A') != 0);
        assert!(has_glyph('…'));
        let w = width("Design review", 10.0, false);
        assert!(w > 30.0 && w < 90.0, "{w}");
        assert!(width("ab", 10.0, true) > width("ab", 10.0, false));
        let f = fit("A very long task name that does not fit", 10.0, false, 60.0);
        assert!(f.ends_with('…') && width(&f, 10.0, false) <= 60.0, "{f}");
        assert_eq!(fit("Short", 10.0, false, 100.0), "Short");
        assert_eq!(fit("Short", 10.0, false, 0.0), "");
        let m = metrics();
        assert!(m.ascent > 0.5 && m.descent < 0.0);
    }
}

//! Print fonts: egui's bundled Ubuntu Light (the same face the app draws with) first, then
//! fallback fonts from the system for characters it lacks (see [`super::fallback`]). Used to
//! measure text for layout and embedded (subset) in PDFs.

use std::sync::OnceLock;

use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// Bundled font file bytes (Ubuntu Font Licence; embedding in documents is allowed).
pub const DATA: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;

/// PostScript name of the bundled font in PDFs.
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

const FALLBACK_METRICS: FontMetrics =
    FontMetrics { units_per_em: 1000.0, ascent: 0.93, descent: -0.19, cap_height: 0.69, bbox: [-0.2, -0.2, 1.0, 1.0], italic_angle: 0.0 };

/// A font face text can print in: face 0 is the bundled font, the others are system fallbacks.
pub struct Face {
    pub data: &'static [u8],
    /// Face index in a font collection (`.ttc`).
    pub index: u32,
    /// Name for PDFs.
    pub name: String,
    /// CFF outlines rather than TrueType.
    pub cff: bool,
    pub metrics: FontMetrics,
}

impl Face {
    fn new(data: &'static [u8], index: u32, name: String, cff: bool) -> Option<Self> {
        let f = FontRef::from_index(data, index).ok()?;
        let m = f.metrics(Size::unscaled(), LocationRef::default());
        let upm = f32::from(m.units_per_em.max(1));
        let fb = FALLBACK_METRICS;
        let b = m.bounds.map_or(fb.bbox, |b| [b.x_min / upm, b.y_min / upm, b.x_max / upm, b.y_max / upm]);
        let metrics = FontMetrics {
            units_per_em: upm,
            ascent: m.ascent / upm,
            descent: m.descent / upm,
            cap_height: m.cap_height.map_or(fb.cap_height, |c| c / upm),
            bbox: b,
            italic_angle: m.italic_angle,
        };
        Some(Self { data, index, name, cff, metrics })
    }

    fn font(&self) -> Option<FontRef<'static>> {
        FontRef::from_index(self.data, self.index).ok()
    }

    /// Advance width of a glyph as a fraction of the font size.
    pub fn advance(&self, gid: u16) -> f32 {
        let Some(f) = self.font() else { return 0.5 };
        let w = f.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(GlyphId::new(u32::from(gid)));
        w.map_or(0.5, |w| w / self.metrics.units_per_em)
    }
}

fn bundled() -> Option<&'static Face> {
    static F: OnceLock<Option<Face>> = OnceLock::new();
    F.get_or_init(|| Face::new(DATA, 0, NAME.into(), false)).as_ref()
}

/// Fallback face `i` (counting from 0), loaded the first time it is needed.
fn fallback_face(i: usize) -> Option<&'static Face> {
    static FACES: OnceLock<Vec<OnceLock<Option<Face>>>> = OnceLock::new();
    let all = super::fallback::fallbacks();
    let slots = FACES.get_or_init(|| all.iter().map(|_| OnceLock::new()).collect());
    let fb = all.get(i)?;
    slots.get(i)?.get_or_init(|| Face::new(fb.face_data()?, fb.index, fb.name.clone(), fb.cff)).as_ref()
}

/// Face `i`: 0 is the bundled font, 1.. are the system fallbacks.
pub fn face(i: usize) -> Option<&'static Face> {
    if i == 0 { bundled() } else { fallback_face(i - 1) }
}

/// Whether the system fallback fonts have been found, so [`pick`] answers without waiting (see
/// `fallback::warm_up`).
pub fn ready() -> bool {
    super::fallback::ready()
}

/// Whether the bundled font has `ch`.
pub fn bundled_has(ch: char) -> bool {
    bundled().and_then(Face::font).and_then(|f| f.charmap().map(ch)).is_some_and(|g| g.to_u32() != 0)
}

/// Bundled font metrics (layout uses them for line placement).
pub fn metrics() -> FontMetrics {
    bundled().map_or(FALLBACK_METRICS, |f| f.metrics)
}

/// Characters that are never drawn and take no room: control characters and Unicode's
/// default-ignorable ones (direction marks, zero-width joiners and spaces, variation selectors,
/// the byte-order mark, soft hyphens, tags).
pub fn invisible(ch: char) -> bool {
    ch.is_control()
        || matches!(u32::from(ch),
            0xAD | 0x34F | 0x61C | 0x115F..=0x1160 | 0x17B4..=0x17B5 | 0x180B..=0x180F | 0x200B..=0x200F
            | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

fn bundled_glyph(ch: char) -> Option<u16> {
    bundled().and_then(Face::font).and_then(|f| f.charmap().map(ch)).and_then(|g| u16::try_from(g.to_u32()).ok()).filter(|g| *g != 0)
}

/// The face and glyph that print `ch`: the bundled font if it has it, else the first fallback
/// font that does; `(0, 0)` (the "missing glyph" box) when none has it, and for [`invisible`]
/// characters (which are not drawn). A space no font has prints as an ordinary space.
pub fn pick(ch: char) -> (usize, u16) {
    if invisible(ch) {
        return (0, 0);
    }
    if let Some(g) = bundled_glyph(ch) {
        return (0, g);
    }
    for (i, fb) in super::fallback::fallbacks().iter().enumerate() {
        if let Some(g) = fb.glyph(ch).filter(|g| *g != 0)
            && fallback_face(i).is_some()
        {
            return (i + 1, g);
        }
    }
    if ch.is_whitespace()
        && let Some(g) = bundled_glyph(' ')
    {
        return (0, g);
    }
    (0, 0)
}

/// Whether some font can draw `ch` (an invisible character needs none).
pub fn has_glyph(ch: char) -> bool {
    ch == ' ' || invisible(ch) || pick(ch).1 != 0
}

/// Width of `text` at `size` points.
pub fn width(text: &str, size: f32, bold: bool) -> f32 {
    let Some(base) = bundled() else { return text.chars().count() as f32 * size * 0.5 };
    let Some(f) = base.font() else { return text.chars().count() as f32 * size * 0.5 };
    let upm = base.metrics.units_per_em;
    let gm = f.glyph_metrics(Size::unscaled(), LocationRef::default());
    let cmap = f.charmap();
    let mut w = 0.0;
    for ch in text.chars().filter(|c| !invisible(*c)) {
        w += match cmap.map(ch) {
            Some(g) if g.to_u32() != 0 => gm.advance_width(g).map_or(0.5, |a| a / upm),
            _ => {
                let (i, g) = pick(ch);
                face(i).map_or(0.5, |f| f.advance(g))
            }
        };
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

    /// Direction marks, joiners and the like take no room and are not drawn; spaces the bundled
    /// font lacks (the ideographic space in Chinese and Japanese text, the narrow no-break space in
    /// French numbers) print as spaces, not as the missing-glyph box.
    #[test]
    fn invisible_characters_and_other_spaces() {
        let plain = width("ab", 10.0, false);
        for mark in ['\u{200F}', '\u{200D}', '\u{2066}', '\u{FEFF}', '\t', '\u{AD}'] {
            assert!(invisible(mark));
            assert_eq!(width(&format!("a{mark}b"), 10.0, false), plain, "{:?}", mark);
            assert_eq!(pick(mark), (0, 0));
            assert!(has_glyph(mark));
        }
        for space in ['\u{3000}', '\u{202F}', '\u{2009}', '\u{2003}'] {
            assert!(!invisible(space));
            assert_ne!(pick(space).1, 0, "{:?} prints as a space or from a fallback font", space);
        }
    }

    #[test]
    fn the_print_font_measures_and_fits_text() {
        assert!(pick('A').1 != 0 && pick('A').0 == 0);
        assert!(has_glyph('…'));
        assert!(bundled_has('Ж'), "Cyrillic is bundled");
        let w = width("Design review", 10.0, false);
        assert!(w > 30.0 && w < 90.0, "{w}");
        assert!(width("ab", 10.0, true) > width("ab", 10.0, false));
        let f = fit("A very long task name that does not fit", 10.0, false, 60.0);
        assert!(f.ends_with('…') && width(&f, 10.0, false) <= 60.0, "{f}");
        assert_eq!(fit("Short", 10.0, false, 100.0), "Short");
        assert_eq!(fit("Short", 10.0, false, 0.0), "");
        let m = metrics();
        assert!(m.ascent > 0.5 && m.descent < 0.0);
        assert_eq!(pick('\u{7}'), (0, 0));
    }

    /// Where the system has a CJK font, Chinese text measures with it (not as boxes).
    #[test]
    fn other_scripts_use_system_fonts_where_installed() {
        let (i, g) = pick('漢');
        if g == 0 {
            eprintln!("no CJK font on this system");
            return;
        }
        assert!(i > 0 && face(i).is_some());
        let w = width("漢字", 10.0, false);
        assert!(w > 15.0 && w < 25.0, "CJK characters are about one em wide: {w}");
    }
}

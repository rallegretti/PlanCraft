//! PDF 1.7 output for printed pages: vector drawing plus one embedded, subset TrueType font
//! (Identity-H encoded, with a ToUnicode map so text can be searched and copied).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::{Document, Item, Pt, Rect, Rgb, Stroke, font};

/// Largest coordinate written; anything beyond is clamped (PDF readers choke on huge numbers).
const MAX_COORD: f32 = 100_000.0;
/// Largest font embedded whole when it cannot be subset (it would go into every PDF).
const MAX_WHOLE_FONT: usize = 4 * 1024 * 1024;

/// A number for a content stream: finite, clamped, at most 2 decimals.
fn num(v: f32) -> String {
    let v = if v.is_finite() { v.clamp(-MAX_COORD, MAX_COORD) } else { 0.0 };
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" || s == "-0" { "0".into() } else { s.into() }
}

fn color(c: Rgb) -> String {
    format!("{} {} {}", num(f32::from(c.0) / 255.0), num(f32::from(c.1) / 255.0), num(f32::from(c.2) / 255.0))
}

/// Text as a PDF string of UTF-16BE (for the document information dictionary).
fn text_string(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        let _ = write!(out, "{u:04X}");
    }
    out.push('>');
    out
}

struct Content<'a> {
    s: String,
    height: f32,
    fonts: &'a Fonts,
}

impl Content<'_> {
    fn y(&self, y: f32) -> f32 {
        self.height - y
    }

    fn stroke_state(&mut self, st: &Stroke) {
        let w = if st.width.is_finite() { st.width.max(0.0) } else { 0.0 };
        let _ = write!(self.s, "{} RG {} w ", color(st.color), num(w));
        match st.dash {
            Some(d) if d.is_finite() && d > 0.0 => {
                let _ = write!(self.s, "[{} {}] 0 d ", num(d), num(d));
            }
            _ => self.s.push_str("[] 0 d "),
        }
    }

    fn rect_path(&mut self, r: &Rect) {
        let _ = write!(self.s, "{} {} {} {} re ", num(r.x), num(self.y(r.y + r.h)), num(r.w), num(r.h));
    }

    fn path(&mut self, pts: &[Pt]) -> bool {
        let mut first = true;
        for p in pts {
            let op = if first { "m" } else { "l" };
            first = false;
            let _ = write!(self.s, "{} {} {op} ", num(p.x), num(self.y(p.y)));
        }
        !first
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Rect { rect, fill, stroke } => {
                self.s.push_str("q ");
                if let Some(f) = fill {
                    let _ = write!(self.s, "{} rg ", color(*f));
                }
                if let Some(st) = stroke {
                    self.stroke_state(st);
                }
                self.rect_path(rect);
                self.s.push_str(match (fill, stroke) {
                    (Some(_), Some(_)) => "B",
                    (Some(_), None) => "f",
                    (None, Some(_)) => "S",
                    (None, None) => "n",
                });
                self.s.push_str(" Q\n");
            }
            Item::Line { points, stroke } => {
                if points.len() < 2 {
                    return;
                }
                self.s.push_str("q ");
                self.stroke_state(stroke);
                self.path(points);
                self.s.push_str("S Q\n");
            }
            Item::Polygon { points, fill } => {
                if points.len() < 3 {
                    return;
                }
                let _ = write!(self.s, "q {} rg ", color(*fill));
                self.path(points);
                self.s.push_str("h f Q\n");
            }
            Item::Text { at, size, color: c, bold, text } => {
                if text.is_empty() || !size.is_finite() || *size <= 0.0 {
                    return;
                }
                // Runs of characters printed with the same font.
                let mut runs: Vec<(usize, String)> = Vec::new();
                for (face, gid) in glyphs_of(text) {
                    let (slot, cid) = self.fonts.cid(face, gid);
                    match runs.last_mut() {
                        Some((s, hex)) if *s == slot => {
                            let _ = write!(hex, "{cid:04X}");
                        }
                        _ => runs.push((slot, format!("{cid:04X}"))),
                    }
                }
                if runs.is_empty() {
                    return;
                }
                let _ = write!(self.s, "q BT {} rg ", color(*c));
                if *bold {
                    // Stroked outlines for weight, and the extra advance layout measured with, so
                    // centred and right-aligned bold text lands where it was measured.
                    let extra = num(size * font::BOLD_EXTRA);
                    let _ = write!(self.s, "2 Tr {} RG {extra} w {extra} Tc ", color(*c));
                }
                let _ = write!(self.s, "{} {} Td", num(at.x), num(self.y(at.y)));
                for (slot, hex) in runs {
                    let _ = write!(self.s, " /F{} {} Tf <{hex}> Tj", slot + 1, num(*size));
                }
                self.s.push_str(" ET Q\n");
            }
            Item::Clip { rect, items } => {
                self.s.push_str("q ");
                self.rect_path(rect);
                self.s.push_str("W n\n");
                for i in items {
                    self.item(i);
                }
                self.s.push_str("Q\n");
            }
        }
    }
}

fn deflate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

struct Writer {
    out: Vec<u8>,
    offsets: Vec<usize>,
}

impl Writer {
    fn obj(&mut self, id: usize, body: &str) {
        self.start(id);
        self.out.extend_from_slice(body.as_bytes());
        self.out.extend_from_slice(b"\nendobj\n");
    }

    fn start(&mut self, id: usize) {
        if let Some(o) = self.offsets.get_mut(id) {
            *o = self.out.len();
        }
        self.out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
    }

    fn stream(&mut self, id: usize, dict: &str, data: &[u8]) {
        let packed = deflate(data);
        self.start(id);
        self.out.extend_from_slice(format!("<< {dict} /Filter /FlateDecode /Length {} >>\nstream\n", packed.len()).as_bytes());
        self.out.extend_from_slice(&packed);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
    }
}

fn to_unicode(glyphs: &BTreeMap<u16, char>) -> String {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u16, &char)> = glyphs.iter().filter(|(g, _)| **g != 0).collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", chunk.len());
        for (g, ch) in chunk {
            let mut buf = [0u16; 2];
            let units: String = ch.encode_utf16(&mut buf).iter().map(|u| format!("{u:04X}")).collect();
            let _ = writeln!(s, "<{g:04X}> <{units}>");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

/// Characters of `text` in display order (right-to-left text reversed), each with the face and
/// glyph that print it. Control characters are left out.
fn glyphs_of(text: &str) -> impl Iterator<Item = (usize, u16)> + '_ {
    let shown: Vec<char> = crate::bidi::visual(text).chars().filter(|c| !super::font::invisible(*c)).collect();
    shown.into_iter().map(font::pick)
}

/// Calls `f` with the text of every text item, inside clips too.
fn each_text(items: &[Item], f: &mut impl FnMut(&str)) {
    for item in items {
        match item {
            Item::Text { text, .. } => f(text),
            Item::Clip { items, .. } => each_text(items, f),
            _ => {}
        }
    }
}

/// How a face's glyphs are embedded.
enum Embed {
    /// A subset: CIDs are the new glyph ids.
    Subset { file: Vec<u8>, remap: super::subset::GlyphRemapper },
    /// The whole TrueType font (when it could not be subset): CIDs are the glyph ids.
    Whole,
}

/// One embedded font: a face, how it is embedded and the glyphs used (glyph id → character).
struct FontSlot {
    face: &'static font::Face,
    embed: Embed,
    glyphs: BTreeMap<u16, char>,
}

/// The fonts of a document: slot 0 is the bundled font.
struct Fonts {
    slots: Vec<FontSlot>,
    /// Face index → slot.
    slot_of: BTreeMap<usize, usize>,
}

impl Fonts {
    /// The font slot and CID for a glyph. Faces that could not be embedded print the bundled
    /// font's missing-glyph box.
    fn cid(&self, face: usize, gid: u16) -> (usize, u16) {
        let Some(&slot) = self.slot_of.get(&face) else { return (0, 0) };
        let Some(s) = self.slots.get(slot) else { return (0, 0) };
        match &s.embed {
            Embed::Subset { remap, .. } => (slot, remap.get(gid).unwrap_or(0)),
            Embed::Whole => (slot, gid),
        }
    }

    /// The CID of glyph `gid` in slot `k`.
    fn cid_in(&self, k: usize, gid: u16) -> u16 {
        match self.slots.get(k).map(|s| &s.embed) {
            Some(Embed::Subset { remap, .. }) => remap.get(gid).unwrap_or(0),
            _ => gid,
        }
    }

    /// CID → character of a slot, for its ToUnicode map.
    fn unicode(&self, slot: &FontSlot) -> BTreeMap<u16, char> {
        slot.glyphs
            .iter()
            .map(|(g, ch)| match &slot.embed {
                Embed::Subset { remap, .. } => (remap.get(*g).unwrap_or(0), *ch),
                Embed::Whole => (*g, *ch),
            })
            .collect()
    }

    /// Collects the glyphs every page uses and subsets their fonts.
    fn for_document(doc: &Document) -> Self {
        let mut used: BTreeMap<usize, BTreeMap<u16, char>> = BTreeMap::new();
        used.entry(0).or_default().insert(0, '\u{FFFD}');
        for page in &doc.pages {
            each_text(&page.items, &mut |text| {
                let shown: Vec<char> = crate::bidi::visual(text).chars().filter(|c| !super::font::invisible(*c)).collect();
                for ch in shown {
                    let (face, gid) = font::pick(ch);
                    used.entry(face).or_default().entry(gid).or_insert(ch);
                }
            });
        }
        let mut fonts = Fonts { slots: Vec::new(), slot_of: BTreeMap::new() };
        for (face_index, glyphs) in used {
            let Some(face) = font::face(face_index) else { continue };
            let keep: BTreeSet<u16> = glyphs.keys().copied().collect();
            let embed = match super::subset::subset(face.data, face.index, &keep) {
                // A CFF subset is embedded as its bare CFF table, so it must have one.
                Some((file, remap)) if !face.cff || super::subset::table(&file, b"CFF ").is_some() => Embed::Subset { file, remap },
                // A whole TrueType font works with glyph ids as CIDs, if it is small enough to put
                // in every PDF; otherwise (or a CFF font) the face is left out.
                None if !face.cff && !face.data.starts_with(b"ttcf") && face.data.len() <= MAX_WHOLE_FONT => Embed::Whole,
                _ => continue,
            };
            fonts.slot_of.insert(face_index, fonts.slots.len());
            fonts.slots.push(FontSlot { face, embed, glyphs });
        }
        fonts
    }
}

/// Objects per embedded font: Type0 font, CID font, descriptor, font file, ToUnicode map.
const FONT_OBJECTS: usize = 5;

/// The document as PDF bytes.
pub fn write(doc: &Document) -> Vec<u8> {
    let fonts = Fonts::for_document(doc);
    let mut contents = Vec::with_capacity(doc.pages.len());
    for page in &doc.pages {
        let mut c = Content { s: String::new(), height: page.height, fonts: &fonts };
        for item in &page.items {
            c.item(item);
        }
        contents.push(c.s);
    }

    // Objects: 1 catalog, 2 page tree, 3 info, five per font, then a page and its content per page.
    let first_font = 4;
    let first_page = first_font + FONT_OBJECTS * fonts.slots.len();
    let total = first_page + 2 * doc.pages.len().max(1);
    let mut w = Writer { out: b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec(), offsets: vec![0; total] };
    let page_ids: Vec<usize> = (0..doc.pages.len()).map(|i| first_page + 2 * i).collect();
    w.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = page_ids.iter().map(|id| format!("{id} 0 R")).collect();
    w.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), page_ids.len()));
    w.obj(3, &format!("<< /Title {} /Producer (PlanCraft) /Creator (PlanCraft) >>", text_string(&doc.title)));

    // Each font: Type0 → CID font → descriptor → subset font file; ToUnicode for copy/search.
    let mut font_refs = String::new();
    for (k, slot) in fonts.slots.iter().enumerate() {
        let id = first_font + FONT_OBJECTS * k;
        let _ = write!(font_refs, "/F{} {id} 0 R ", k + 1);
        let face = slot.face;
        let m = face.metrics;
        let subset_file = match &slot.embed {
            Embed::Subset { file, .. } => Some(file),
            Embed::Whole => None,
        };
        // Subset fonts get a tag unique in the document (six capital letters).
        let tag = char::from(b'A' + u8::try_from(k % 26).unwrap_or(0));
        let base = if subset_file.is_some() { format!("PCSUB{tag}+{}", face.name) } else { face.name.clone() };
        w.obj(
            id,
            &format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /{base} /Encoding /Identity-H /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
                id + 1,
                id + 4
            ),
        );
        let mut widths = String::new();
        for (g, cid) in slot.glyphs.keys().map(|g| (*g, fonts.cid_in(k, *g))) {
            let _ = write!(widths, "{cid} [{}] ", num(face.advance(g) * 1000.0));
        }
        let (subtype, gid_map) = if face.cff { ("CIDFontType0", "") } else { ("CIDFontType2", " /CIDToGIDMap /Identity") };
        w.obj(
            id + 1,
            &format!(
                "<< /Type /Font /Subtype /{subtype} /BaseFont /{base} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {} 0 R{gid_map} /DW 1000 /W [{widths}] >>",
                id + 2
            ),
        );
        let file_key = if face.cff { "FontFile3" } else { "FontFile2" };
        w.obj(
            id + 2,
            &format!(
                "<< /Type /FontDescriptor /FontName /{base} /Flags 32 /FontBBox [{} {} {} {}] /ItalicAngle {} /Ascent {} /Descent {} /CapHeight {} /StemV 80 /{file_key} {} 0 R >>",
                num(m.bbox[0] * 1000.0),
                num(m.bbox[1] * 1000.0),
                num(m.bbox[2] * 1000.0),
                num(m.bbox[3] * 1000.0),
                num(m.italic_angle),
                num(m.ascent * 1000.0),
                num(m.descent * 1000.0),
                num(m.cap_height * 1000.0),
                id + 3,
            ),
        );
        match subset_file {
            // CFF outlines are embedded as the bare CFF table.
            Some(file) if face.cff => {
                let cff = super::subset::table(file, b"CFF ").unwrap_or_default();
                w.stream(id + 3, "/Subtype /CIDFontType0C", cff);
            }
            Some(file) => w.stream(id + 3, &format!("/Length1 {}", file.len()), file),
            None => w.stream(id + 3, &format!("/Length1 {}", face.data.len()), face.data),
        }
        w.stream(id + 4, "", to_unicode(&fonts.unicode(slot)).as_bytes());
    }

    for ((page, content), id) in doc.pages.iter().zip(&contents).zip(&page_ids) {
        let (pw, ph) = (num(page.width.max(1.0)), num(page.height.max(1.0)));
        w.obj(
            *id,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /Font << {font_refs}>> >> /Contents {} 0 R >>",
                id + 1
            ),
        );
        w.stream(id + 1, "", content.as_bytes());
    }
    // An empty document still gets a valid page tree (no pages).

    let xref_at = w.out.len();
    let count = w.offsets.len();
    let mut xref = format!("xref\n0 {count}\n0000000000 65535 f \n");
    for (i, o) in w.offsets.iter().enumerate().skip(1) {
        if *o == 0 && i >= first_page + 2 * doc.pages.len() {
            xref.push_str("0000000000 65535 f \n");
        } else {
            let _ = writeln!(xref, "{o:010} 00000 n ");
        }
    }
    let _ = write!(xref, "trailer\n<< /Size {count} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref_at}\n%%EOF\n");
    w.out.extend_from_slice(xref.as_bytes());
    w.out
}

#[cfg(test)]
mod tests {
    use super::super::{Page, pt};
    use super::*;

    fn sample() -> Document {
        let black = Rgb(0, 0, 0);
        Document {
            title: "Plan – Ünïcode".into(),
            pages: vec![Page {
                width: 792.0,
                height: 612.0,
                items: vec![
                    Item::Rect {
                        rect: Rect::new(10.0, 10.0, 100.0, 20.0),
                        fill: Some(Rgb(27, 58, 140)),
                        stroke: Some(Stroke::new(1.0, black)),
                    },
                    Item::Line { points: vec![pt(0.0, 0.0), pt(50.0, 50.0)], stroke: Stroke::dashed(0.5, black, 2.0) },
                    Item::Polygon { points: vec![pt(0.0, 0.0), pt(5.0, 5.0), pt(0.0, 10.0)], fill: black },
                    Item::Clip {
                        rect: Rect::new(0.0, 0.0, 100.0, 100.0),
                        items: vec![Item::Text { at: pt(20.0, 40.0), size: 9.0, color: black, bold: true, text: "Design (é)".into() }],
                    },
                    Item::Text { at: pt(f32::NAN, f32::INFINITY), size: 9.0, color: black, bold: false, text: "safe".into() },
                ],
            }],
            truncated: false,
        }
    }

    #[test]
    fn writes_a_well_formed_pdf() {
        let bytes = write(&sample());
        assert!(bytes.starts_with(b"%PDF-1.7"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        let find = |needle: &[u8]| bytes.windows(needle.len()).rposition(|w| w == needle);
        // The xref offset points at the xref table.
        let sx = find(b"startxref\n").unwrap() + 10;
        let at: usize = std::str::from_utf8(&bytes[sx..]).unwrap().lines().next().unwrap().parse().unwrap();
        assert!(bytes[at..].starts_with(b"xref"));
        // Every object offset points at its object header.
        let xref = std::str::from_utf8(&bytes[at..]).unwrap();
        for (i, line) in xref.lines().skip(2).take_while(|l| l.ends_with(" n ") || l.ends_with(" f ")).enumerate() {
            if line.ends_with(" n ") {
                let off: usize = line.get(..10).unwrap().parse().unwrap();
                assert!(bytes[off..].starts_with(format!("{i} 0 obj").as_bytes()), "object {i}");
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Count 1"));
        assert!(text.contains("PCSUBA+Ubuntu-Light"), "font is subset");
        assert!(bytes.len() < 60_000, "{} bytes", bytes.len());
    }

    /// Text in other scripts embeds the system fonts that draw it (where installed), and
    /// Hebrew is written in display order.
    #[test]
    fn other_scripts_embed_their_fonts() {
        let black = Rgb(0, 0, 0);
        let texts = ["設計レビュー 漢字", "תכנון", "Plan ก"];
        let items = texts
            .iter()
            .zip(0..)
            .map(|(t, i)| Item::Text { at: pt(20.0, 40.0 + 30.0 * i as f32), size: 18.0, color: black, bold: false, text: (*t).into() })
            .collect();
        let doc = Document { title: "多言語".into(), pages: vec![Page { width: 600.0, height: 400.0, items }], truncated: false };
        let bytes = write(&doc);
        let text = String::from_utf8_lossy(&bytes);
        let faces: BTreeSet<usize> = texts.iter().flat_map(|t| t.chars()).map(|c| font::pick(c).0).collect();
        assert_eq!(text.matches("/Type /FontDescriptor").count(), faces.len(), "one font per face used");
        if faces.len() == 1 {
            eprintln!("no fallback fonts on this system");
            return;
        }
        assert!(text.contains("/F2 "), "a second font is on the page");
        assert!(bytes.len() < 400_000, "fonts are subset: {} bytes", bytes.len());
        if let Ok(path) = std::env::var("PLANCRAFT_TEST_PDF") {
            std::fs::write(path, &bytes).unwrap();
        }
    }

    #[test]
    fn numbers_are_always_valid() {
        assert_eq!(num(f32::NAN), "0");
        assert_eq!(num(f32::INFINITY), "0");
        assert_eq!(num(1e30), "100000");
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(-0.001), "0");
        assert_eq!(num(2.0), "2");
    }

    #[test]
    fn empty_documents_are_valid() {
        let bytes = write(&Document::default());
        assert!(String::from_utf8_lossy(&bytes).contains("/Count 0"));
    }
}

//! PDF 1.7 output for printed pages: vector drawing plus one embedded, subset TrueType font
//! (Identity-H encoded, with a ToUnicode map so text can be searched and copied).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::{Document, Item, Pt, Rect, Rgb, Stroke, font};

/// Largest coordinate written; anything beyond is clamped (PDF readers choke on huge numbers).
const MAX_COORD: f32 = 100_000.0;

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
    /// Glyphs used, with the character each stands for.
    glyphs: &'a mut BTreeMap<u16, char>,
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
                let mut hex = String::with_capacity(text.len() * 4);
                for ch in text.chars().filter(|c| !c.is_control()) {
                    let gid = font::glyph(ch);
                    self.glyphs.entry(gid).or_insert(ch);
                    let _ = write!(hex, "{gid:04X}");
                }
                let _ = write!(self.s, "q BT /F1 {} Tf {} rg ", num(*size), color(*c));
                if *bold {
                    let _ = write!(self.s, "2 Tr {} RG {} w ", color(*c), num(size * font::BOLD_EXTRA));
                }
                let _ = writeln!(self.s, "{} {} Td <{hex}> Tj ET Q", num(at.x), num(self.y(at.y)));
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

/// The document as PDF bytes.
pub fn write(doc: &Document) -> Vec<u8> {
    let mut glyphs: BTreeMap<u16, char> = BTreeMap::new();
    glyphs.insert(0, '\u{FFFD}');
    let mut contents = Vec::with_capacity(doc.pages.len());
    for page in &doc.pages {
        let mut c = Content { s: String::new(), height: page.height, glyphs: &mut glyphs };
        for item in &page.items {
            c.item(item);
        }
        contents.push(c.s);
    }

    // Objects: 1 catalog, 2 page tree, 3 info, 4-8 font, then a page and its content per page.
    let first_page = 9;
    let total = first_page + 2 * doc.pages.len().max(1);
    let mut w = Writer { out: b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec(), offsets: vec![0; total] };
    let page_ids: Vec<usize> = (0..doc.pages.len()).map(|i| first_page + 2 * i).collect();
    w.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = page_ids.iter().map(|id| format!("{id} 0 R")).collect();
    w.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), page_ids.len()));
    w.obj(3, &format!("<< /Title {} /Producer (PlanCraft) /Creator (PlanCraft) >>", text_string(&doc.title)));

    // Font: Type0 → CIDFontType2 → descriptor → subset font file; ToUnicode for copy/search.
    let m = font::metrics();
    let keep: BTreeSet<u16> = glyphs.keys().copied().collect();
    let (file, prefix) = match super::subset::subset(font::DATA, &keep) {
        Some(f) => (f, "PCSUBS+"),
        None => (font::DATA.to_vec(), ""),
    };
    let base = format!("{prefix}{}", font::NAME);
    w.obj(
        4,
        &format!("<< /Type /Font /Subtype /Type0 /BaseFont /{base} /Encoding /Identity-H /DescendantFonts [5 0 R] /ToUnicode 8 0 R >>"),
    );
    let mut widths = String::new();
    for g in glyphs.keys() {
        let _ = write!(widths, "{g} [{}] ", num(font::advance(*g) * 1000.0));
    }
    w.obj(
        5,
        &format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{base} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 6 0 R /CIDToGIDMap /Identity /DW 500 /W [{widths}] >>"
        ),
    );
    w.obj(
        6,
        &format!(
            "<< /Type /FontDescriptor /FontName /{base} /Flags 32 /FontBBox [{} {} {} {}] /ItalicAngle {} /Ascent {} /Descent {} /CapHeight {} /StemV 80 /FontFile2 7 0 R >>",
            num(m.bbox[0] * 1000.0),
            num(m.bbox[1] * 1000.0),
            num(m.bbox[2] * 1000.0),
            num(m.bbox[3] * 1000.0),
            num(m.italic_angle),
            num(m.ascent * 1000.0),
            num(m.descent * 1000.0),
            num(m.cap_height * 1000.0),
        ),
    );
    w.stream(7, &format!("/Length1 {}", file.len()), &file);
    w.stream(8, "", to_unicode(&glyphs).as_bytes());

    for ((page, content), id) in doc.pages.iter().zip(&contents).zip(&page_ids) {
        let (pw, ph) = (num(page.width.max(1.0)), num(page.height.max(1.0)));
        w.obj(
            *id,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /Font << /F1 4 0 R >> >> /Contents {} 0 R >>",
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
        assert!(text.contains("PCSUBS+Ubuntu-Light"), "font is subset");
        assert!(bytes.len() < 60_000, "{} bytes", bytes.len());
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

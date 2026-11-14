//! Font subsetting for PDF embedding, with the `subsetter` crate (TrueType and CFF outlines, font
//! collections). Glyph ids are renumbered from 0; the new ids are the CIDs written in PDFs.

use std::collections::BTreeSet;

pub use subsetter::GlyphRemapper;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?]))
}

/// A table of a (single-face) font file by tag.
pub fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let n = usize::from(u16_at(font, 4)?);
    for i in 0..n {
        let rec = 12usize.checked_add(i.checked_mul(16)?)?;
        if font.get(rec..rec.checked_add(4)?)? == tag {
            let off = usize::try_from(u32_at(font, rec + 8)?).ok()?;
            let len = usize::try_from(u32_at(font, rec + 12)?).ok()?;
            return font.get(off..off.checked_add(len)?);
        }
    }
    None
}

/// Face `index` of `font` with only `glyphs` (and glyph 0), and the map from old glyph ids to new
/// ones. `None` when the font cannot be subset (the caller then embeds it whole or not at all).
pub fn subset(font: &[u8], index: u32, glyphs: &BTreeSet<u16>) -> Option<(Vec<u8>, GlyphRemapper)> {
    let mut remap = GlyphRemapper::new();
    for g in glyphs {
        remap.remap(*g);
    }
    // System fonts are trusted less than our own code: a panic in the subsetter is an error.
    let data = crate::guard::catch("subsetting a font", || subsetter::subset(font, index, &remap)).ok()?.ok()?;
    Some((data, remap))
}

#[cfg(test)]
mod tests {
    use super::super::font;
    use super::*;

    #[test]
    fn subsets_are_small() {
        let a = font::pick('A').1;
        let keep: BTreeSet<u16> = [a, font::pick('é').1].into_iter().collect();
        let (s, remap) = subset(font::DATA, 0, &keep).expect("subset");
        assert!(s.len() < font::DATA.len() / 4, "{} bytes", s.len());
        assert_eq!(remap.get(0), Some(0), "the missing glyph stays first");
        assert_eq!(remap.get(a), Some(1));
        let glyf = table(&s, b"glyf").expect("glyf");
        assert!(!glyf.is_empty());
        assert!(table(&s, b"cmap").is_none(), "PDFs bring their own character map");
    }

    #[test]
    fn garbage_fonts_do_not_panic() {
        for junk in [&[][..], &[0, 1, 0, 0, 0, 9, 0, 0][..], &[0xFF; 64][..], &b"ttcf\0\x01\0\0\0\0\0\x09"[..]] {
            assert!(subset(junk, 0, &BTreeSet::from([1, 2])).is_none());
            assert!(subset(junk, 3, &BTreeSet::new()).is_none());
            assert!(table(junk, b"CFF ").is_none());
        }
    }
}

//! TrueType subsetting for PDF embedding: keeps the glyph ids, empties every glyph that is not
//! used (and not part of a used composite glyph), and keeps only the tables a PDF reader needs.

use std::collections::BTreeSet;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?]))
}

/// Tables of the font by tag.
fn tables(font: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let n = usize::from(u16_at(font, 4)?);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let rec = 12usize.checked_add(i.checked_mul(16)?)?;
        let tag = font.get(rec..rec + 4)?;
        let off = usize::try_from(u32_at(font, rec + 8)?).ok()?;
        let len = usize::try_from(u32_at(font, rec + 12)?).ok()?;
        let data = font.get(off..off.checked_add(len)?)?;
        out.push(([*tag.first()?, *tag.get(1)?, *tag.get(2)?, *tag.get(3)?], data));
    }
    Some(out)
}

fn find<'a>(t: &[([u8; 4], &'a [u8])], tag: &[u8; 4]) -> Option<&'a [u8]> {
    t.iter().find(|(k, _)| k == tag).map(|(_, d)| *d)
}

/// Byte range of glyph `gid` in `glyf`.
fn glyph_range(loca: &[u8], long: bool, gid: usize) -> Option<(usize, usize)> {
    if long {
        let a = usize::try_from(u32_at(loca, gid.checked_mul(4)?)?).ok()?;
        let b = usize::try_from(u32_at(loca, gid.checked_mul(4)?.checked_add(4)?)?).ok()?;
        Some((a, b.max(a)))
    } else {
        let a = usize::from(u16_at(loca, gid.checked_mul(2)?)?).checked_mul(2)?;
        let b = usize::from(u16_at(loca, gid.checked_mul(2)?.checked_add(2)?)?).checked_mul(2)?;
        Some((a, b.max(a)))
    }
}

/// Glyph ids a composite glyph refers to.
fn components(glyph: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    let Some(contours) = u16_at(glyph, 0) else { return out };
    if (contours as i16) >= 0 {
        return out;
    }
    let mut at = 10usize;
    for _ in 0..256 {
        let (Some(flags), Some(gid)) = (u16_at(glyph, at), u16_at(glyph, at + 2)) else { break };
        out.push(gid);
        at += 4;
        at += if flags & 0x0001 != 0 { 4 } else { 2 };
        if flags & 0x0008 != 0 {
            at += 2;
        } else if flags & 0x0040 != 0 {
            at += 4;
        } else if flags & 0x0080 != 0 {
            at += 8;
        }
        if flags & 0x0020 == 0 {
            break;
        }
    }
    out
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, c| {
        let mut w = [0u8; 4];
        for (d, s) in w.iter_mut().zip(c) {
            *d = *s;
        }
        sum.wrapping_add(u32::from_be_bytes(w))
    })
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

/// A font file with only `keep` glyphs (plus glyph 0 and composite parts), or `None` when the
/// font cannot be read (the caller then embeds the whole font).
pub fn subset(font: &[u8], keep: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let t = tables(font)?;
    let head = find(&t, b"head")?;
    let maxp = find(&t, b"maxp")?;
    let glyf = find(&t, b"glyf")?;
    let loca = find(&t, b"loca")?;
    let num_glyphs = usize::from(u16_at(maxp, 4)?);
    let long = u16_at(head, 50)? == 1;

    // Glyphs to keep, with composite parts.
    let mut wanted: BTreeSet<u16> = keep.iter().copied().filter(|&g| usize::from(g) < num_glyphs).collect();
    wanted.insert(0);
    let mut todo: Vec<u16> = wanted.iter().copied().collect();
    let mut guard = 0;
    while let Some(g) = todo.pop() {
        guard += 1;
        if guard > 65_536 {
            break;
        }
        let (a, b) = glyph_range(loca, long, usize::from(g))?;
        for c in components(glyf.get(a..b)?) {
            if usize::from(c) < num_glyphs && wanted.insert(c) {
                todo.push(c);
            }
        }
    }

    // New glyf and long loca.
    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity((num_glyphs + 1) * 4);
    for gid in 0..num_glyphs {
        new_loca.extend_from_slice(&u32::try_from(new_glyf.len()).ok()?.to_be_bytes());
        if wanted.contains(&u16::try_from(gid).ok()?) {
            let (a, b) = glyph_range(loca, long, gid)?;
            new_glyf.extend_from_slice(glyf.get(a..b)?);
            pad4(&mut new_glyf);
        }
    }
    new_loca.extend_from_slice(&u32::try_from(new_glyf.len()).ok()?.to_be_bytes());
    let mut new_head = head.to_vec();
    // Long loca offsets; checkSumAdjustment is recomputed below.
    new_head.get_mut(50..52)?.copy_from_slice(&1u16.to_be_bytes());
    new_head.get_mut(8..12)?.copy_from_slice(&[0; 4]);

    let mut out_tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for tag in [b"cvt ", b"fpgm", b"glyf", b"head", b"hhea", b"hmtx", b"loca", b"maxp", b"prep"] {
        let data = match tag {
            b"glyf" => new_glyf.clone(),
            b"loca" => new_loca.clone(),
            b"head" => new_head.clone(),
            _ => match find(&t, tag) {
                Some(d) => d.to_vec(),
                None => continue,
            },
        };
        out_tables.push((*tag, data));
    }

    // Write the font: offset table, table records (sorted by tag), tables.
    let n = u16::try_from(out_tables.len()).ok()?;
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow.checked_mul(2).is_some_and(|p| p <= n) {
        pow *= 2;
        log += 1;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&(pow * 16).to_be_bytes());
    out.extend_from_slice(&log.to_be_bytes());
    out.extend_from_slice(&(n * 16 - pow * 16).to_be_bytes());
    let mut offset = 12 + usize::from(n) * 16;
    let mut body = Vec::new();
    let mut head_at = None;
    for (tag, data) in &out_tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).ok()?.to_be_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).ok()?.to_be_bytes());
        if tag == b"head" {
            head_at = Some(offset);
        }
        body.extend_from_slice(data);
        pad4(&mut body);
        offset = 12 + usize::from(n) * 16 + body.len();
    }
    out.extend_from_slice(&body);
    if let Some(at) = head_at {
        let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out.get_mut(at + 8..at + 12)?.copy_from_slice(&adj.to_be_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsets_are_small_and_keep_glyph_ids() {
        let a = super::super::font::glyph('A');
        let keep: BTreeSet<u16> = [a, super::super::font::glyph('é')].into_iter().collect();
        let s = subset(super::super::font::DATA, &keep).expect("subset");
        assert!(s.len() < super::super::font::DATA.len() / 4, "{} bytes", s.len());
        let t = tables(&s).expect("tables");
        let maxp = find(&t, b"maxp").unwrap();
        let orig_maxp = find(&tables(super::super::font::DATA).unwrap(), b"maxp").unwrap();
        assert_eq!(u16_at(maxp, 4), u16_at(orig_maxp, 4), "glyph ids are kept");
        let (lo, hi) = glyph_range(find(&t, b"loca").unwrap(), true, usize::from(a)).unwrap();
        assert!(hi > lo, "kept glyph has an outline");
    }

    #[test]
    fn garbage_fonts_do_not_panic() {
        for junk in [&[][..], &[0, 1, 0, 0, 0, 9, 0, 0][..], &[0xFF; 64][..]] {
            assert!(subset(junk, &BTreeSet::from([1, 2])).is_none());
        }
    }
}

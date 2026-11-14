//! Right-to-left text (Hebrew, Arabic) put in display order.
//!
//! The app's text engine and the PDF writer draw characters left to right in the order they are
//! stored. Text with right-to-left characters is reordered with the Unicode Bidirectional
//! Algorithm first ([`visual`]), so a Hebrew task name reads correctly on screen and on paper.
//! Text being edited stays in typing order. Arabic letters are not joined (no shaping).

use std::borrow::Cow;

use unicode_bidi::BidiInfo;

/// Whether `c` makes a paragraph need reordering: a right-to-left letter or a direction mark.
fn is_rtl(c: char) -> bool {
    matches!(u32::from(c),
        0x0590..=0x08FF        // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic
        | 0xFB1D..=0xFDFF      // Hebrew and Arabic presentation forms
        | 0xFE70..=0xFEFF      // Arabic presentation forms B
        | 0x10800..=0x10FFF    // historic right-to-left scripts
        | 0x1E800..=0x1EFFF    // Mende Kikakui, Adlam, Arabic mathematical symbols
        | 0x200F | 0x202B | 0x202E | 0x2067) // RLM, RLE, RLO, RLI
}

/// Whether `s` has right-to-left text (and so differs in display order).
pub fn has_rtl(s: &str) -> bool {
    s.chars().any(is_rtl)
}

/// The mirror image of a bracket in right-to-left text, so "(" closes on the left.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        _ => c,
    }
}

/// `s` in display (left-to-right drawing) order. Text without right-to-left characters comes back
/// unchanged and unallocated. Each line is one paragraph whose direction comes from its first
/// strong character.
pub fn visual(s: &str) -> Cow<'_, str> {
    if !has_rtl(s) {
        return Cow::Borrowed(s);
    }
    let info = BidiInfo::new(s, None);
    let mut out = String::with_capacity(s.len());
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let Some(text) = s.get(run.clone()) else { continue };
            let rtl = levels.get(run.start).is_some_and(|l| l.is_rtl());
            if rtl {
                // Line breaks and other separators keep their place at the end of the line.
                out.extend(text.chars().rev().filter(|c| *c != '\n' && *c != '\r').map(mirror));
                out.extend(text.chars().filter(|c| *c == '\n' || *c == '\r'));
            } else {
                out.push_str(text);
            }
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_to_right_text_is_unchanged() {
        assert!(matches!(visual("Design review (phase 2)"), Cow::Borrowed(_)));
        assert!(matches!(visual("设计评审"), Cow::Borrowed(_)), "CJK is left to right");
        assert!(matches!(visual(""), Cow::Borrowed(_)));
    }

    #[test]
    fn hebrew_is_reversed_for_display() {
        // "shalom" stored first letter first; drawn left to right it must come out last letter first.
        assert_eq!(visual("שלום"), "םולש");
        // Numbers keep their own order inside right-to-left text.
        assert_eq!(visual("שלב 12"), "12 בלש");
        // Brackets are mirrored.
        assert_eq!(visual("(שלב)"), "(בלש)");
        // A Latin word inside a Hebrew paragraph keeps its order.
        assert_eq!(visual("תכנון PlanCraft"), "PlanCraft ןונכת");
        // A Hebrew word inside an English paragraph.
        assert_eq!(visual("Review שלום now"), "Review םולש now");
    }

    #[test]
    fn every_input_is_safe() {
        for s in ["\u{202E}", "\u{2067}abc", "א\nב", "\u{05D0}\u{0301}\u{200F}\r\n", "\u{FEFF}\u{10900}"] {
            let v = visual(s);
            assert_eq!(v.chars().count(), s.chars().count(), "{s:?}");
        }
    }
}

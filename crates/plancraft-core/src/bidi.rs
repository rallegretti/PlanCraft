//! Right-to-left text (Hebrew, Arabic) put in display order.
//!
//! Two consumers, two forms:
//! - The PDF writer draws characters left to right exactly as given, so it needs the full
//!   display order ([`visual`]): right-to-left runs reversed and their brackets mirrored. Arabic
//!   letters are not joined there (no shaping).
//! - The app's text engine (egui) already lays out each run of right-to-left letters right to
//!   left (and joins Arabic), but places runs and words in stored order. It needs [`display`]:
//!   runs and words in display order, each run of right-to-left letters left in typing order.
//!
//! Text being edited stays in typing order. Each line is its own paragraph, whose direction comes
//! from its first strong character.

use std::borrow::Cow;

use unicode_bidi::{BidiClass, BidiInfo, bidi_class};

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

/// Characters that end a paragraph (line breaks and separators).
fn ends_paragraph(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{1C}'..='\u{1E}' | '\u{85}' | '\u{2029}')
}

/// One line (no paragraph separators) in full display order.
fn visual_line(line: &str, out: &mut String) {
    if !has_rtl(line) {
        out.push_str(line);
        return;
    }
    let info = BidiInfo::new(line, None);
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let Some(text) = line.get(run.clone()) else { continue };
            if levels.get(run.start).is_some_and(|l| l.is_rtl()) {
                out.extend(text.chars().rev().map(mirror));
            } else {
                out.push_str(text);
            }
        }
    }
}

/// `s` in display (left-to-right drawing) order, for drawing character by character (PDFs).
/// Text without right-to-left characters comes back unchanged and unallocated. Line breaks keep
/// their places.
pub fn visual(s: &str) -> Cow<'_, str> {
    if !has_rtl(s) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut line_start = 0;
    for (i, c) in s.char_indices() {
        if ends_paragraph(c) {
            visual_line(s.get(line_start..i).unwrap_or(""), &mut out);
            out.push(c);
            line_start = i + c.len_utf8();
        }
    }
    visual_line(s.get(line_start..).unwrap_or(""), &mut out);
    Cow::Owned(out)
}

/// Letters (and their combining marks) that a shaping text engine lays out right to left.
fn shaped_rtl(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::R | BidiClass::AL | BidiClass::NSM) && !c.is_ascii()
}

/// `s` for the app's text engine, which shapes each run of right-to-left letters itself: the
/// display order of [`visual`] with every run of right-to-left letters turned back into typing
/// order. Unchanged and unallocated without right-to-left characters.
pub fn display(s: &str) -> Cow<'_, str> {
    let Cow::Owned(v) = visual(s) else { return Cow::Borrowed(s) };
    let mut out = String::with_capacity(v.len());
    let mut run = String::new();
    for c in v.chars() {
        if shaped_rtl(c) {
            run.push(c);
            continue;
        }
        out.extend(run.chars().rev());
        run.clear();
        out.push(c);
    }
    out.extend(run.chars().rev());
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
    fn line_breaks_keep_their_places() {
        assert_eq!(visual("שלב 12\nשלב 3"), "12 בלש\n3 בלש");
        assert_eq!(visual("אב cd\nxy"), "cd בא\nxy");
        assert_eq!(visual("אב\u{2029}cd"), "בא\u{2029}cd");
        assert_eq!(visual("א\r\nב"), "א\r\nב");
    }

    /// egui lays out each run of right-to-left letters right to left itself; it gets the runs in
    /// display order with each run's letters in typing order (see the app's layout test).
    #[test]
    fn display_order_for_a_shaping_text_engine() {
        assert!(matches!(display("Design review"), Cow::Borrowed(_)));
        assert_eq!(display("שלום"), "שלום");
        assert_eq!(display("שלום עולם"), "עולם שלום");
        assert_eq!(display("תכנון PlanCraft"), "PlanCraft תכנון");
        assert_eq!(display("Review שלום now"), "Review שלום now");
        assert_eq!(display("שלב 12"), "12 שלב");
        assert_eq!(display("(שלב)"), "(שלב)");
        // Combining marks stay after their letters.
        assert_eq!(display("שָׁלוֹם"), "שָׁלוֹם");
    }

    #[test]
    fn every_input_is_safe() {
        for s in ["\u{202E}", "\u{2067}abc", "א\nב", "\u{05D0}\u{0301}\u{200F}\r\n", "\u{FEFF}\u{10900}", "\n\n", "א\u{85}"] {
            assert_eq!(visual(s).chars().count(), s.chars().count(), "{s:?}");
            assert_eq!(display(s).chars().count(), s.chars().count(), "{s:?}");
        }
    }
}

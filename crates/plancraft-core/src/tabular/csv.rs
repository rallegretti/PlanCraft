//! Reading CSV and tab-separated text (RFC 4180, plus what spreadsheets write in practice).
//!
//! - Encodings: UTF-8 with or without a byte order mark, UTF-16 with a byte order mark (Excel's
//!   "Unicode Text"), and otherwise Windows-1252 (Excel's "CSV" on Western Windows systems).
//! - Separators: comma, semicolon (spreadsheets in countries that write decimal commas) or tab,
//!   chosen by counting them on the first line; Excel's `sep=;` first line names it outright.
//! - Quoted cells may hold separators, line breaks and doubled quotes.

use super::{Grid, Limits};

/// Decodes a text file's bytes (see the module notes for the encodings).
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], le: bool| {
        let units = rest.as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) });
        char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect::<String>()
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            Err(_) => encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes).0.into_owned(),
        },
    }
}

/// The separator a file uses: named by an Excel `sep=` line (which is then skipped), or the most
/// common of comma, semicolon and tab on the first line (outside quotes; comma on a tie).
fn separator(text: &str) -> (char, &str) {
    let first = text.lines().next().unwrap_or("");
    if let Some(rest) = first.trim().strip_prefix("sep=")
        && let Some(sep) = rest.chars().next()
        && sep != '"'
    {
        let skip = text.find('\n').map_or(text.len(), |i| i.saturating_add(1));
        return (sep, text.get(skip..).unwrap_or(""));
    }
    let mut counts = [(',', 0usize), (';', 0), ('\t', 0)];
    let mut quoted = false;
    for c in first.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && let Some(entry) = counts.iter_mut().find(|(s, _)| *s == c) {
            entry.1 = entry.1.saturating_add(1);
        }
    }
    // `max_by_key` returns the last of equal counts; reversed, a tie goes to the earlier entry.
    let sep = counts.iter().rev().max_by_key(|(_, n)| *n).map_or(',', |(s, _)| *s);
    (sep, text)
}

/// Reads CSV text into a grid: the first row with any text holds the column headings.
pub fn read(text: &str, limits: &mut Limits) -> Grid {
    let (sep, body) = separator(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = body.chars().peekable();
    // Ends the current cell (cells past the last column are dropped); false once the file's
    // budget of cells and text is spent.
    let end_cell = |row: &mut Vec<String>, cell: &mut String, limits: &mut Limits| -> bool {
        let text = unguard(std::mem::take(cell));
        if row.len() >= limits.max_columns() {
            limits.columns_cut();
            return true;
        }
        if !limits.spend(1, text.len()) {
            return false;
        }
        row.push(limits.cell(text));
        true
    };
    // Ends the current row: kept when it has any text (without its empty cells at the end).
    let end_row = |rows: &mut Vec<Vec<String>>, row: &mut Vec<String>, limits: &mut Limits| -> bool {
        while row.last().is_some_and(|c| c.trim().is_empty()) {
            row.pop();
        }
        if row.is_empty() {
            return true;
        }
        if rows.len() > limits.max_rows() {
            limits.rows_cut();
            row.clear();
            return false;
        }
        rows.push(std::mem::take(row));
        true
    };
    let mut full = false;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    limits.push_char(&mut cell, '"');
                } else {
                    quoted = false;
                }
            } else {
                limits.push_char(&mut cell, c);
            }
            continue;
        }
        match c {
            '"' if cell.trim().is_empty() => {
                // A quote opens a quoted cell only at its start (spaces before it are dropped).
                cell.clear();
                quoted = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                if !end_cell(&mut row, &mut cell, limits) || !end_row(&mut rows, &mut row, limits) {
                    full = true;
                    break;
                }
            }
            c if c == sep => {
                if !end_cell(&mut row, &mut cell, limits) {
                    full = true;
                    break;
                }
            }
            c => limits.push_char(&mut cell, c),
        }
    }
    if !full && end_cell(&mut row, &mut cell, limits) {
        end_row(&mut rows, &mut row, limits);
    }
    Grid::from_rows(rows)
}

/// A cell PlanCraft's own export guarded against running as a formula (an apostrophe in front of
/// `=`, `+`, `-`, `@` or a tab or line break), without the apostrophe.
fn unguard(cell: String) -> String {
    match cell.strip_prefix('\'') {
        Some(rest)
            if rest.starts_with(['\t', '\r', '\n']) || rest.trim_start().starts_with(['=', '+', '-', '@', '＝', '＋', '－', '＠']) =>
        {
            rest.to_string()
        }
        _ => cell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(text: &str) -> Grid {
        read(text, &mut Limits::default())
    }

    #[test]
    fn reads_quoted_cells_with_separators_quotes_and_line_breaks() {
        let g = grid("Name,Notes\r\n\"Design, phase 1\",\"Say \"\"hi\"\"\nthen go\"\r\nBuild,\r\n");
        assert_eq!(g.headers, ["Name", "Notes"]);
        // Empty cells at the end of a row are not kept.
        assert_eq!(g.rows, [vec!["Design, phase 1".to_string(), "Say \"hi\"\nthen go".into()], vec!["Build".into()]]);
    }

    #[test]
    fn finds_semicolon_and_tab_separators_and_excels_sep_line() {
        assert_eq!(grid("Name;Duration\nA;3,5 days\n").rows, [vec!["A".to_string(), "3,5 days".into()]]);
        assert_eq!(grid("Name\tDuration\nA\t3d\n").rows, [vec!["A".to_string(), "3d".into()]]);
        let g = grid("sep=;\nName;Start\nA;2026-10-19\n");
        assert_eq!(g.headers, ["Name", "Start"]);
        assert_eq!(g.rows.len(), 1);
    }

    #[test]
    fn skips_blank_lines_and_keeps_short_rows() {
        let g = grid("\n\nName,Duration,Start\n\nA\n,,\nB,2d\n");
        assert_eq!(g.headers, ["Name", "Duration", "Start"]);
        assert_eq!(g.rows, [vec!["A".to_string()], vec!["B".into(), "2d".into()]]);
    }

    #[test]
    fn decodes_byte_order_marks_and_windows_1252() {
        assert_eq!(decode(b"\xEF\xBB\xBFName"), "Name");
        assert_eq!(decode(&[0xFF, 0xFE, b'N', 0, b'o', 0]), "No");
        assert_eq!(decode(b"Caf\xE9"), "Café");
        assert_eq!(decode("Projekt Ü".as_bytes()), "Projekt Ü");
    }

    #[test]
    fn the_exports_formula_guard_comes_off() {
        let g = grid("Name\n\"'-Prep\"\n\"'=1+1\"\n'tis\n");
        assert_eq!(g.rows, [vec!["-Prep".to_string()], vec!["=1+1".into()], vec!["'tis".into()]]);
    }

    #[test]
    fn a_file_full_of_separators_stops_at_the_cell_budget() {
        let line = ",".repeat(1000) + "x\n";
        let text = format!("Name\n{}", line.repeat(6000));
        let mut limits = Limits::default();
        let g = read(&text, &mut limits);
        let cells: usize = g.rows.iter().map(Vec::len).sum();
        assert!(cells <= super::super::MAX_CELLS, "{cells} cells kept");
        assert!(limits.into_notes().iter().any(|n| n.contains("only its first part")));
    }

    #[test]
    fn an_unclosed_quote_runs_to_the_end_without_failing() {
        let g = grid("Name\n\"never closed\nstill the same cell");
        assert_eq!(g.rows, [vec!["never closed\nstill the same cell".to_string()]]);
    }
}

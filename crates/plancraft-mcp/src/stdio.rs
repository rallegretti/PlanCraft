//! Newline-delimited JSON-RPC over stdin/stdout (the MCP stdio transport).
//!
//! Only protocol messages are written to stdout; diagnostics go to stderr. Lines longer than
//! [`MAX_LINE`] are skipped with a parse error instead of being buffered without bound.

use std::io::{self, BufRead, Read, Write};

use crate::server::McpServer;

/// Largest accepted message (one line).
pub const MAX_LINE: u64 = 32 * 1024 * 1024;

/// One line read from a client.
pub(crate) enum Line {
    Eof,
    TooLong,
    Text(String),
}

/// Reads one line of at most [`MAX_LINE`] bytes; a longer line is consumed and reported.
pub(crate) fn read_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<Line> {
    buf.clear();
    let n = reader.by_ref().take(MAX_LINE + 1).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(Line::Eof);
    }
    if buf.last() != Some(&b'\n') && u64::try_from(buf.len()).unwrap_or(u64::MAX) > MAX_LINE {
        // Skip the rest of the oversized line.
        loop {
            buf.clear();
            let n = reader.by_ref().take(MAX_LINE).read_until(b'\n', buf)?;
            if n == 0 || buf.last() == Some(&b'\n') {
                break;
            }
        }
        return Ok(Line::TooLong);
    }
    Ok(Line::Text(String::from_utf8_lossy(buf).trim().to_string()))
}

/// Serves MCP over the process's stdin/stdout until stdin is closed.
pub fn run_stdio(server: &McpServer) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_stdio_with(server, stdin.lock(), stdout.lock())
}

/// Serves MCP over arbitrary reader/writer streams (one JSON message per line).
pub fn run_stdio_with<R: BufRead, W: Write>(server: &McpServer, mut reader: R, mut writer: W) -> io::Result<()> {
    let mut buf = Vec::new();
    loop {
        let resp = match read_line(&mut reader, &mut buf)? {
            Line::Eof => return Ok(()),
            Line::TooLong => Some(crate::protocol::parse_error("message too large")),
            Line::Text(line) if line.is_empty() => None,
            Line::Text(line) => server.handle_text(&line),
        };
        if let Some(resp) = resp {
            serde_json::to_writer(&mut writer, &resp).map_err(io::Error::other)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_lines_are_skipped() {
        let big = "x".repeat(usize::try_from(MAX_LINE).unwrap() + 10);
        let input = format!("{big}\nshort\n");
        let mut r = io::Cursor::new(input.into_bytes());
        let mut buf = Vec::new();
        assert!(matches!(read_line(&mut r, &mut buf).unwrap(), Line::TooLong));
        assert!(matches!(read_line(&mut r, &mut buf).unwrap(), Line::Text(t) if t == "short"));
        assert!(matches!(read_line(&mut r, &mut buf).unwrap(), Line::Eof));
    }
}

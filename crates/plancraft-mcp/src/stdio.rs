//! Newline-delimited JSON-RPC over stdin/stdout (the MCP stdio transport).
//!
//! Only protocol messages are written to stdout; diagnostics go to stderr.

use std::io::{self, BufRead, Write};

use crate::server::McpServer;

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
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(());
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(resp) = server.handle_text(line) {
            serde_json::to_writer(&mut writer, &resp).map_err(io::Error::other)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
}

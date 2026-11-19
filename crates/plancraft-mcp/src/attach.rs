//! `plancraft mcp --attach`: a stdio MCP server that forwards to the running desktop app.
//!
//! MCP clients that launch servers as commands (Claude Code's default) connect this way. The
//! bridge reads the app's session file (URL and bearer token, owner-only) and relays each
//! JSON-RPC line over HTTP, so the token never has to be copied into a client configuration and
//! a restarted app (with a new token) is picked up automatically.

use std::io::{self, BufRead, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::{Value, json};

use crate::session::{self, SessionInfo};

use crate::stdio::{Line, read_line};

/// Largest response accepted from the app.
const MAX_RESPONSE: u64 = 64 * 1024 * 1024;

/// Relays stdin to the desktop app until stdin closes.
pub fn run_attach() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_attach_with(stdin.lock(), stdout.lock(), &session::read)
}

/// Relays `reader` to the app described by `session` (re-read after a refused token).
pub fn run_attach_with<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    session: &dyn Fn() -> io::Result<SessionInfo>,
) -> io::Result<()> {
    let mut buf = Vec::new();
    // The protocol revision the client agreed with the app, sent as `MCP-Protocol-Version`.
    let mut version: Option<String> = None;
    loop {
        let line = match read_line(&mut reader, &mut buf)? {
            Line::Eof => return Ok(()),
            Line::TooLong => {
                write_message(&mut writer, &crate::protocol::parse_error("message too large"))?;
                continue;
            }
            Line::Text(t) => t,
        };
        if line.is_empty() {
            continue;
        }
        match forward(&line, version.as_deref(), session) {
            Ok(Some(reply)) => {
                if let Some(v) = crate::server::negotiated_version(&reply) {
                    version = Some(v.to_string());
                }
                write_message(&mut writer, &reply)?;
            }
            Ok(None) => {}
            Err(e) => {
                // Answer every request (not notifications) so the client isn't left waiting,
                // including each request of a batch.
                let msg = format!("PlanCraft is not reachable: {e}. Open PlanCraft and start the server in Project ▸ AI Agents.");
                let error = |id: &Value| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": msg } });
                match serde_json::from_str::<Value>(&line) {
                    Ok(Value::Array(items)) => {
                        let replies: Vec<Value> = items.iter().filter_map(|m| m.get("id")).map(error).collect();
                        if !replies.is_empty() {
                            write_message(&mut writer, &Value::Array(replies))?;
                        }
                    }
                    Ok(v) => {
                        if let Some(id) = v.get("id") {
                            write_message(&mut writer, &error(id))?;
                        }
                    }
                    Err(_) => write_message(&mut writer, &crate::protocol::parse_error("the message is not valid JSON"))?,
                }
            }
        }
    }
}

fn write_message<W: Write>(w: &mut W, v: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *w, v).map_err(io::Error::other)?;
    w.write_all(b"\n")?;
    w.flush()
}

/// Sends one message to the app; retries once with a fresh session file after a 401 (the app
/// restarted and has a new token).
fn forward(line: &str, version: Option<&str>, session: &dyn Fn() -> io::Result<SessionInfo>) -> io::Result<Option<Value>> {
    let info = session().map_err(|e| io::Error::new(e.kind(), format!("no running PlanCraft session found ({e})")))?;
    match post_retrying(&info, line, version)? {
        (401, _) => {
            let fresh = session()?;
            match post_retrying(&fresh, line, version)? {
                (401, _) => Err(io::Error::new(io::ErrorKind::PermissionDenied, "the app refused the session token")),
                reply => parse_reply(reply),
            }
        }
        reply => parse_reply(reply),
    }
}

/// [`post`], waiting and trying again (up to three times) while the app answers `503` because it
/// is serving many requests at once.
fn post_retrying(info: &SessionInfo, body: &str, version: Option<&str>) -> io::Result<(u16, Vec<u8>)> {
    let mut reply = post(info, body, version)?;
    for _ in 0..3 {
        if reply.0 != 503 {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
        reply = post(info, body, version)?;
    }
    if reply.0 == 503 {
        return Err(io::Error::other("PlanCraft is busy with many requests at once; try again"));
    }
    Ok(reply)
}

fn parse_reply((status, body): (u16, Vec<u8>)) -> io::Result<Option<Value>> {
    match status {
        202 => Ok(None),
        200 | 400 | 413 => serde_json::from_slice(&body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        other => Err(io::Error::other(format!("HTTP {other}: {}", String::from_utf8_lossy(&body).trim()))),
    }
}

/// The socket address and path of a loopback `http://` URL (anything else is refused).
fn loopback_target(url: &str) -> io::Result<(SocketAddr, String)> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, format!("the session URL '{url}' is not a local http:// address"));
    let rest = url.strip_prefix("http://").ok_or_else(bad)?;
    let (authority, path) = rest.split_once('/').map_or((rest, String::from("/")), |(a, p)| (a, format!("/{p}")));
    let addr: SocketAddr = authority.parse().map_err(|_| bad())?;
    if !addr.ip().is_loopback() {
        return Err(bad());
    }
    Ok((addr, path))
}

/// A minimal HTTP/1.1 POST over a loopback socket. Returns the status and body.
fn post(info: &SessionInfo, body: &str, version: Option<&str>) -> io::Result<(u16, Vec<u8>)> {
    let (addr, path) = loopback_target(&info.url)?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    s.set_read_timeout(Some(Duration::from_secs(600)))?;
    s.set_write_timeout(Some(Duration::from_secs(60)))?;
    // Only a supported revision is sent (it came from the app's own reply), so no CR or LF can
    // reach the header.
    let version_header = version
        .filter(|v| crate::protocol::SUPPORTED_PROTOCOL_VERSIONS.contains(v))
        .map(|v| format!("MCP-Protocol-Version: {v}\r\n"))
        .unwrap_or_default();
    // A fresh value the app must answer with proof of knowing the token (`session::proof`).
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\n{}: {nonce}\r\nContent-Type: application/json\r\nAccept: application/json\r\n{version_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
        info.token,
        session::NONCE_HEADER,
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body.as_bytes())?;
    let mut raw = Vec::new();
    s.take(MAX_RESPONSE).read_to_end(&mut raw)?;
    let split =
        raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad HTTP reply"))?;
    let (head, rest) = raw.split_at(split);
    let head = String::from_utf8_lossy(head);
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad HTTP status line"))?;
    let body = rest.get(4..).unwrap_or_default();
    // Some HTTP servers send large replies with chunked transfer encoding.
    let chunked = head.lines().skip(1).any(|l| {
        l.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("transfer-encoding") && value.split(',').any(|v| v.trim().eq_ignore_ascii_case("chunked"))
        })
    });
    // A reply the app gave past the token check carries the proof; one without it (or with a
    // wrong one) comes from another program listening where PlanCraft was (it crashed and left
    // its session file), whose replies must never reach the agent.
    if matches!(status, 200 | 202 | 400 | 413) {
        let shown = head.lines().skip(1).find_map(|l| {
            l.split_once(':').filter(|(name, _)| name.trim().eq_ignore_ascii_case(session::PROOF_HEADER)).map(|(_, v)| v.trim().to_string())
        });
        if !shown.is_some_and(|p| session::tokens_match(&p, &session::proof(&info.token, &nonce))) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "the program answering at {} is not this PlanCraft (it does not know the session); open PlanCraft and start the server in Project ▸ AI Agents",
                    info.url
                ),
            ));
        }
    }
    let body = if chunked { decode_chunked(body)? } else { body.to_vec() };
    Ok((status, body))
}

/// Decodes an HTTP/1.1 chunked body (chunk extensions and trailers are ignored).
fn decode_chunked(mut data: &[u8]) -> io::Result<Vec<u8>> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("bad chunked HTTP reply: {what}"));
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n").ok_or_else(|| bad("missing chunk size"))?;
        let size_line = data.get(..line_end).ok_or_else(|| bad("missing chunk size"))?;
        let size_text = std::str::from_utf8(size_line).map_err(|_| bad("chunk size is not text"))?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| bad("chunk size is not hexadecimal"))?;
        let start = line_end.checked_add(2).ok_or_else(|| bad("chunk too large"))?;
        if size == 0 {
            return Ok(out);
        }
        let end = start.checked_add(size).ok_or_else(|| bad("chunk too large"))?;
        out.extend_from_slice(data.get(start..end).ok_or_else(|| bad("truncated chunk"))?);
        if data.get(end..end.saturating_add(2)) != Some(b"\r\n".as_slice()) {
            return Err(bad("chunk not followed by CRLF"));
        }
        data = data.get(end.saturating_add(2)..).ok_or_else(|| bad("truncated chunk"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_bodies_decode() {
        assert_eq!(decode_chunked(b"4\r\nWiki\r\n5;ext=1\r\npedia\r\n0\r\n\r\n").unwrap(), b"Wikipedia");
        assert_eq!(decode_chunked(b"0\r\n\r\n").unwrap(), b"");
        assert_eq!(decode_chunked(b"A\r\n0123456789\r\n0\r\n").unwrap(), b"0123456789");
        // Hostile or truncated framing is an error, never a panic.
        for junk in [&b""[..], b"zz\r\nab\r\n", b"5\r\nab", b"3\r\nabcXX0\r\n", b"ffffffffffffffffff\r\n", b"4\r\nWiki"] {
            assert!(decode_chunked(junk).is_err(), "{junk:?}");
        }
    }

    #[test]
    fn only_loopback_urls() {
        assert_eq!(loopback_target("http://127.0.0.1:7878/mcp").unwrap().1, "/mcp");
        assert!(loopback_target("http://[::1]:7878/mcp").is_ok());
        assert!(loopback_target("http://192.168.1.5:7878/mcp").is_err());
        assert!(loopback_target("https://127.0.0.1:7878/mcp").is_err());
        assert!(loopback_target("http://evil.example:80/").is_err());
    }
}

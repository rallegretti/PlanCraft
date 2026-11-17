#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Streamable HTTP transport over a real socket.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use plancraft_core::workspace::Workspace;
use plancraft_mcp::{HttpLimits, McpServer, serve_http, serve_http_with};
use serde_json::{Value, json};

struct HttpReply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpReply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("bad JSON body {:?}: {e}", self.body))
    }
}

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

fn send(port: u16, method: &str, path: &str, extra_headers: &[(&str, &str)], body: &str) -> HttpReply {
    let auth = format!("Bearer {TOKEN}");
    let mut headers = vec![("Authorization", auth.as_str())];
    headers.extend_from_slice(extra_headers);
    send_raw(port, method, path, &headers, body)
}

fn send_raw(port: u16, method: &str, path: &str, extra_headers: &[(&str, &str)], body: &str) -> HttpReply {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    HttpReply { status, headers, body: body.to_string() }
}

#[test]
fn short_tokens_are_refused() {
    let server = Arc::new(McpServer::new(Workspace::default().shared()));
    assert!(serve_http(server, "127.0.0.1:0", "short").is_err());
}

#[test]
fn http_transport() {
    let server = Arc::new(McpServer::new(Workspace::default().shared()));
    let handle = serve_http(server, "127.0.0.1:0", TOKEN).unwrap();
    let port = handle.port();
    assert_ne!(port, 0);
    assert!(handle.url().ends_with("/mcp"));

    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } }).to_string();
    let r = send(port, "POST", "/mcp", &[], &init);
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.header("Content-Type").unwrap().starts_with("application/json"));
    // The server is stateless: no session id is issued, so none is expected back.
    assert!(r.header("Mcp-Session-Id").is_none());
    let v = r.json();
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");

    // Notifications are accepted with 202 and no body.
    let r = send(port, "POST", "/mcp", &[], &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());
    assert_eq!(r.status, 202);

    // Tool call through HTTP, also at "/".
    let call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "new_project", "arguments": { "name": "Over HTTP", "start": "2026-10-19" } } }).to_string();
    let r = send(port, "POST", "/", &[("Origin", "http://localhost:5173")], &call);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["result"]["structuredContent"]["project"]["name"], "Over HTTP");
    assert!(r.header("Mcp-Session-Id").is_none());

    // DNS-rebinding protection.
    let r = send(port, "POST", "/mcp", &[("Origin", "http://evil.example.com")], &init);
    assert_eq!(r.status, 403);
    // A missing, wrong or truncated bearer token is refused before anything runs.
    let r = send_raw(port, "POST", "/mcp", &[], &init);
    assert_eq!(r.status, 401);
    assert!(r.header("WWW-Authenticate").unwrap().starts_with("Bearer"));
    let wrong = format!("Bearer {}x", &TOKEN[..TOKEN.len() - 1]);
    assert_eq!(send_raw(port, "POST", "/mcp", &[("Authorization", &wrong)], &init).status, 401);
    assert_eq!(send_raw(port, "POST", "/mcp", &[("Authorization", TOKEN)], &init).status, 401);
    assert_eq!(send_raw(port, "GET", "/mcp", &[], "").status, 401);
    // The scheme is case-insensitive, as in RFC 7235.
    let lower = format!("bearer {TOKEN}");
    assert_eq!(send_raw(port, "POST", "/mcp", &[("Authorization", &lower)], &init).status, 200);
    // GET has no SSE stream.
    let r = send(port, "GET", "/mcp", &[], "");
    assert_eq!(r.status, 405);
    // DELETE is accepted (there is no session to end).
    let r = send(port, "DELETE", "/mcp", &[], "");
    assert_eq!(r.status, 200);
    // Unknown path.
    assert_eq!(send(port, "POST", "/other", &[], &init).status, 404);
    // Malformed JSON.
    let r = send(port, "POST", "/mcp", &[], "{oops");
    assert_eq!(r.status, 400);
    assert_eq!(r.json()["error"]["code"], -32700);

    handle.shutdown();
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err() || send_fails(port));
}

/// After shutdown the port may still accept briefly at the OS level; a request must not be served.
fn send_fails(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) else {
        return true;
    };
    s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    let _ = s.write_all(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}");
    let mut buf = [0u8; 16];
    !matches!(s.read(&mut buf), Ok(n) if n > 0)
}

#[test]
fn attach_bridge_relays_and_follows_a_new_token() {
    use plancraft_mcp::attach::run_attach_with;
    use plancraft_mcp::session::{SessionInfo, new_token};
    use std::cell::RefCell;

    let token = new_token();
    let server = Arc::new(McpServer::new(Workspace::default().shared()));
    let handle = serve_http(server, "127.0.0.1:0", &token).unwrap();
    // The first read returns a stale token (an app restart); the bridge must re-read and retry.
    let reads = RefCell::new(0);
    let session = || {
        *reads.borrow_mut() += 1;
        let t = if *reads.borrow() == 1 { new_token() } else { token.clone() };
        Ok(SessionInfo { url: handle.url(), token: t, pid: 1 })
    };
    let input = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
        "{not json".to_string(),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "new_project", "arguments": { "name": "Via bridge" } } })
            .to_string(),
    ]
    .join("\n");
    let mut out = Vec::new();
    run_attach_with(std::io::Cursor::new(input), &mut out, &session).unwrap();
    let replies: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"], json!({}));
    assert_eq!(replies[1]["error"]["code"], -32700);
    assert_eq!(replies[2]["id"], 2);
    assert!(replies[2]["result"]["content"][0]["text"].as_str().unwrap().contains("Via bridge"));

    // With the app gone, requests get an error reply instead of silence.
    let url = handle.url();
    handle.shutdown();
    let gone = || Ok(SessionInfo { url: url.clone(), token: token.clone(), pid: 1 });
    let mut out = Vec::new();
    run_attach_with(std::io::Cursor::new(json!({ "jsonrpc": "2.0", "id": 9, "method": "ping" }).to_string()), &mut out, &gone).unwrap();
    let reply: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(reply["id"], 9);
    assert!(reply["error"]["message"].as_str().unwrap().contains("not reachable"));
}

/// Regression: replies of 32 KiB or more used to arrive chunked, and the bridge handed the chunk
/// framing to the JSON parser ("trailing characters"). Large replies must still relay intact.
#[test]
fn attach_bridge_relays_large_replies() {
    use plancraft_mcp::attach::run_attach_with;
    use plancraft_mcp::session::{SessionInfo, new_token};

    let token = new_token();
    let server = Arc::new(McpServer::new(Workspace::default().shared()));
    let handle = serve_http(server, "127.0.0.1:0", &token).unwrap();
    let session = || Ok(SessionInfo { url: handle.url(), token: token.clone(), pid: 1 });
    let input = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string();
    let mut out = Vec::new();
    run_attach_with(std::io::Cursor::new(input), &mut out, &session).unwrap();
    assert!(out.len() > 32 * 1024, "tools/list should be a large reply ({} bytes)", out.len());
    let reply: Value = serde_json::from_slice(&out).unwrap();
    assert!(reply.get("error").is_none(), "{reply}");
    assert!(reply["result"]["tools"].as_array().unwrap().len() > 50);
    handle.shutdown();
}

fn server() -> Arc<McpServer> {
    Arc::new(McpServer::new(Workspace::default().shared()))
}

/// Writes raw bytes and reads the reply's status (0 when the connection closed without one).
fn raw_status(port: u16, request: &[u8]) -> u16 {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let _ = s.write_all(request);
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    String::from_utf8_lossy(&raw).split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
}

/// Regression: the Host check accepted any name starting with "127.", so a page served from
/// `127.evil.example` (resolving to 127.0.0.1) passed the DNS-rebinding protection.
#[test]
fn host_names_that_only_look_like_loopback_are_refused() {
    let handle = serve_http(server(), "127.0.0.1:0", TOKEN).unwrap();
    let port = handle.port();
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string();
    for host in ["127.evil.example", "127.0.0.1.evil.example:80", "localhost.evil.example"] {
        let req =
            format!("POST /mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\n\r\n{ping}", ping.len());
        assert_eq!(raw_status(port, req.as_bytes()), 403, "{host}");
    }
    let r = send(port, "POST", "/mcp", &[("Origin", "http://127.evil.example:7878")], &ping);
    assert_eq!(r.status, 403);
    // A request with no Host header at all is refused too.
    let req = format!("POST /mcp HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\n\r\n{ping}", ping.len());
    assert_eq!(raw_status(port, req.as_bytes()), 403);
    handle.shutdown();
}

/// Regression: every connection got a thread and its body was read before the token was checked,
/// with no time limit. Now a client without the token is refused before its body is read, a
/// client that stalls is cut off, and connections beyond the limit are turned away at once.
#[test]
fn slow_unauthenticated_and_excess_clients_are_cut_off() {
    let limits = HttpLimits { max_connections: 4, read_timeout: Duration::from_millis(300), request_deadline: Duration::from_secs(2) };
    let handle = serve_http_with(server(), "127.0.0.1:0", TOKEN, limits).unwrap();
    let port = handle.port();

    // No token, a huge declared body that never comes: refused straight away, body unread.
    let started = std::time::Instant::now();
    let req = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n", 30 * 1024 * 1024);
    assert_eq!(raw_status(port, req.as_bytes()), 401);
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());

    // A body larger than the limit is refused from its Content-Length alone.
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {}\r\n\r\n",
        64 * 1024 * 1024
    );
    assert_eq!(raw_status(port, req.as_bytes()), 413);

    // Headers that stop arriving: 408 after the read timeout.
    let started = std::time::Instant::now();
    assert_eq!(raw_status(port, b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n"), 408);
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());

    // A client that drips one byte at a time (each read in time) still has to finish by the deadline.
    let mut drip = TcpStream::connect(("127.0.0.1", port)).unwrap();
    drip.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let started = std::time::Instant::now();
    let head = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 1000\r\n\r\n");
    drip.write_all(head.as_bytes()).unwrap();
    while started.elapsed() < Duration::from_secs(4) {
        if drip.write_all(b" ").is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut raw = Vec::new();
    let _ = drip.read_to_end(&mut raw);
    assert!(String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 408"), "{}", String::from_utf8_lossy(&raw));

    // Oversized headers: 431.
    let req = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Big: {}\r\n\r\n", "a".repeat(70 * 1024));
    assert_eq!(raw_status(port, req.as_bytes()), 431);

    // Four idle connections fill the server; the fifth is turned away with 503.
    let idle: Vec<TcpStream> = (0..4).map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap()).collect();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(raw_status(port, b"GET /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"), 503);
    drop(idle);
    // Once they are gone (timed out or closed) the server takes requests again.
    std::thread::sleep(Duration::from_millis(600));
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string();
    assert_eq!(send(port, "POST", "/mcp", &[], &ping).status, 200);
    handle.shutdown();
}

/// Bodies sent chunked or after `Expect: 100-continue` are read; malformed framing is refused.
#[test]
fn chunked_and_expect_continue_bodies() {
    let handle = serve_http(server(), "127.0.0.1:0", TOKEN).unwrap();
    let port = handle.port();
    let ping = json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" }).to_string();
    let (a, b) = ping.split_at(10);
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x};ext=1\r\n{b}\r\n0\r\nX-Trailer: 1\r\n\r\n",
        a.len(),
        b.len()
    );
    assert_eq!(raw_status(port, req.as_bytes()), 200);
    for bad in ["zz\r\n", "5\r\nabcdefgh\r\n0\r\n\r\n"] {
        let req =
            format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nTransfer-Encoding: chunked\r\n\r\n{bad}");
        assert_eq!(raw_status(port, req.as_bytes()), 400, "{bad:?}");
    }
    let req = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nTransfer-Encoding: gzip\r\n\r\n");
    assert_eq!(raw_status(port, req.as_bytes()), 501);
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{{}}"
    );
    assert_eq!(raw_status(port, req.as_bytes()), 400);

    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {TOKEN}\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
        ping.len()
    );
    s.write_all(head.as_bytes()).unwrap();
    let mut first = [0u8; 25];
    s.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"HTTP/1.1 100 Continue\r\n\r\n");
    s.write_all(ping.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    assert!(raw.starts_with("HTTP/1.1 200"), "{raw}");
    assert!(raw.contains("\"id\":7"), "{raw}");
    handle.shutdown();
}

/// Regression: the HTTP transport ignored `MCP-Protocol-Version` and accepted batches from clients
/// on 2025-06-18 or later, which removed them.
#[test]
fn protocol_version_header_and_batches() {
    let handle = serve_http(server(), "127.0.0.1:0", TOKEN).unwrap();
    let port = handle.port();
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string();
    let r = send(port, "POST", "/mcp", &[("MCP-Protocol-Version", "1999-01-01")], &ping);
    assert_eq!(r.status, 400);
    assert_eq!(send(port, "POST", "/mcp", &[("MCP-Protocol-Version", "2025-06-18")], &ping).status, 200);

    let batch = json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }, { "jsonrpc": "2.0", "id": 2, "method": "ping" }]).to_string();
    let r = send(port, "POST", "/mcp", &[("MCP-Protocol-Version", "2025-06-18")], &batch);
    assert_eq!(r.json()["error"]["code"], -32600, "{}", r.body);
    // Without the header the client is on 2025-03-26, where batches still exist.
    let r = send(port, "POST", "/mcp", &[], &batch);
    assert_eq!(r.json().as_array().map(Vec::len), Some(2), "{}", r.body);
    handle.shutdown();
}

/// Regression: when the app could not be reached, the bridge answered a single request but left
/// every request in a batch line unanswered.
#[test]
fn attach_bridge_answers_each_request_of_a_failed_batch() {
    use plancraft_mcp::attach::run_attach_with;
    use plancraft_mcp::session::SessionInfo;

    let gone = || Ok(SessionInfo { url: "http://127.0.0.1:1/mcp".into(), token: TOKEN.into(), pid: 1 });
    let batch = json!([
        { "jsonrpc": "2.0", "id": 1, "method": "ping" },
        { "jsonrpc": "2.0", "method": "notifications/initialized" },
        { "jsonrpc": "2.0", "id": "b", "method": "ping" }
    ])
    .to_string();
    let mut out = Vec::new();
    run_attach_with(std::io::Cursor::new(batch), &mut out, &gone).unwrap();
    let reply: Value = serde_json::from_slice(&out).unwrap();
    let ids: Vec<&Value> = reply.as_array().unwrap().iter().map(|r| &r["id"]).collect();
    assert_eq!(ids, [&json!(1), &json!("b")]);
    assert!(reply[0]["error"]["message"].as_str().unwrap().contains("not reachable"));
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Streamable HTTP transport over a real socket.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use plancraft_core::workspace::Workspace;
use plancraft_mcp::{McpServer, serve_http};
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
    let session = r.header("Mcp-Session-Id").expect("session id").to_string();
    assert!(!session.is_empty());
    let v = r.json();
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");

    // Notifications are accepted with 202 and no body.
    let r = send(
        port,
        "POST",
        "/mcp",
        &[("Mcp-Session-Id", &session)],
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
    );
    assert_eq!(r.status, 202);

    // Tool call through HTTP, also at "/".
    let call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "new_project", "arguments": { "name": "Over HTTP", "start": "2026-10-19" } } }).to_string();
    let r = send(port, "POST", "/", &[("Mcp-Session-Id", &session), ("Origin", "http://localhost:5173")], &call);
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
    // DELETE ends a session.
    let r = send(port, "DELETE", "/mcp", &[("Mcp-Session-Id", &session)], "");
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

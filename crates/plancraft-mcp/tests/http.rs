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
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("bad JSON body {:?}: {e}", self.body))
    }
}

fn send(
    port: u16,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> HttpReply {
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
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    HttpReply {
        status,
        headers,
        body: body.to_string(),
    }
}

#[test]
fn http_transport() {
    let server = Arc::new(McpServer::new(Workspace::default().shared()));
    let handle = serve_http(server, "127.0.0.1:0").unwrap();
    let port = handle.port();
    assert_ne!(port, 0);
    assert!(handle.url().ends_with("/mcp"));

    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } }).to_string();
    let r = send(port, "POST", "/mcp", &[], &init);
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.header("Content-Type")
            .unwrap()
            .starts_with("application/json")
    );
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
    let r = send(
        port,
        "POST",
        "/",
        &[
            ("Mcp-Session-Id", &session),
            ("Origin", "http://localhost:5173"),
        ],
        &call,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.json()["result"]["structuredContent"]["project"]["name"],
        "Over HTTP"
    );
    assert!(r.header("Mcp-Session-Id").is_none());

    // DNS-rebinding protection.
    let r = send(
        port,
        "POST",
        "/mcp",
        &[("Origin", "http://evil.example.com")],
        &init,
    );
    assert_eq!(r.status, 403);
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
    s.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let _ = s.write_all(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}");
    let mut buf = [0u8; 16];
    !matches!(s.read(&mut buf), Ok(n) if n > 0)
}

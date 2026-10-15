//! MCP "Streamable HTTP" transport (JSON responses only) on top of `tiny_http`.
//!
//! * `POST /mcp` (or `/`) with a JSON-RPC message or batch → `200 application/json`,
//!   or `202 Accepted` when the body only contained notifications/responses.
//! * `GET /mcp` → `405` (no server-initiated stream).
//! * `DELETE /mcp` → `200` (session end; sessions are stateless).
//! * An `Mcp-Session-Id` header is issued on `initialize`.
//!
//! Requests carrying an `Origin` header that is not a localhost origin are
//! rejected (DNS-rebinding protection); when bound to a loopback address the
//! `Host` header must name a loopback host as well.

use std::io::{self, Read};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::protocol::{INVALID_REQUEST, RpcError, error_response, parse_error};
use crate::server::McpServer;

/// Maximum accepted request body.
const MAX_BODY: u64 = 32 * 1024 * 1024;

/// A running HTTP server. Dropping it (or calling [`HttpHandle::shutdown`]) stops it.
pub struct HttpHandle {
    addr: SocketAddr,
    http: Arc<Server>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for HttpHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpHandle").field("addr", &self.addr).finish()
    }
}

impl HttpHandle {
    /// The bound socket address (useful when binding to port 0).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The MCP endpoint URL, e.g. `http://127.0.0.1:7878/mcp`.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// Whether the server thread is still running.
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Stops the server and waits for the accept loop to exit.
    pub fn shutdown(mut self) {
        self.stop_now();
    }

    fn stop_now(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.http.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for HttpHandle {
    fn drop(&mut self) {
        self.stop_now();
    }
}

/// Starts serving MCP over HTTP on `addr` (e.g. `"127.0.0.1:7878"`, or port 0
/// for an ephemeral port) on a background thread.
pub fn serve_http(server: Arc<McpServer>, addr: &str) -> io::Result<HttpHandle> {
    let http = Server::http(addr).map_err(|e| io::Error::new(io::ErrorKind::AddrNotAvailable, format!("cannot listen on {addr}: {e}")))?;
    let bound = http.server_addr().to_ip().ok_or_else(|| io::Error::other("server is not bound to an IP address"))?;
    let http = Arc::new(http);
    let stop = Arc::new(AtomicBool::new(false));
    let loopback = bound.ip().is_loopback();
    let thread = {
        let http = Arc::clone(&http);
        let stop = Arc::clone(&stop);
        std::thread::Builder::new().name("plancraft-mcp-http".into()).spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match http.recv_timeout(Duration::from_millis(250)) {
                    Ok(Some(req)) => {
                        let server = Arc::clone(&server);
                        let spawned =
                            std::thread::Builder::new().name("plancraft-mcp-request".into()).spawn(move || handle(&server, req, loopback));
                        if let Err(e) = spawned {
                            eprintln!("plancraft-mcp: cannot spawn request thread: {e}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if !stop.load(Ordering::SeqCst) {
                            eprintln!("plancraft-mcp: HTTP accept error: {e}");
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            }
        })?
    };
    Ok(HttpHandle { addr: bound, http, stop, thread: Some(thread) })
}

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    // Strip a port, keeping bracketed IPv6 literals intact.
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.rsplit_once(':').map(|(n, port)| if port.chars().all(|c| c.is_ascii_digit()) { n } else { h.as_str() }).unwrap_or(&h)
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1") || name.ends_with(".localhost") || name.starts_with("127.")
}

/// Whether an `Origin` header value is a localhost origin.
pub fn is_local_origin(origin: &str) -> bool {
    let o = origin.trim();
    let Some((scheme, rest)) = o.split_once("://") else {
        return false;
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return false;
    }
    let authority = rest.split('/').next().unwrap_or("");
    let authority = authority.rsplit_once('@').map(|(_, a)| a).unwrap_or(authority);
    is_loopback_host(authority)
}

fn h(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn json_response(status: u16, body: &Value) -> Response<io::Cursor<Vec<u8>>> {
    Response::from_data(serde_json::to_vec(body).unwrap_or_default())
        .with_status_code(StatusCode(status))
        .with_header(h("Content-Type", "application/json"))
}

fn text_response(status: u16, text: &str) -> Response<io::Cursor<Vec<u8>>> {
    Response::from_string(text).with_status_code(StatusCode(status)).with_header(h("Content-Type", "text/plain; charset=utf-8"))
}

fn contains_initialize(v: &Value) -> bool {
    match v {
        Value::Array(items) => items.iter().any(contains_initialize),
        Value::Object(m) => m.get("method").and_then(Value::as_str) == Some("initialize") && m.get("id").is_some(),
        _ => false,
    }
}

fn handle(server: &McpServer, mut req: Request, loopback: bool) {
    let path = req.url().split('?').next().unwrap_or("").trim_end_matches('/').to_string();
    let response = route(server, &mut req, &path, loopback);
    if let Err(e) = req.respond(response) {
        eprintln!("plancraft-mcp: failed to send HTTP response: {e}");
    }
}

fn route(server: &McpServer, req: &mut Request, path: &str, loopback: bool) -> Response<io::Cursor<Vec<u8>>> {
    if let Some(origin) = header(req, "Origin")
        && !is_local_origin(origin)
    {
        return text_response(403, "Forbidden: non-local Origin");
    }
    if loopback
        && let Some(host) = header(req, "Host")
        && !is_loopback_host(host)
    {
        return text_response(403, "Forbidden: invalid Host header");
    }
    if !(path.is_empty() || path == "/mcp") {
        return text_response(404, "Not found: the MCP endpoint is /mcp");
    }
    match req.method() {
        Method::Post => {}
        Method::Delete => return text_response(200, ""),
        Method::Get => {
            return text_response(405, "Method Not Allowed: this server does not offer an SSE stream; use POST")
                .with_header(h("Allow", "POST, DELETE"));
        }
        _ => {
            return text_response(405, "Method Not Allowed").with_header(h("Allow", "POST, DELETE"));
        }
    }
    let mut body = Vec::new();
    if let Err(e) = req.as_reader().take(MAX_BODY + 1).read_to_end(&mut body) {
        return json_response(400, &parse_error(format!("cannot read body: {e}")));
    }
    if body.len() as u64 > MAX_BODY {
        return json_response(413, &error_response(Value::Null, &RpcError::new(INVALID_REQUEST, "request body too large")));
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, &parse_error(e)),
    };
    let init = contains_initialize(&msg);
    match server.handle_message(msg) {
        None => Response::from_data(Vec::new()).with_status_code(StatusCode(202)),
        Some(resp) => {
            let mut r = json_response(200, &resp);
            if init {
                r.add_header(h("Mcp-Session-Id", &uuid::Uuid::new_v4().to_string()));
            }
            r
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert!(is_local_origin("http://localhost:3000"));
        assert!(is_local_origin("http://127.0.0.1"));
        assert!(is_local_origin("https://[::1]:8080"));
        assert!(!is_local_origin("http://evil.com"));
        assert!(!is_local_origin("http://localhost.evil.com"));
        assert!(!is_local_origin("null"));
        assert!(is_loopback_host("127.0.0.1:7878"));
        assert!(is_loopback_host("localhost"));
        assert!(!is_loopback_host("attacker.example:7878"));
    }
}

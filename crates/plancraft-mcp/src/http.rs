//! MCP "Streamable HTTP" transport (JSON responses only): a small HTTP/1.1 server on `std::net`.
//!
//! * `POST /mcp` (or `/`) with a JSON-RPC message → `200 application/json`, or `202 Accepted`
//!   when the body only contained notifications/responses.
//! * `GET /mcp` → `405` (no server-initiated stream).
//! * `DELETE /mcp` → `200` (sessions are stateless: no `Mcp-Session-Id` is issued).
//!
//! Requests carrying an `Origin` header that is not a localhost origin are rejected
//! (DNS-rebinding protection); when bound to a loopback address the `Host` header must name a
//! loopback host as well. Every request must carry `Authorization: Bearer <token>` with the
//! server's per-session token (see [`crate::session`]); others get `401 Unauthorized`.
//!
//! The server is written to hold up against a hostile local client:
//! * at most [`MAX_CONNECTIONS`] connections may be sending their request line and headers at
//!   once, before the token is checked (others get `503` at once), so clients without the token
//!   can't tie up the server; requests that passed the token check have their own, larger limit
//!   ([`MAX_REQUESTS`]), so parallel tool calls from several agents aren't refused;
//! * the request line and headers are checked (origin, host, token) *before* the body is read,
//!   so an unauthenticated client can't make the server read a large body;
//! * each read waits at most [`READ_TIMEOUT`], the request line and headers must arrive within
//!   [`HEAD_DEADLINE`] and the whole request within [`REQUEST_DEADLINE`], so a client sending one
//!   byte at a time is cut off (and cannot hold a pre-authentication slot for long);
//! * headers are limited to [`MAX_HEAD`] bytes and the body to [`MAX_BODY`] bytes.
//!
//! Each connection carries one request and is closed after the reply (`Connection: close`).

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::protocol::{INVALID_REQUEST, RpcError, SUPPORTED_PROTOCOL_VERSIONS, error_response, parse_error};
use crate::server::McpServer;

/// Maximum accepted request body.
pub const MAX_BODY: usize = 32 * 1024 * 1024;
/// Maximum size of the request line and headers together.
pub const MAX_HEAD: usize = 64 * 1024;
/// Maximum number of header lines.
const MAX_HEADERS: usize = 100;
/// Connections whose headers are still being read and checked (before the token is accepted);
/// more get `503 Service Unavailable`.
pub const MAX_CONNECTIONS: usize = 32;
/// Requests that passed the token check being served at the same time; more get `503`.
pub const MAX_REQUESTS: usize = 256;
/// After a reply, how long (and how much) the server reads what the client still sends before
/// closing, so the client sees the reply instead of a reset connection.
const DRAIN_TIME: Duration = Duration::from_secs(1);
const DRAIN_BYTES: u64 = 64 * 1024;
/// Longest wait for any one read from a client.
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Time a client has to send the whole request (headers and body).
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(60);
/// Time a client has to send the request line and headers: the pre-authentication slot it holds
/// meanwhile is one of few, so a client dripping header bytes must not keep it long.
pub const HEAD_DEADLINE: Duration = Duration::from_secs(10);
/// Longest wait for a client to take the reply.
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

/// A running HTTP server. Dropping it (or calling [`HttpHandle::shutdown`]) stops it.
pub struct HttpHandle {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    token: Arc<str>,
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

    /// The bearer token clients must send (`Authorization: Bearer <token>`).
    pub fn token(&self) -> &str {
        &self.token
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
        // The accept loop blocks in `accept`; a connection of our own wakes it to see the flag.
        let woke = TcpStream::connect_timeout(&wake_addr(self.addr), Duration::from_secs(1)).is_ok();
        if let Some(t) = self.thread.take()
            && (woke || t.is_finished())
        {
            let _ = t.join();
        }
    }
}

impl Drop for HttpHandle {
    fn drop(&mut self) {
        self.stop_now();
    }
}

/// Where to connect to reach a server bound to `addr` (a wildcard address is reached on loopback).
fn wake_addr(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port()),
        IpAddr::V6(ip) if ip.is_unspecified() => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port()),
        _ => addr,
    }
}

/// The limits that protect the server from slow or numerous clients ([`HttpLimits::default`]
/// gives the documented ones; tests use shorter times).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpLimits {
    /// Connections still sending headers (not yet past the token check); more get `503`.
    pub max_connections: usize,
    /// Authenticated requests served at the same time; more get `503`.
    pub max_requests: usize,
    /// Longest wait for any one read from a client.
    pub read_timeout: Duration,
    /// Time a client has to send the whole request (headers and body).
    pub request_deadline: Duration,
    /// Time a client has to send the request line and headers.
    pub head_deadline: Duration,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            max_requests: MAX_REQUESTS,
            read_timeout: READ_TIMEOUT,
            request_deadline: REQUEST_DEADLINE,
            head_deadline: HEAD_DEADLINE,
        }
    }
}

/// Counts a connection (or an authenticated request) as active until dropped.
struct Active(Arc<AtomicUsize>);

impl Active {
    /// Takes a slot of `counter` unless `max` are taken already.
    fn take(counter: &Arc<AtomicUsize>, max: usize) -> Option<Active> {
        if counter.fetch_add(1, Ordering::SeqCst) >= max {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Active(Arc::clone(counter)))
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Starts serving MCP over HTTP on `addr` (e.g. `"127.0.0.1:7878"`, or port 0
/// for an ephemeral port) on a background thread.
///
/// Clients must authenticate with `token` (make one with [`crate::session::new_token`]).
pub fn serve_http(server: Arc<McpServer>, addr: &str, token: &str) -> io::Result<HttpHandle> {
    serve_http_with(server, addr, token, HttpLimits::default())
}

/// [`serve_http`] with other [`HttpLimits`].
pub fn serve_http_with(server: Arc<McpServer>, addr: &str, token: &str, limits: HttpLimits) -> io::Result<HttpHandle> {
    if token.len() < 32 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the bearer token must be at least 32 characters"));
    }
    let token: Arc<str> = Arc::from(token);
    let listener = TcpListener::bind(addr).map_err(|e| io::Error::new(e.kind(), format!("cannot listen on {addr}: {e}")))?;
    let bound = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let loopback = bound.ip().is_loopback();
    let thread = {
        let stop = Arc::clone(&stop);
        let token = Arc::clone(&token);
        let pending = Arc::new(AtomicUsize::new(0));
        let busy = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new().name("plancraft-mcp-http".into()).spawn(move || {
            for conn in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let stream = match conn {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("plancraft-mcp: HTTP accept error: {e}");
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                };
                let Some(guard) = Active::take(&pending, limits.max_connections) else {
                    refuse_busy(stream);
                    continue;
                };
                let server = Arc::clone(&server);
                let token = Arc::clone(&token);
                let busy = Arc::clone(&busy);
                // If the thread can't start, the closure (and the guard with it) is dropped.
                let spawned = std::thread::Builder::new().name("plancraft-mcp-request".into()).spawn(move || {
                    let slots = Slots { pending: Some(guard), busy, request: None };
                    serve_connection(&server, stream, loopback, &token, limits, slots);
                });
                if let Err(e) = spawned {
                    eprintln!("plancraft-mcp: cannot start a request thread: {e}");
                }
            }
        })?
    };
    Ok(HttpHandle { addr: bound, stop, thread: Some(thread), token })
}

/// Answers `503` without waiting on the client (the reply fits in the socket buffer).
fn refuse_busy(mut stream: TcpStream) {
    let _ = stream.set_nonblocking(true);
    let reply = Reply::text(503, "Service Unavailable: too many connections").header("Retry-After", "1");
    let _ = stream.write_all(&reply.to_bytes());
    let _ = stream.shutdown(Shutdown::Both);
}

/// An HTTP reply.
#[derive(Debug)]
struct Reply {
    status: u16,
    content_type: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn text(status: u16, text: &str) -> Self {
        Self { status, content_type: "text/plain; charset=utf-8", headers: Vec::new(), body: text.as_bytes().to_vec() }
    }

    fn json(status: u16, body: &Value) -> Self {
        Self { status, content_type: "application/json", headers: Vec::new(), body: serde_json::to_vec(body).unwrap_or_default() }
    }

    fn header(mut self, name: &'static str, value: &str) -> Self {
        // Values here are constants or numbers; a CR or LF would split the header, so drop them.
        self.headers.push((name, value.chars().filter(|c| !matches!(c, '\r' | '\n')).collect()));
        self
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
            self.status,
            reason(self.status),
            self.content_type,
            self.body.len()
        );
        for (name, value) in &self.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// The connection's place in the server's limits: a pre-authentication slot until the token is
/// accepted, then a request slot.
struct Slots {
    pending: Option<Active>,
    busy: Arc<AtomicUsize>,
    request: Option<Active>,
}

/// Serves one request on `stream` and closes it.
fn serve_connection(server: &McpServer, mut stream: TcpStream, loopback: bool, token: &str, limits: HttpLimits, mut slots: Slots) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let reply = {
        let deadline = Instant::now().checked_add(limits.request_deadline).unwrap_or_else(Instant::now);
        let mut conn = Conn { stream: &mut stream, buf: Vec::new(), deadline, read_timeout: limits.read_timeout };
        match read_and_route(server, &mut conn, loopback, token, limits, &mut slots) {
            Ok(r) | Err(r) => r,
        }
    };
    if reply.status == 0 {
        return; // the client went away
    }
    if stream.write_all(&reply.to_bytes()).and_then(|()| stream.flush()).is_err() {
        return;
    }
    // Answered: its slots go back now, not after the drain below.
    drop(slots);
    // A request refused before its body was read leaves the body unread; closing with unread
    // data can reset the connection before the client sees the reply, so drain a little first,
    // for at most DRAIN_TIME in all (a client dripping bytes can't keep its slot).
    let _ = stream.shutdown(Shutdown::Write);
    let Some(until) = Instant::now().checked_add(DRAIN_TIME) else { return };
    let mut left = DRAIN_BYTES;
    let mut chunk = [0u8; 8 * 1024];
    while left > 0 {
        let Some(wait) = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { break };
        if stream.set_read_timeout(Some(wait)).is_err() {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(u64::try_from(n).unwrap_or(u64::MAX)),
        }
    }
}

/// A client connection being read, with everything received so far in `buf`.
struct Conn<'a> {
    stream: &'a mut TcpStream,
    buf: Vec<u8>,
    deadline: Instant,
    read_timeout: Duration,
}

/// "The client went away": no reply is sent.
fn gone() -> Reply {
    Reply { status: 0, content_type: "", headers: Vec::new(), body: Vec::new() }
}

impl Conn<'_> {
    /// Reads more bytes into `buf`, waiting at most the read timeout and never past the deadline.
    fn fill(&mut self) -> Result<(), Reply> {
        let timeout = || Reply::text(408, "Request Timeout: the request did not arrive in time");
        loop {
            let left = self.deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(timeout)?;
            self.stream.set_read_timeout(Some(left.min(self.read_timeout))).map_err(|_| gone())?;
            let mut chunk = [0u8; 16 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(gone()),
                Ok(n) => {
                    self.buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Err(timeout()),
                Err(_) => return Err(gone()),
            }
        }
    }

    /// Reads up to the end of the headers; returns where the body starts in `buf`.
    fn read_head(&mut self) -> Result<usize, Reply> {
        let mut searched = 0;
        loop {
            if let Some(i) = find(self.buf.get(searched..).unwrap_or_default(), b"\r\n\r\n") {
                let end = searched.saturating_add(i).saturating_add(4);
                if end > MAX_HEAD {
                    return Err(Reply::text(431, "Request Header Fields Too Large"));
                }
                return Ok(end);
            }
            if self.buf.len() > MAX_HEAD {
                return Err(Reply::text(431, "Request Header Fields Too Large"));
            }
            searched = self.buf.len().saturating_sub(3);
            self.fill()?;
        }
    }

    /// Makes sure `buf` holds at least `n` bytes.
    fn need(&mut self, n: usize) -> Result<(), Reply> {
        while self.buf.len() < n {
            self.fill()?;
        }
        Ok(())
    }

    /// Reads a CRLF-terminated line starting at `at` (at most `max` bytes); returns the line and
    /// the position after its CRLF.
    fn line(&mut self, at: usize, max: usize) -> Result<(String, usize), Reply> {
        loop {
            if let Some(i) = find(self.buf.get(at..).unwrap_or_default(), b"\r\n") {
                let text = self.buf.get(at..at.saturating_add(i)).unwrap_or_default();
                return Ok((String::from_utf8_lossy(text).into_owned(), at.saturating_add(i).saturating_add(2)));
            }
            if self.buf.len().saturating_sub(at) > max {
                return Err(Reply::text(400, "Bad Request: malformed chunked body"));
            }
            self.fill()?;
        }
    }

    /// Reads a body of `len` bytes starting at `start`.
    fn body_with_length(&mut self, start: usize, len: usize) -> Result<Vec<u8>, Reply> {
        let end = start.checked_add(len).ok_or_else(too_large)?;
        self.need(end)?;
        Ok(self.buf.get(start..end).unwrap_or_default().to_vec())
    }

    /// Reads a chunked body starting at `start` (at most [`MAX_BODY`] bytes of data).
    fn body_chunked(&mut self, start: usize) -> Result<Vec<u8>, Reply> {
        let bad = || Reply::text(400, "Bad Request: malformed chunked body");
        let mut body = Vec::new();
        let mut at = start;
        loop {
            let (size_line, next) = self.line(at, 1024)?;
            let size_text = size_line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_text, 16).map_err(|_| bad())?;
            at = next;
            if size == 0 {
                // Trailers (ignored) up to an empty line.
                let mut trailer_bytes = 0usize;
                loop {
                    let (line, next) = self.line(at, MAX_HEAD)?;
                    trailer_bytes = trailer_bytes.saturating_add(next.saturating_sub(at));
                    at = next;
                    if line.is_empty() {
                        return Ok(body);
                    }
                    if trailer_bytes > MAX_HEAD {
                        return Err(Reply::text(431, "Request Header Fields Too Large"));
                    }
                }
            }
            if body.len().saturating_add(size) > MAX_BODY {
                return Err(too_large());
            }
            let end = at.checked_add(size).ok_or_else(too_large)?;
            let after = end.checked_add(2).ok_or_else(too_large)?;
            self.need(after)?;
            body.extend_from_slice(self.buf.get(at..end).unwrap_or_default());
            if self.buf.get(end..after) != Some(b"\r\n".as_slice()) {
                return Err(bad());
            }
            // Drop what has been consumed so a long body isn't kept twice.
            self.buf.drain(..after);
            at = 0;
        }
    }
}

fn too_large() -> Reply {
    Reply::json(413, &error_response(Value::Null, &RpcError::new(INVALID_REQUEST, "request body too large")))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A parsed request line and headers.
struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn all(&self, name: &str) -> impl Iterator<Item = &str> {
        self.headers.iter().filter(move |(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn parse_head(raw: &[u8]) -> Result<Head, Reply> {
    let bad = |what: &str| Reply::text(400, &format!("Bad Request: {what}"));
    let text = std::str::from_utf8(raw).map_err(|_| bad("headers are not UTF-8"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(bad("malformed request line"));
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(bad("malformed request line"));
    }
    let mut headers = Vec::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(|| bad("malformed header"))?;
        // A name with spaces, or a folded continuation line, is refused (request smuggling).
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(bad("malformed header"));
        }
        if headers.len() >= MAX_HEADERS {
            return Err(Reply::text(431, "Request Header Fields Too Large"));
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }
    let path = target.split('?').next().unwrap_or("").trim_end_matches('/').to_string();
    Ok(Head { method: method.to_string(), path, headers })
}

/// Whether `host` (a `Host` header value or a URL authority) names this machine: `localhost`,
/// a `.localhost` name, or a loopback IP address. A name that merely starts with `127.`
/// (`127.evil.example`) is not one.
fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    // Strip a port, keeping bracketed IPv6 literals intact.
    let name = if let Some(rest) = h.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inside, after)) if after.is_empty() || after.strip_prefix(':').is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit())) => {
                inside
            }
            _ => return false,
        }
    } else {
        h.rsplit_once(':').map(|(n, port)| if port.bytes().all(|b| b.is_ascii_digit()) { n } else { h.as_str() }).unwrap_or(&h)
    };
    name == "localhost" || name.ends_with(".localhost") || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
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

/// Whether the request carries `Authorization: Bearer <token>`.
fn authorized(head: &Head, token: &str) -> bool {
    head.header("Authorization")
        .and_then(|v| v.trim().split_once(' '))
        .is_some_and(|(scheme, value)| scheme.eq_ignore_ascii_case("bearer") && crate::session::tokens_match(value.trim(), token))
}

fn read_and_route(
    server: &McpServer,
    conn: &mut Conn<'_>,
    loopback: bool,
    token: &str,
    limits: HttpLimits,
    slots: &mut Slots,
) -> Result<Reply, Reply> {
    let full = conn.deadline;
    conn.deadline = Instant::now().checked_add(limits.head_deadline).map_or(full, |d| d.min(full));
    let body_start = conn.read_head()?;
    conn.deadline = full;
    let head = parse_head(conn.buf.get(..body_start.saturating_sub(4)).unwrap_or_default())?;

    // Everything that can refuse the request is checked before the body is read.
    if let Some(origin) = head.header("Origin")
        && !is_local_origin(origin)
    {
        return Ok(Reply::text(403, "Forbidden: non-local Origin"));
    }
    if loopback && !head.header("Host").is_some_and(is_loopback_host) {
        return Ok(Reply::text(403, "Forbidden: invalid Host header"));
    }
    if !authorized(&head, token) {
        return Ok(Reply::text(401, "Unauthorized: send the PlanCraft session token as 'Authorization: Bearer <token>'")
            .header("WWW-Authenticate", "Bearer realm=\"plancraft\""));
    }
    // Past the token check: the reply proves this server knows the token, when asked.
    let proof = head
        .header(crate::session::NONCE_HEADER)
        .map(str::trim)
        .filter(|n| crate::session::valid_nonce(n))
        .map(|n| crate::session::proof(token, n));
    let proved = |r: Reply| match &proof {
        Some(p) => r.header(crate::session::PROOF_HEADER, p),
        None => r,
    };
    authorized_route(server, conn, &head, body_start, limits, slots).map(proved).map_err(proved)
}

/// [`read_and_route`] past the token check.
fn authorized_route(
    server: &McpServer,
    conn: &mut Conn<'_>,
    head: &Head,
    body_start: usize,
    limits: HttpLimits,
    slots: &mut Slots,
) -> Result<Reply, Reply> {
    // Free the pre-authentication slot and take a request slot.
    slots.pending = None;
    match Active::take(&slots.busy, limits.max_requests) {
        Some(slot) => slots.request = Some(slot),
        None => return Ok(Reply::text(503, "Service Unavailable: too many requests at once").header("Retry-After", "1")),
    }
    if !(head.path.is_empty() || head.path == "/mcp") {
        return Ok(Reply::text(404, "Not found: the MCP endpoint is /mcp"));
    }
    match head.method.as_str() {
        "POST" => {}
        "DELETE" => return Ok(Reply::text(200, "")),
        "GET" => {
            return Ok(
                Reply::text(405, "Method Not Allowed: this server does not offer an SSE stream; use POST").header("Allow", "POST, DELETE")
            );
        }
        _ => return Ok(Reply::text(405, "Method Not Allowed").header("Allow", "POST, DELETE")),
    }
    // The spec: an unsupported MCP-Protocol-Version gets 400; without the header the client is
    // taken to use 2025-03-26.
    let version = head.header("MCP-Protocol-Version");
    if let Some(v) = version
        && !SUPPORTED_PROTOCOL_VERSIONS.contains(&v)
    {
        return Ok(Reply::text(400, &format!("Bad Request: unsupported MCP-Protocol-Version '{v}'")));
    }

    let lengths: Vec<&str> = head.all("Content-Length").collect();
    let body = if let Some(te) = head.header("Transfer-Encoding") {
        if !te.eq_ignore_ascii_case("chunked") || !lengths.is_empty() {
            return Ok(Reply::text(501, "Not Implemented: only a chunked or Content-Length body is accepted"));
        }
        continue_if_asked(conn, head)?;
        conn.body_chunked(body_start)?
    } else {
        let len = match lengths.as_slice() {
            [] => 0,
            [one, rest @ ..] if rest.iter().all(|r| r == one) => {
                one.parse::<usize>().map_err(|_| Reply::text(400, "Bad Request: invalid Content-Length"))?
            }
            _ => return Ok(Reply::text(400, "Bad Request: conflicting Content-Length headers")),
        };
        if len > MAX_BODY {
            return Ok(too_large());
        }
        continue_if_asked(conn, head)?;
        conn.body_with_length(body_start, len)?
    };

    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Ok(Reply::json(400, &parse_error(e))),
    };
    drop(body);
    Ok(match server.handle_message_as(msg, version) {
        None => Reply { status: 202, content_type: "application/json", headers: Vec::new(), body: Vec::new() },
        Some(resp) => Reply::json(200, &resp),
    })
}

/// Answers `Expect: 100-continue`, so a client waiting for it sends the body.
fn continue_if_asked(conn: &mut Conn<'_>, head: &Head) -> Result<(), Reply> {
    if head.header("Expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
        conn.stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").map_err(|_| gone())?;
    }
    Ok(())
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
        assert!(is_loopback_host("127.0.0.2"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("[::1]:7878"));
        assert!(!is_loopback_host("attacker.example:7878"));
    }

    /// Regression: the DNS-rebinding check took any name starting with "127." as loopback, so a
    /// page on `127.evil.example` (which its owner can point at 127.0.0.1) passed the Host and
    /// Origin checks.
    #[test]
    fn names_that_only_look_like_loopback_are_refused() {
        for host in ["127.evil.example", "127.0.0.1.evil.example", "127.evil.example:7878", "[::1]evil", "[::1]:x", "127.1"] {
            assert!(!is_loopback_host(host), "{host}");
        }
        assert!(!is_local_origin("http://127.evil.example:7878"));
    }

    #[test]
    fn heads_parse_strictly() {
        let h = parse_head(b"POST /mcp/?x=1 HTTP/1.1\r\nHost: localhost\r\nX-A:  b \r\n").unwrap();
        assert_eq!((h.method.as_str(), h.path.as_str(), h.header("x-a")), ("POST", "/mcp", Some("b")));
        for bad in [
            &b"POST /mcp\r\n"[..],
            b"POST /mcp HTTP/2\r\n",
            b"post /mcp HTTP/1.1\r\n",
            b"POST /mcp HTTP/1.1\r\nBad Name: x\r\n",
            b"POST / HTTP/1.1\r\n folded\r\n",
        ] {
            assert!(parse_head(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }
}

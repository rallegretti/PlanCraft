//! Bearer tokens and the session file of the desktop app's MCP server.
//!
//! Every HTTP server gets a fresh random token when it starts; requests without
//! `Authorization: Bearer <token>` are refused. The desktop app publishes its URL and token in
//! a session file only its owner can read (0600 inside a 0700 directory on Unix), so a local
//! MCP client started as `plancraft mcp --attach` can connect across app restarts without
//! anyone copying the token around.

use std::io;
use std::path::PathBuf;

use plancraft_core::io::{Privacy, create_private_dir, write_atomic};
use serde::{Deserialize, Serialize};

/// Where the running desktop app's MCP server is and how to authenticate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The MCP endpoint, e.g. `http://127.0.0.1:7878/mcp`.
    pub url: String,
    pub token: String,
    /// Process id of the app that wrote the file.
    pub pid: u32,
}

/// A new random bearer token (244 random bits from the operating system's generator, as hex).
pub fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Compares two secrets in time independent of where they differ.
pub fn tokens_match(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// How `plancraft mcp --attach` talks to the app without ever sending it the token, so a
/// leftover session file can't make the relay hand the token (or trust replies) to whatever
/// program now listens at the old address:
/// - each request carries a fresh random [`NONCE_HEADER`] and, in [`AUTH_HEADER`],
///   [`request_mac`] over that nonce and the body, in place of `Authorization: Bearer`;
/// - the app answers in [`PROOF_HEADER`] with [`reply_mac`] over the nonce, the status and the
///   reply's body.
///
/// Only a holder of the token can make either; the two use different labels, so one can't pass
/// for the other, and the app takes each nonce once, so a request can't be replayed.
pub const NONCE_HEADER: &str = "X-PlanCraft-Nonce";
pub const AUTH_HEADER: &str = "X-PlanCraft-Auth";
pub const PROOF_HEADER: &str = "X-PlanCraft-Proof";

/// HMAC-SHA256 (RFC 2104) of `parts`, joined with NUL bytes, keyed by `key`, as hex.
fn hmac_hex(key: &[u8], parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        for (slot, b) in k.iter_mut().zip(Sha256::digest(key)) {
            *slot = b;
        }
    } else {
        for (slot, b) in k.iter_mut().zip(key) {
            *slot = *b;
        }
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            inner.update([0u8]);
        }
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn body_hash(body: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(body).to_vec()
}

/// What a request signed with the session `token` carries in [`AUTH_HEADER`].
pub fn request_mac(token: &str, nonce: &str, body: &[u8]) -> String {
    hmac_hex(token.as_bytes(), &[b"plancraft-mcp-request", nonce.as_bytes(), &body_hash(body)])
}

/// What the app's reply to a request with `nonce` carries in [`PROOF_HEADER`].
pub fn reply_mac(token: &str, nonce: &str, status: u16, body: &[u8]) -> String {
    hmac_hex(token.as_bytes(), &[b"plancraft-mcp-reply", nonce.as_bytes(), status.to_string().as_bytes(), &body_hash(body)])
}

/// Nonces of signed requests already taken (this process): a captured request can't be sent
/// again. The oldest are forgotten past [`MAX_NONCES_KEPT`] (each is 128 random bits, so a
/// forgotten one coming back is a replay of a request many thousands old).
const MAX_NONCES_KEPT: usize = 65_536;

/// Takes `nonce` for one signed request: `false` if it was taken before.
pub fn take_nonce(nonce: &str) -> bool {
    use std::collections::{HashSet, VecDeque};
    use std::sync::{Mutex, OnceLock, PoisonError};
    static SEEN: OnceLock<Mutex<(HashSet<String>, VecDeque<String>)>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner);
    let (set, order) = &mut *seen;
    if !set.insert(nonce.to_string()) {
        return false;
    }
    order.push_back(nonce.to_string());
    while order.len() > MAX_NONCES_KEPT {
        if let Some(old) = order.pop_front() {
            set.remove(&old);
        }
    }
    true
}

/// A nonce as the relay sends it: 16 to 64 letters, digits or dashes (anything else is refused).
pub fn valid_nonce(nonce: &str) -> bool {
    (16..=64).contains(&nonce.len()) && nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn path() -> io::Result<PathBuf> {
    plancraft_core::paths::mcp_session_file().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home or config directory"))
}

/// Publishes the session for `plancraft mcp --attach` (owner-only file).
pub fn publish(info: &SessionInfo) -> io::Result<PathBuf> {
    let path = path()?;
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let text = serde_json::to_vec_pretty(info).map_err(io::Error::other)?;
    write_atomic(&path, &text, Privacy::OwnerOnly)?;
    Ok(path)
}

/// Writes the bearer token of a standalone `plancraft-mcp --http` server to an owner-only file
/// (one per port) and returns its path. The token is never printed or logged, so it can't end
/// up in a terminal's scrollback, a log collector or a screen share.
pub fn publish_http_token(addr: std::net::SocketAddr, token: &str) -> io::Result<PathBuf> {
    let dir = path()?.parent().map(PathBuf::from).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
    create_private_dir(&dir)?;
    let file = dir.join(format!("mcp-http-{}.token", addr.port()));
    // The address the server listens on, so a later start can tell whether it still runs. It is
    // written first: another server's sweep never sees this token without its address.
    write_atomic(&file.with_extension("addr"), addr.to_string().as_bytes(), Privacy::OwnerOnly)?;
    write_atomic(&file, token.as_bytes(), Privacy::OwnerOnly)?;
    Ok(file)
}

/// Removes token files left by standalone HTTP servers that are no longer running (one stopped
/// with Ctrl+C has no chance to remove its own): a file whose server's address (stored beside
/// it) no longer accepts connections. Best effort; errors are ignored.
pub fn remove_stale_http_tokens() {
    if let Some(dir) = path().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        remove_stale_http_tokens_in(&dir);
    }
}

/// [`remove_stale_http_tokens`] in `dir`.
pub fn remove_stale_http_tokens_in(dir: &std::path::Path) {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(port) = name
            .to_str()
            .and_then(|n| n.strip_prefix("mcp-http-"))
            .and_then(|n| n.strip_suffix(".token"))
            .and_then(|p| p.parse::<u16>().ok())
        else {
            continue;
        };
        let file = entry.path();
        let addr_file = file.with_extension("addr");
        // Where the server listens: its stored address (a wildcard one is reached on loopback),
        // or for a file without one, the loopback addresses.
        let targets: Vec<SocketAddr> = match std::fs::read_to_string(&addr_file).ok().and_then(|a| a.trim().parse::<SocketAddr>().ok()) {
            Some(a) if a.ip().is_unspecified() => {
                let lo = if a.is_ipv4() { IpAddr::V4(Ipv4Addr::LOCALHOST) } else { IpAddr::V6(Ipv6Addr::LOCALHOST) };
                vec![SocketAddr::new(lo, a.port())]
            }
            Some(a) => vec![a],
            None => vec![SocketAddr::from(([127, 0, 0, 1], port)), SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)],
        };
        let running = targets.iter().any(|a| TcpStream::connect_timeout(a, std::time::Duration::from_millis(300)).is_ok());
        if !running {
            let _ = std::fs::remove_file(&file);
            let _ = std::fs::remove_file(&addr_file);
        }
    }
}

/// Removes the token file of a standalone HTTP server (and its address file) if it still holds
/// `token`.
pub fn withdraw_http_token(file: &std::path::Path, token: &str) {
    if std::fs::read_to_string(file).is_ok_and(|t| tokens_match(t.trim(), token)) {
        let _ = std::fs::remove_file(file);
        let _ = std::fs::remove_file(file.with_extension("addr"));
    }
}

/// Reads the session published by the running desktop app.
pub fn read() -> io::Result<SessionInfo> {
    let path = path()?;
    let text = std::fs::read(&path)?;
    serde_json::from_slice(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))
}

/// Removes the session file if it still describes the session with `token`.
pub fn withdraw(token: &str) {
    if let Ok(info) = read()
        && tokens_match(&info.token, token)
        && let Ok(path) = path()
    {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4231 test cases 1, 2 and 6 (a key longer than a block).
    #[test]
    fn hmac_matches_the_published_test_vectors() {
        assert_eq!(hmac_hex(&[0x0b; 20], &[b"Hi There"]), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        assert_eq!(
            hmac_hex(b"Jefe", &[b"what do ya want for nothing?"]),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hmac_hex(&[0xaa; 131], &[b"Test Using Larger Than Block-Size Key - Hash Key First"]),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn requests_and_replies_are_signed_apart_and_nonces_taken_once() {
        let t = new_token();
        let n = "0123456789abcdef0123";
        assert_ne!(request_mac(&t, n, b"{}"), reply_mac(&t, n, 200, b"{}"));
        assert_ne!(request_mac(&t, n, b"{}"), request_mac(&t, n, b"{ }"), "the body is covered");
        assert_ne!(reply_mac(&t, n, 200, b"{}"), reply_mac(&t, n, 400, b"{}"), "the status is covered");
        assert_ne!(request_mac(&t, n, b"{}"), request_mac(&new_token(), n, b"{}"));
        let fresh = uuid::Uuid::new_v4().simple().to_string();
        assert!(valid_nonce(&fresh));
        assert!(take_nonce(&fresh));
        assert!(!take_nonce(&fresh), "a replay is refused");
        assert!(!valid_nonce("short") && !valid_nonce("bad nonce with spaces"));
    }

    #[test]
    fn tokens_are_long_unique_and_compared_exactly() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(tokens_match(&a, &a.clone()));
        assert!(!tokens_match(&a, &b));
        assert!(!tokens_match(&a, &a[..63]));
        assert!(!tokens_match("", &a));
    }

    /// Regression (review): a server stopped with Ctrl+C left its token file behind. The next
    /// start removes token files whose port nothing listens on any more, and keeps the others.
    #[test]
    fn stale_token_files_are_removed_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let listening = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let live = listening.local_addr().unwrap().port();
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        for name in
            [format!("mcp-http-{live}.token"), format!("mcp-http-{dead}.token"), "mcp-session.json".into(), "mcp-http-x.token".into()]
        {
            std::fs::write(dir.path().join(name), "t").unwrap();
        }
        // A live server on IPv6 loopback (or any address other than 127.0.0.1) keeps its file too:
        // its address is stored beside the token.
        let v6 = std::net::TcpListener::bind("[::1]:0").ok();
        if let Some(l) = &v6 {
            let addr = l.local_addr().unwrap();
            std::fs::write(dir.path().join(format!("mcp-http-{}.token", addr.port())), "t").unwrap();
            std::fs::write(dir.path().join(format!("mcp-http-{}.addr", addr.port())), addr.to_string()).unwrap();
        }
        remove_stale_http_tokens_in(dir.path());
        if let Some(l) = &v6 {
            let port = l.local_addr().unwrap().port();
            assert!(dir.path().join(format!("mcp-http-{port}.token")).exists(), "a server on ::1 keeps its file");
        }
        assert!(dir.path().join(format!("mcp-http-{live}.token")).exists(), "a running server keeps its file");
        assert!(!dir.path().join(format!("mcp-http-{dead}.token")).exists(), "a stopped server's file is removed");
        assert!(dir.path().join("mcp-session.json").exists());
        assert!(dir.path().join("mcp-http-x.token").exists());
        drop(listening);
    }

    /// Regression: `--http` printed the bearer token to stderr. It now goes to an owner-only file.
    #[test]
    fn the_http_token_file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.token");
        write_atomic(&file, b"secret-token", Privacy::OwnerOnly).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        withdraw_http_token(&file, "another-token");
        assert!(file.exists(), "a file holding another server's token is left alone");
        withdraw_http_token(&file, "secret-token");
        assert!(!file.exists());
    }
}

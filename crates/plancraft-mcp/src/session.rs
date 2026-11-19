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

/// The header `plancraft mcp --attach` sends a fresh random value in, and the app answers in
/// [`PROOF_HEADER`] with [`proof`]: only the app holding the session token can, so a leftover
/// session file never makes the relay trust whatever program now listens at its address.
pub const NONCE_HEADER: &str = "X-PlanCraft-Nonce";
pub const PROOF_HEADER: &str = "X-PlanCraft-Proof";

/// The proof of knowing `token` for `nonce` (hex SHA-256 of both; the token itself never travels
/// back).
pub fn proof(token: &str, nonce: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"plancraft-mcp-proof\0");
    h.update(token.as_bytes());
    h.update(b"\0");
    h.update(nonce.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A nonce as the relay sends it: 1 to 64 letters, digits or dashes (anything else is ignored).
pub fn valid_nonce(nonce: &str) -> bool {
    (1..=64).contains(&nonce.len()) && nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
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

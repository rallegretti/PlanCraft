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
pub fn publish_http_token(port: u16, token: &str) -> io::Result<PathBuf> {
    let dir = path()?.parent().map(PathBuf::from).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
    create_private_dir(&dir)?;
    let file = dir.join(format!("mcp-http-{port}.token"));
    write_atomic(&file, token.as_bytes(), Privacy::OwnerOnly)?;
    Ok(file)
}

/// Removes the token file of a standalone HTTP server if it still holds `token`.
pub fn withdraw_http_token(file: &std::path::Path, token: &str) {
    if std::fs::read_to_string(file).is_ok_and(|t| tokens_match(t.trim(), token)) {
        let _ = std::fs::remove_file(file);
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

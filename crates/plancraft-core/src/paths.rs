//! Per-user directories shared by the desktop app and the MCP server.
//!
//! Everything PlanCraft keeps for a user (settings, recovery copies, the MCP session file) lives
//! in one private directory (0700 on Unix), because plans hold people's names and rates.

use std::path::PathBuf;

/// PlanCraft's per-user configuration directory (not created here).
///
/// `$XDG_CONFIG_HOME/plancraft`, `%APPDATA%\PlanCraft`, `~/Library/Application Support/PlanCraft`
/// on macOS, or `~/.config/plancraft`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("plancraft"));
    }
    if let Some(a) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(a).join("PlanCraft"));
    }
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| {
        let h = PathBuf::from(h);
        if cfg!(target_os = "macos") { h.join("Library/Application Support/PlanCraft") } else { h.join(".config/plancraft") }
    })
}

/// Where the desktop app keeps recovery copies of unsaved work.
pub fn recovery_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("recovery"))
}

/// The file through which the desktop app tells local MCP clients (`plancraft mcp --attach`)
/// where its server listens and which bearer token it expects. Readable by the owner only.
pub fn mcp_session_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("mcp-session.json"))
}

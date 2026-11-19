//! Per-user application settings (recent files, MCP preferences).
//!
//! The file is treated as hostile: it is read with a size cap and anything unreadable falls back
//! to defaults. It is written atomically and readable by its owner only.

use std::io::Read;
use std::path::{Path, PathBuf};

use plancraft_core::io::{Privacy, create_private_dir, write_atomic};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub recent: Vec<PathBuf>,
    pub mcp_port: u16,
    pub mcp_autostart: bool,
    pub table_width: f32,
    pub theme: crate::theme::ThemeChoice,
    /// Servers plans were opened from, last used first (never a password).
    pub servers: Vec<KnownServer>,
}

/// A PlanCraft Server this person opened plans from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownServer {
    /// `host:port`.
    pub address: String,
    pub user: String,
    /// The server's key fingerprint as the person confirmed it on their first visit: a server
    /// showing another key is refused.
    pub fingerprint: String,
    /// The plan last opened there.
    pub plan: String,
    /// The name the server gave itself (to warn when a "new" server has the name of a known one).
    pub name: String,
}

/// Whether two addresses name the same server (stored ones may predate lower-casing).
pub fn same_address(stored: &str, address: &str) -> bool {
    stored == address || plancraft_server::link::normalise_address(stored).is_ok_and(|a| a == address)
}

/// Servers remembered, at most.
const MAX_SERVERS: usize = 10;
/// Longest text kept in a remembered server's fields.
const MAX_SERVER_FIELD: usize = 300;

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            recent: Vec::new(),
            mcp_port: 7878,
            mcp_autostart: false,
            table_width: 720.0,
            theme: crate::theme::ThemeChoice::System,
            servers: Vec::new(),
        }
    }
}

/// Settings files are small; a bigger one is not ours and is ignored.
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

fn path() -> Option<PathBuf> {
    crate::dialogs::misc::config_dir().map(|d| d.join("settings.json"))
}

impl AppSettings {
    pub fn load() -> Self {
        if cfg!(test) {
            return Self::default();
        }
        path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    /// Reads settings from `path`; a missing, oversized or damaged file gives the defaults.
    fn load_from(path: &Path) -> Self {
        let read = || -> Option<Self> {
            let file = std::fs::File::open(path).ok()?;
            if file.metadata().ok()?.len() > MAX_SETTINGS_BYTES {
                return None;
            }
            let mut text = String::new();
            file.take(MAX_SETTINGS_BYTES).read_to_string(&mut text).ok()?;
            serde_json::from_str::<Self>(&text).ok()
        };
        read().unwrap_or_default().sanitized()
    }

    /// Keeps values usable whatever the file said.
    fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        if !self.table_width.is_finite() || !(100.0..=10_000.0).contains(&self.table_width) {
            self.table_width = defaults.table_width;
        }
        if self.mcp_port == 0 {
            self.mcp_port = defaults.mcp_port;
        }
        self.recent.truncate(12);
        self.servers.truncate(MAX_SERVERS);
        let short = |s: &mut String| *s = s.chars().filter(|c| !c.is_control()).take(MAX_SERVER_FIELD).collect();
        for k in &mut self.servers {
            short(&mut k.address);
            short(&mut k.user);
            short(&mut k.fingerprint);
            short(&mut k.plan);
            short(&mut k.name);
        }
        self.servers.retain(|k| !k.address.is_empty());
        self
    }

    /// The fingerprint confirmed for the server at `address`, if any.
    pub fn fingerprint_for(&self, address: &str) -> Option<&str> {
        self.servers.iter().find(|k| same_address(&k.address, address) && !k.fingerprint.is_empty()).map(|k| k.fingerprint.as_str())
    }

    /// A known server that calls itself `name` but has another key than `fingerprint`.
    pub fn same_name_other_key(&self, name: &str, fingerprint: &str) -> Option<&KnownServer> {
        self.servers.iter().find(|k| !k.name.is_empty() && k.name == name && !k.fingerprint.is_empty() && k.fingerprint != fingerprint)
    }

    /// Remembers a server plans were opened from (first in the list from now on).
    pub fn remember_server(&mut self, server: KnownServer) {
        self.servers.retain(|k| !same_address(&k.address, &server.address));
        self.servers.insert(0, server);
        self.servers.truncate(MAX_SERVERS);
        self.save();
    }

    pub fn save(&self) {
        if cfg!(test) {
            return;
        }
        if let Some(p) = path() {
            let _ = self.save_to(&p);
        }
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        write_atomic(path, text.as_bytes(), Privacy::OwnerOnly)
    }

    pub fn add_recent(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(12);
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::tests::TempDir;

    #[test]
    fn settings_round_trip_privately_and_garbage_gives_defaults() {
        let tmp = TempDir::new("settings");
        let path = tmp.0.join("cfg").join("settings.json");
        let s = AppSettings { mcp_port: 9000, table_width: 500.0, ..AppSettings::default() };
        s.save_to(&path).unwrap();
        let back = AppSettings::load_from(&path);
        assert_eq!((back.mcp_port, back.table_width), (9000, 500.0));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        }

        std::fs::write(&path, b"\x00{ not json").unwrap();
        assert_eq!(AppSettings::load_from(&path).mcp_port, 7878);
        std::fs::write(&path, br#"{"table_width": -1e308, "mcp_port": 0}"#).unwrap();
        let s = AppSettings::load_from(&path);
        assert_eq!((s.mcp_port, s.table_width), (7878, 720.0));
        std::fs::write(&path, vec![b' '; (MAX_SETTINGS_BYTES + 1) as usize]).unwrap();
        assert_eq!(AppSettings::load_from(&path).mcp_port, 7878);
    }

    #[test]
    fn servers_are_remembered_without_passwords_and_kept_in_bounds() {
        let tmp = TempDir::new("servers");
        let path = tmp.0.join("settings.json");
        let mut s = AppSettings::default();
        for i in 0..15 {
            s.remember_server(KnownServer {
                address: format!("host{i}:47340"),
                user: "ana".into(),
                fingerprint: format!("fp{i}"),
                plan: "P".into(),
                name: format!("server {i}"),
            });
        }
        assert_eq!(s.servers.len(), MAX_SERVERS);
        assert_eq!(s.servers[0].address, "host14:47340", "last used first");
        assert_eq!(s.fingerprint_for("host14:47340"), Some("fp14"));
        assert_eq!(s.fingerprint_for("elsewhere:1"), None);
        // A known server's name with another key, at whatever address, is noticed.
        assert_eq!(s.same_name_other_key("server 14", "another key").map(|k| k.address.as_str()), Some("host14:47340"));
        assert!(s.same_name_other_key("server 14", "fp14").is_none() && s.same_name_other_key("new one", "x").is_none());
        // An address remembered before addresses were put in lower case is still found.
        s.servers[0].address = "Host14.:47340".into();
        assert_eq!(s.fingerprint_for("host14:47340"), Some("fp14"));
        s.save_to(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("password"));
        let long = "x".repeat(100_000);
        std::fs::write(&path, format!(r#"{{"servers": [{{"address": "{long}", "user": "\u0000ana"}}, {{"address": ""}}]}}"#)).unwrap();
        let back = AppSettings::load_from(&path);
        assert_eq!(back.servers.len(), 1);
        assert_eq!(back.servers[0].address.len(), MAX_SERVER_FIELD);
        assert_eq!(back.servers[0].user, "ana");
    }
}

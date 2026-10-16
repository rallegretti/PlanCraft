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
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { recent: Vec::new(), mcp_port: 7878, mcp_autostart: false, table_width: 720.0, theme: crate::theme::ThemeChoice::System }
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
        self
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
}

//! Per-user application settings (recent files, MCP preferences).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub recent: Vec<PathBuf>,
    pub mcp_port: u16,
    pub mcp_autostart: bool,
    pub table_width: f32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { recent: Vec::new(), mcp_port: 7878, mcp_autostart: false, table_width: 720.0 }
    }
}

fn path() -> Option<PathBuf> {
    crate::dialogs::misc::config_dir().map(|d| d.join("settings.json"))
}

impl AppSettings {
    pub fn load() -> Self {
        path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }

    pub fn add_recent(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(12);
        self.save();
    }
}

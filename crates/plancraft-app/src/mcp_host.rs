//! Hosts the embedded MCP HTTP server for the desktop app.
//!
//! The server shares the app's [`SharedWorkspace`], so agents operate on the
//! documents open in the window and every change shows up immediately.
//!
//! Every start uses a fresh bearer token. The URL and token are published in an owner-only
//! session file so `plancraft mcp --attach` can relay a stdio client to this window; the file is
//! withdrawn when the server stops.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use plancraft_core::workspace::SharedWorkspace;
use plancraft_mcp::session::{self, SessionInfo};
use plancraft_mcp::{HttpHandle, McpServer, ServerOptions, serve_http};

#[derive(Default)]
pub struct McpHost {
    handle: Option<HttpHandle>,
    pub error: Option<String>,
    changes: Arc<AtomicU64>,
}

impl McpHost {
    pub fn running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| h.is_running())
    }

    pub fn url(&self) -> Option<String> {
        self.handle.as_ref().filter(|h| h.is_running()).map(|h| h.url())
    }

    /// The bearer token HTTP clients must send while the server runs (new on every start).
    pub fn token(&self) -> Option<&str> {
        self.handle.as_ref().filter(|h| h.is_running()).map(|h| h.token())
    }

    /// Number of change notifications received from agents.
    pub fn changes(&self) -> u64 {
        self.changes.load(Ordering::Relaxed)
    }

    pub fn start(&mut self, ws: SharedWorkspace, ctx: egui::Context, port: u16) {
        self.stop();
        let changes = self.changes.clone();
        let options = ServerOptions {
            extra_instructions: Some(
                "You are connected to the PlanCraft desktop app. Edits appear live in the user's window and can be undone there; \
                 call save_project when the user wants changes written to disk."
                    .into(),
            ),
            ..ServerOptions::default()
        };
        let server = McpServer::with_options(ws, options).with_change_callback(Arc::new(move || {
            changes.fetch_add(1, Ordering::Relaxed);
            ctx.request_repaint();
        }));
        let token = session::new_token();
        match serve_http(Arc::new(server), &format!("127.0.0.1:{port}"), &token) {
            Ok(h) => {
                let info = SessionInfo { url: h.url(), token: h.token().to_string(), pid: std::process::id() };
                self.error = match session::publish(&info) {
                    Ok(_) => None,
                    Err(e) => Some(format!("The server is running, but `plancraft mcp --attach` cannot find it: {e}")),
                };
                self.handle = Some(h);
            }
            Err(e) => self.error = Some(format!("Could not start the MCP server on port {port}: {e}")),
        }
    }

    pub fn stop(&mut self) {
        if let Some(h) = self.handle.take() {
            session::withdraw(h.token());
            h.shutdown();
        }
    }
}

impl Drop for McpHost {
    fn drop(&mut self) {
        self.stop();
    }
}

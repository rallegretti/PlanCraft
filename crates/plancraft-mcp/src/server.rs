//! Transport-agnostic MCP server: JSON-RPC dispatch, tool execution context.

use std::cell::Cell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, MutexGuard};

use plancraft_core::model::Project;
use plancraft_core::workspace::{ChangeSource, Document, SharedWorkspace, Workspace};
use serde_json::{Map, Value, json};

use crate::args::{Args, ToolError, ToolResult, err};
use crate::protocol::*;
use crate::tools::{self, Tool};

/// Callback invoked (outside the workspace lock) after the agent changed something.
pub type ChangeCallback = Arc<dyn Fn() + Send + Sync>;

/// Server configuration.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    /// Name reported in `serverInfo`.
    pub name: String,
    /// Human-friendly title reported in `serverInfo`.
    pub title: String,
    /// Version reported in `serverInfo`.
    pub version: String,
    /// Whether tools may open, create, save or link arbitrary file paths.
    ///
    /// When `false`, `open_project`, `new_project` with a path, `save_project`
    /// with a new path and `add_external_project` are refused; saving an open
    /// document to its existing path is still allowed.
    pub allow_file_access: bool,
    /// Extra text appended to the `instructions` returned by `initialize`.
    pub extra_instructions: Option<String>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            name: "plancraft".into(),
            title: "PlanCraft".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            allow_file_access: true,
            extra_instructions: None,
        }
    }
}

/// The PlanCraft MCP server.
///
/// It wraps a [`SharedWorkspace`]; the workspace mutex is only held while a
/// single request is processed, never while waiting on a transport.
pub struct McpServer {
    workspace: SharedWorkspace,
    on_change: Option<ChangeCallback>,
    options: ServerOptions,
    tools: Vec<Tool>,
    tool_index: HashMap<&'static str, usize>,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer").field("options", &self.options).field("tools", &self.tools.len()).finish()
    }
}

impl McpServer {
    /// Creates a server with default options.
    pub fn new(workspace: SharedWorkspace) -> Self {
        Self::with_options(workspace, ServerOptions::default())
    }

    pub fn with_options(workspace: SharedWorkspace, options: ServerOptions) -> Self {
        let tools = tools::all_tools();
        let tool_index = tools.iter().enumerate().map(|(i, t)| (t.name, i)).collect();
        Self { workspace, on_change: None, options, tools, tool_index }
    }

    /// Sets the callback run after every change made through the server
    /// (the desktop app uses it to request a repaint).
    pub fn with_change_callback(mut self, cb: ChangeCallback) -> Self {
        self.on_change = Some(cb);
        self
    }

    pub fn set_change_callback(&mut self, cb: Option<ChangeCallback>) {
        self.on_change = cb;
    }

    pub fn workspace(&self) -> &SharedWorkspace {
        &self.workspace
    }

    pub fn options(&self) -> &ServerOptions {
        &self.options
    }

    /// All tool definitions.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Workspace> {
        self.workspace.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn notify_change(&self) {
        if let Some(cb) = &self.on_change {
            cb();
        }
    }

    /// Handles one JSON-RPC message (or a batch array).
    ///
    /// Returns the response to send, or `None` for notifications and
    /// client responses.
    pub fn handle_message(&self, msg: Value) -> Option<Value> {
        match msg {
            Value::Array(items) => {
                if items.is_empty() {
                    return Some(error_response(Value::Null, &RpcError::new(INVALID_REQUEST, "Invalid Request: empty batch")));
                }
                let out: Vec<Value> = items.into_iter().filter_map(|m| self.handle_single(m)).collect();
                if out.is_empty() { None } else { Some(Value::Array(out)) }
            }
            other => self.handle_single(other),
        }
    }

    /// Parses and handles a raw JSON text message.
    pub fn handle_text(&self, text: &str) -> Option<Value> {
        match serde_json::from_str::<Value>(text) {
            Ok(v) => self.handle_message(v),
            Err(e) => Some(parse_error(e)),
        }
    }

    fn handle_single(&self, msg: Value) -> Option<Value> {
        let Value::Object(obj) = msg else {
            return Some(error_response(Value::Null, &RpcError::new(INVALID_REQUEST, "Invalid Request: expected an object")));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            if obj.contains_key("result") || obj.contains_key("error") {
                return None; // a response to a server request; we never send any
            }
            return Some(error_response(id.unwrap_or(Value::Null), &RpcError::new(INVALID_REQUEST, "Invalid Request: missing 'method'")));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        match id {
            None => {
                self.handle_notification(method, &params);
                None
            }
            Some(id) => Some(match self.handle_request(method, &params) {
                Ok(result) => response(id, result),
                Err(e) => error_response(id, &e),
            }),
        }
    }

    fn handle_notification(&self, method: &str, _params: &Value) {
        match method {
            "notifications/initialized" | "notifications/cancelled" | "notifications/roots/list_changed" | "notifications/progress" => {}
            other => eprintln!("plancraft-mcp: ignoring notification {other}"),
        }
    }

    fn handle_request(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.tools.iter().map(Tool::to_json).collect::<Vec<_>>() })),
            "tools/call" => self.call_tool_request(params),
            "resources/list" => Ok(crate::resources::list(self)),
            "resources/templates/list" => Ok(crate::resources::templates()),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).ok_or_else(|| RpcError::invalid_params("missing 'uri'"))?;
                crate::resources::read(self, uri)
            }
            "prompts/list" => Ok(crate::prompts::list()),
            "prompts/get" => crate::prompts::get(params),
            "logging/setLevel" => Ok(json!({})),
            "completion/complete" => Ok(json!({ "completion": { "values": [], "hasMore": false } })),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = negotiate_version(requested);
        let mut instructions = crate::INSTRUCTIONS.to_string();
        if !self.options.allow_file_access {
            instructions.push_str("\nFile access is disabled on this server: work with the documents that are already open.\n");
        }
        if let Some(extra) = &self.options.extra_instructions {
            instructions.push('\n');
            instructions.push_str(extra);
        }
        json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false },
                "logging": {},
            },
            "serverInfo": {
                "name": self.options.name,
                "title": self.options.title,
                "version": self.options.version,
            },
            "instructions": instructions,
        })
    }

    fn call_tool_request(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params.get("name").and_then(Value::as_str).ok_or_else(|| RpcError::invalid_params("missing tool 'name'"))?;
        let empty = Value::Object(Map::new());
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(v @ Value::Object(_)) => v,
            Some(_) => return Ok(tool_error_result("'arguments' must be an object")),
        };
        if !self.tool_index.contains_key(name) {
            return Err(RpcError::invalid_params(format!("Unknown tool: {name}")));
        }
        Ok(self.call_tool(name, arguments))
    }

    /// Runs a tool and returns the MCP `CallToolResult` JSON.
    ///
    /// Unknown tools and failures are reported as results with `isError: true`.
    pub fn call_tool(&self, name: &str, arguments: &Value) -> Value {
        let Some(tool) = self.tool_index.get(name).and_then(|&i| self.tools.get(i)) else {
            return tool_error_result(format!("unknown tool '{name}'"));
        };
        let empty = Map::new();
        let map = arguments.as_object().unwrap_or(&empty);
        if let Err(e) = tool.validate(map) {
            return tool_error_result(e.0);
        }
        let ctx = Ctx { server: self, changed: Cell::new(false) };
        let args = Args::new(map);
        let outcome = catch_unwind(AssertUnwindSafe(|| (tool.handler)(&ctx, &args)));
        if ctx.changed.get() {
            self.notify_change();
        }
        match outcome {
            Ok(Ok(value)) => tool_success_result(tool, value),
            Ok(Err(e)) => tool_error_result(e.0),
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown panic".into());
                tool_error_result(format!("internal error in '{name}': {msg}"))
            }
        }
    }
}

fn render_text(v: &Value) -> String {
    let compact = serde_json::to_string(v).unwrap_or_default();
    if compact.len() > 24_000 { compact } else { serde_json::to_string_pretty(v).unwrap_or(compact) }
}

fn tool_success_result(tool: &Tool, value: Value) -> Value {
    let structured = match value {
        Value::Object(_) => value,
        other => json!({ "result": other }),
    };
    let text = match tool.text_field.and_then(|f| structured.get(f)).and_then(Value::as_str) {
        Some(t) => t.to_string(),
        None => render_text(&structured),
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": false,
    })
}

fn tool_error_result(msg: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": format!("Error: {}", msg.into()) }],
        "isError": true,
    })
}

/// Execution context handed to tool handlers.
pub struct Ctx<'a> {
    pub server: &'a McpServer,
    changed: Cell<bool>,
}

impl Ctx<'_> {
    /// Marks that the workspace changed (triggers the change callback).
    pub fn mark_changed(&self) {
        self.changed.set(true);
    }

    pub fn allow_file_access(&self) -> bool {
        self.server.options.allow_file_access
    }

    pub fn require_file_access(&self, what: &str) -> ToolResult<()> {
        if self.allow_file_access() { Ok(()) } else { err(format!("{what} is disabled on this server (file access not permitted)")) }
    }

    /// Finds the document selected by the optional `document` argument.
    pub fn doc_index(ws: &Workspace, a: &Args) -> ToolResult<usize> {
        if ws.docs.is_empty() {
            return err("no document is open; call new_project or open_project first");
        }
        match a.get("document") {
            None => {
                ws.active.filter(|&i| i < ws.docs.len()).ok_or_else(|| ToolError("no active document; call set_active_document".into()))
            }
            Some(v) => {
                let key = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    other => {
                        return err(format!("'document' must be a handle number or a name/path string, got {other}"));
                    }
                };
                ws.find(&key).ok_or_else(|| {
                    ToolError(format!(
                        "no open document matches '{key}'; open documents: {}",
                        ws.docs.iter().map(|d| format!("{} ({})", d.handle, d.project.name)).collect::<Vec<_>>().join(", ")
                    ))
                })
            }
        }
    }

    /// Runs `f` with the whole workspace locked.
    pub fn with_workspace<R>(&self, f: impl FnOnce(&mut Workspace) -> ToolResult<R>) -> ToolResult<R> {
        let mut ws = self.server.lock();
        f(&mut ws)
    }

    /// Runs `f` on the selected document (read only).
    pub fn read<R>(&self, a: &Args, f: impl FnOnce(&Document) -> ToolResult<R>) -> ToolResult<R> {
        let ws = self.server.lock();
        let i = Self::doc_index(&ws, a)?;
        f(doc_at(&ws, i)?)
    }

    /// Runs `f` on the selected document mutably, without undo support (e.g. save).
    pub fn with_doc<R>(&self, a: &Args, f: impl FnOnce(&mut Document) -> ToolResult<R>) -> ToolResult<R> {
        let mut ws = self.server.lock();
        let i = Self::doc_index(&ws, a)?;
        f(doc_at_mut(&mut ws, i)?)
    }

    /// Applies an undoable edit through [`Document::try_edit`] and renders the result.
    ///
    /// `f` receives the project and the document's path. On error the project is
    /// restored. After a successful edit the schedule is recalculated and `view`
    /// renders the outcome from the updated document.
    pub fn edit<R>(
        &self,
        a: &Args,
        label: &str,
        f: impl FnOnce(&mut Project, Option<&Path>) -> ToolResult<R>,
        view: impl FnOnce(&Document, R) -> ToolResult<Value>,
    ) -> ToolResult<Value> {
        let mut ws = self.server.lock();
        let i = Self::doc_index(&ws, a)?;
        let doc = doc_at_mut(&mut ws, i)?;
        let path = doc.path.clone();
        // `try_edit` validates the result against the core limits and rolls back on any error,
        // including a panic inside `f` (turned into an error by the core guard).
        let r = doc.try_edit(label, ChangeSource::Agent, |p| f(p, path.as_deref()))?;
        self.mark_changed();
        let mut out = view(doc, r)?;
        if let Value::Object(m) = &mut out {
            m.entry("document").or_insert(json!(doc.handle));
        }
        Ok(out)
    }
}

/// The open document at index `i`.
pub fn doc_at(ws: &Workspace, i: usize) -> ToolResult<&Document> {
    ws.docs.get(i).ok_or_else(|| crate::args::missing("a document"))
}

/// The open document at index `i`, mutably.
pub fn doc_at_mut(ws: &mut Workspace, i: usize) -> ToolResult<&mut Document> {
    ws.docs.get_mut(i).ok_or_else(|| crate::args::missing("a document"))
}

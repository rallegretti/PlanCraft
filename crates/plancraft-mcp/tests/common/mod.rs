#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use plancraft_core::workspace::Workspace;
use plancraft_mcp::McpServer;
use serde_json::{Value, json};

/// Drives an [`McpServer`] through `handle_message`, like a real client.
pub struct Client {
    pub server: McpServer,
    pub changes: Arc<AtomicUsize>,
    next_id: std::cell::Cell<i64>,
}

impl Client {
    pub fn new() -> Self {
        let changes = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&changes);
        let server = McpServer::new(Workspace::default().shared()).with_change_callback(Arc::new(
            move || {
                c.fetch_add(1, Ordering::SeqCst);
            },
        ));
        let client = Self {
            server,
            changes,
            next_id: std::cell::Cell::new(1),
        };
        let init = client.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }));
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(
            client
                .server
                .handle_message(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .is_none()
        );
        client
    }

    pub fn request(&self, method: &str, params: Value) -> Value {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let resp = self
            .server
            .handle_message(
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
            )
            .expect("response");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], id);
        resp
    }

    /// Calls a tool and returns the raw `CallToolResult`.
    pub fn call_raw(&self, name: &str, args: Value) -> Value {
        let resp = self.request("tools/call", json!({ "name": name, "arguments": args }));
        assert!(
            resp.get("error").is_none(),
            "protocol error calling {name}: {resp}"
        );
        resp["result"].clone()
    }

    /// Calls a tool that must succeed; returns `structuredContent`.
    pub fn call(&self, name: &str, args: Value) -> Value {
        let r = self.call_raw(name, args.clone());
        assert_eq!(
            r["isError"], false,
            "tool {name}({args}) failed: {}",
            r["content"][0]["text"]
        );
        assert_eq!(r["content"][0]["type"], "text");
        assert!(
            r["content"][0]["text"]
                .as_str()
                .is_some_and(|t| !t.is_empty())
        );
        assert!(r["structuredContent"].is_object());
        r["structuredContent"].clone()
    }

    /// Calls a tool that must fail; returns the error text.
    pub fn call_err(&self, name: &str, args: Value) -> String {
        let r = self.call_raw(name, args.clone());
        assert_eq!(
            r["isError"], true,
            "tool {name}({args}) unexpectedly succeeded: {r}"
        );
        r["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

/// Finds a task id by name in a create_tasks result.
pub fn created_id(created: &Value, name: &str) -> u64 {
    created["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no created task {name}"))["id"]
        .as_u64()
        .unwrap()
}

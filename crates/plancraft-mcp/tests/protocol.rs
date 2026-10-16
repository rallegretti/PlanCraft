#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! JSON-RPC / MCP protocol behaviour driven through `handle_message`.

mod common;

use std::collections::HashSet;
use std::io::Cursor;

use common::Client;
use plancraft_core::workspace::Workspace;
use plancraft_mcp::{McpServer, run_stdio_with};
use serde_json::{Value, json};

#[test]
fn initialize_negotiates_versions() {
    let server = McpServer::new(Workspace::default().shared());
    for (asked, expected) in
        [("2025-11-25", "2025-11-25"), ("2025-03-26", "2025-03-26"), ("2024-11-05", "2024-11-05"), ("1999-01-01", "2025-11-25")]
    {
        let r = server.handle_message(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": asked, "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } } })).unwrap();
        assert_eq!(r["result"]["protocolVersion"], expected);
        assert_eq!(r["result"]["serverInfo"]["name"], "plancraft");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert!(r["result"]["capabilities"]["resources"].is_object());
        assert!(r["result"]["instructions"].as_str().unwrap().contains("YYYY-MM-DD"));
    }
}

#[test]
fn basic_jsonrpc_behaviour() {
    let c = Client::new();
    assert_eq!(c.request("ping", json!({}))["result"], json!({}));
    let r = c.request("no/such/method", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    // Notifications produce no response.
    assert!(
        c.server.handle_message(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 3 } })).is_none()
    );
    // Client responses are ignored.
    assert!(c.server.handle_message(json!({ "jsonrpc": "2.0", "id": 99, "result": {} })).is_none());
    // Parse errors.
    let r = c.server.handle_text("{ not json").unwrap();
    assert_eq!(r["error"]["code"], -32700);
    assert_eq!(r["id"], Value::Null);
    // Invalid requests.
    assert_eq!(c.server.handle_message(json!(42)).unwrap()["error"]["code"], -32600);
    assert_eq!(c.server.handle_message(json!([])).unwrap()["error"]["code"], -32600);
    // Batches: two requests + one notification -> two responses.
    let batch = c
        .server
        .handle_message(json!([
            { "jsonrpc": "2.0", "id": "a", "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "id": "b", "method": "tools/call", "params": { "name": "list_documents", "arguments": {} } }
        ]))
        .unwrap();
    let arr = batch.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], "a");
    assert_eq!(arr[1]["result"]["isError"], false);
    // Notification-only batch -> no response.
    assert!(c.server.handle_message(json!([{ "jsonrpc": "2.0", "method": "notifications/initialized" }])).is_none());
}

#[test]
fn tool_schemas_are_valid() {
    let c = Client::new();
    let r = c.request("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    assert!(tools.len() >= 60, "only {} tools", tools.len());
    let mut names = HashSet::new();
    for t in tools {
        let name = t["name"].as_str().unwrap();
        assert!(names.insert(name.to_string()), "duplicate tool {name}");
        assert!(name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'), "tool name {name} is not snake_case");
        assert!(t["description"].as_str().is_some_and(|d| d.len() > 15), "{name} lacks a description");
        let schema = &t["inputSchema"];
        assert!(schema.is_object(), "{name}: schema is not an object");
        assert_eq!(schema["type"], "object", "{name}: schema type must be object");
        let props = schema["properties"].as_object().unwrap_or_else(|| panic!("{name}: properties missing"));
        for (pname, p) in props {
            assert!(p.is_object(), "{name}.{pname} schema is not an object");
            assert!(p.get("type").is_some() || p.get("anyOf").is_some(), "{name}.{pname} has no type");
        }
        if let Some(req) = schema.get("required") {
            for r in req.as_array().unwrap() {
                assert!(props.contains_key(r.as_str().unwrap()), "{name}: required '{r}' not in properties");
            }
        }
        assert!(t["annotations"]["readOnlyHint"].is_boolean());
        // Round-trips as JSON text.
        let text = serde_json::to_string(schema).unwrap();
        let _: Value = serde_json::from_str(&text).unwrap();
    }
    for required in [
        "list_documents",
        "new_project",
        "open_project",
        "save_project",
        "close_project",
        "set_active_document",
        "get_project",
        "update_project",
        "list_tasks",
        "get_task",
        "create_task",
        "create_tasks",
        "update_task",
        "delete_tasks",
        "move_task",
        "indent_tasks",
        "outdent_tasks",
        "create_milestone",
        "update_milestone_members",
        "milestone_status",
        "link_tasks",
        "unlink_tasks",
        "link_chain",
        "list_dependencies",
        "add_external_project",
        "list_external_projects",
        "list_external_tasks",
        "link_external_task",
        "refresh_external_links",
        "list_resources",
        "get_resource",
        "create_resource",
        "update_resource",
        "delete_resources",
        "add_resource_exception",
        "remove_resource_exception",
        "set_resource_availability",
        "assign_resource",
        "update_assignment",
        "unassign_resource",
        "list_assignments",
        "list_calendars",
        "get_calendar",
        "create_calendar",
        "set_work_week",
        "add_calendar_exception",
        "remove_calendar_exception",
        "delete_calendar",
        "log_time",
        "list_time_entries",
        "delete_time_entry",
        "update_progress_from_time",
        "get_schedule",
        "critical_path",
        "get_workload",
        "get_overallocations",
        "level_resources",
        "clear_leveling",
        "project_statistics",
        "set_baseline",
        "clear_baseline",
        "get_variance",
        "undo",
        "redo",
        "get_history",
    ] {
        assert!(names.contains(required), "missing tool {required}");
    }
}

#[test]
fn tool_errors_are_results() {
    let c = Client::new();
    // No document open yet.
    let e = c.call_err("list_tasks", json!({}));
    assert!(e.contains("new_project"), "{e}");
    // Unknown tool is a protocol error.
    let r = c.request("tools/call", json!({ "name": "nope", "arguments": {} }));
    assert_eq!(r["error"]["code"], -32602);
    c.call("new_project", json!({ "name": "P", "start": "2026-10-19" }));
    // Unknown argument.
    let e = c.call_err("create_task", json!({ "name": "A", "durration": "2d" }));
    assert!(e.contains("durration"), "{e}");
    // Missing required argument.
    let e = c.call_err("get_task", json!({}));
    assert!(e.contains("task"), "{e}");
    // Bad values.
    let e = c.call_err("create_task", json!({ "name": "A", "duration": "three days" }));
    assert!(e.contains("duration"), "{e}");
    let e = c.call_err("get_task", json!({ "task": 999 }));
    assert!(e.contains("999"), "{e}");
    // Failed edits are rolled back and leave no undo step.
    let hist = c.call("get_history", json!({}));
    assert_eq!(hist["undo_steps"], 0);
}

#[test]
fn resources_and_prompts() {
    let c = Client::new();
    c.call("new_project", json!({ "name": "Res Test", "start": "2026-10-19" }));
    c.call("create_task", json!({ "name": "Only task", "duration": "2d" }));
    let list = c.request("resources/list", json!({}));
    let uris: Vec<&str> = list["result"]["resources"].as_array().unwrap().iter().map(|r| r["uri"].as_str().unwrap()).collect();
    assert!(uris.contains(&"plancraft://documents"));
    assert!(uris.iter().any(|u| u.ends_with("/project.json")));

    let read = c.request("resources/read", json!({ "uri": "plancraft://document/active/project.json" }));
    let text = read["result"]["contents"][0]["text"].as_str().unwrap();
    let project: Value = serde_json::from_str(text).unwrap();
    assert_eq!(project["name"], "Res Test");
    assert_eq!(project["tasks"][0]["name"], "Only task");

    let summary = c.request("resources/read", json!({ "uri": "plancraft://document/1/summary" }));
    assert!(summary["result"]["contents"][0]["text"].as_str().unwrap().contains("# Res Test"));
    let sched = c.request("resources/read", json!({ "uri": "plancraft://document/active/schedule" }));
    assert!(sched["result"]["contents"][0]["text"].as_str().unwrap().contains("Only task"));
    let missing = c.request("resources/read", json!({ "uri": "plancraft://document/77/summary" }));
    assert_eq!(missing["error"]["code"], -32002);

    let templates = c.request("resources/templates/list", json!({}));
    assert_eq!(templates["result"]["resourceTemplates"].as_array().unwrap().len(), 3);

    let prompts = c.request("prompts/list", json!({}));
    assert_eq!(prompts["result"]["prompts"].as_array().unwrap().len(), 2);
    let p = c.request("prompts/get", json!({ "name": "plan_project", "arguments": { "goal": "Build a shed" } }));
    assert!(p["result"]["messages"][0]["content"]["text"].as_str().unwrap().contains("Build a shed"));
}

#[test]
fn stdio_transport_round_trip() {
    let server = McpServer::new(Workspace::default().shared());
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"new_project","arguments":{"name":"Stdio"}}}"#,
        "\n",
        "garbage\n",
    );
    let mut out = Vec::new();
    run_stdio_with(&server, Cursor::new(input.as_bytes()), &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[1]["id"], 2);
    assert_eq!(lines[1]["result"]["isError"], false);
    assert_eq!(lines[2]["error"]["code"], -32700);
}

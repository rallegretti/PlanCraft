#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Fuzz-ish sweeps: no message and no tool argument may crash the server, leave it unable to
//! answer, or even panic inside a guard.
//!
//! The server and the core turn panics into errors (a last resort), so a panic would not show up
//! as a failure by itself. A panic hook counts panics on the test's own thread; any count above
//! zero fails the test with the tool and arguments that caused it.

use std::cell::Cell;
use std::sync::Once;

use chrono::NaiveDate;
use plancraft_core::sample::demo_project;
use plancraft_core::workspace::Workspace;
use plancraft_mcp::{McpServer, ServerOptions};
use proptest::prelude::*;
use serde_json::{Map, Value, json};

thread_local! {
    static PANICS: Cell<usize> = const { Cell::new(0) };
}

/// Counts panics per thread (and keeps the default report on stderr).
fn count_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICS.with(|p| p.set(p.get() + 1));
            default(info);
        }));
    });
}

fn panics() -> usize {
    PANICS.with(Cell::get)
}

/// A server with the demo project open and file access off (junk must not write files).
fn server() -> McpServer {
    count_panics();
    let mut ws = Workspace::default();
    ws.add(demo_project(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()), None);
    McpServer::with_options(ws.shared(), ServerOptions { allow_file_access: false, ..ServerOptions::default() })
}

fn still_alive(s: &McpServer) {
    let r = s.handle_text(r#"{"jsonrpc":"2.0","id":"alive","method":"ping"}"#).unwrap();
    assert_eq!(r["result"], json!({}));
}

/// Values that have broken parsers before, or plausibly could.
fn junk_values() -> Vec<Value> {
    vec![
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(1),
        json!(i64::MAX),
        json!(i64::MIN),
        json!(u64::MAX),
        json!(1e308),
        json!(-1e308),
        json!(0.5),
        json!(""),
        json!(" "),
        json!("ééé"),
        json!("#ééé"),
        json!("#aébé"),
        json!("aébé"),
        json!("#zzzzzz"),
        json!("3aé"),
        json!("1FSé"),
        json!("999999999999999999999d"),
        json!("-5d"),
        json!("1e400h"),
        json!("NaN"),
        json!("inf"),
        json!("+262143-12-31"),
        json!("-262143-01-01"),
        json!("9999-99-99 99:99"),
        json!("24:00-23:00"),
        json!("#0"),
        json!("#99999999999"),
        json!("x".repeat(20_000)),
        json!("🙂".repeat(100)),
        json!([]),
        json!([1, "2", null, [3]]),
        json!(["ééé", -1, 1e308]),
        json!({}),
        json!({ "name": "ééé", "children": [{ "name": "x", "duration": "1e9d" }] }),
    ]
}

fn call(s: &McpServer, tool: &str, args: Value) -> Value {
    let line = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": args } });
    s.handle_text(&line.to_string()).unwrap()
}

/// A valid value for a commonly required argument, so the junk under test reaches the handler.
fn plausible(name: &str) -> Option<Value> {
    Some(match name {
        "task" | "predecessor" | "milestone" => json!(3),
        "successor" => json!(6),
        "tasks" | "members" => json!([3, 4]),
        "resource" | "assignment" | "entry" | "calendar" => json!(1),
        "resources" => json!([1]),
        "name" => json!("Junk test"),
        "start" | "date" | "from" => json!("2026-10-20"),
        "to" | "finish" => json!("2026-12-20"),
        "units" => json!("50%"),
        "work" | "hours" | "minutes" | "duration" => json!("2h"),
        _ => return None,
    })
}

/// Every tool, every argument, every junk value; the other required arguments get plausible
/// values (one pass) or junk too (a second pass).
#[test]
fn every_tool_argument_with_junk() {
    let tools = plancraft_mcp::tools::all_tools();
    assert!(tools.len() > 60);
    let junk = junk_values();
    let mut calls = 0;
    for tool in &tools {
        // A fresh server per tool keeps one destructive tool from starving the next of data.
        let s = server();
        let props: Vec<String> =
            tool.input_schema.get("properties").and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let required: Vec<String> = tool
            .input_schema
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        for prop in &props {
            for (k, value) in junk.iter().enumerate().flat_map(|kv| [(kv, true), (kv, false)]).map(|((k, v), good)| ((k, good), v)) {
                let (k, good) = k;
                let mut args = Map::new();
                for r in &required {
                    let fill = if good { plausible(r) } else { None };
                    args.insert(r.clone(), fill.unwrap_or_else(|| junk[(k + 7) % junk.len()].clone()));
                }
                args.insert(prop.clone(), value.clone());
                let before = panics();
                let r = call(&s, tool.name, Value::Object(args.clone()));
                assert!(
                    panics() == before,
                    "tool '{}' panicked on {}: {}",
                    tool.name,
                    Value::Object(args),
                    r["result"]["content"][0]["text"]
                );
                assert!(r.get("result").is_some() || r.get("error").is_some(), "{r}");
                calls += 1;
            }
        }
        still_alive(&s);
    }
    assert!(calls > 1000, "only {calls} calls");
}

fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Value::from),
        "[a-z#0-9 .:/{}\\-é%?]{0,12}".prop_map(Value::from),
        prop::sample::select(junk_values()),
    ];
    leaf.prop_recursive(3, 24, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::btree_map("[a-z_]{1,10}", inner, 0..5).prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

fn tool_names() -> Vec<&'static str> {
    plancraft_mcp::tools::all_tools().iter().map(|t| t.name).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(256), ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_text_lines(line in "\\PC{0,200}") {
        let s = server();
        let before = panics();
        let _ = s.handle_text(&line);
        prop_assert_eq!(panics(), before);
        still_alive(&s);
    }

    #[test]
    fn arbitrary_json_messages(v in arb_json(), method in prop::sample::select(vec![
        "initialize", "tools/call", "tools/list", "resources/list", "resources/read", "resources/templates/list",
        "prompts/list", "prompts/get", "completion/complete", "logging/setLevel", "ping", "x",
    ])) {
        let s = server();
        let before = panics();
        let _ = s.handle_message(v.clone());
        let _ = s.handle_message(json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": v }));
        let _ = s.handle_message(json!([{ "jsonrpc": "2.0", "id": 2, "method": method, "params": v }, v]));
        prop_assert_eq!(panics(), before);
        still_alive(&s);
    }

    /// Random argument objects built from each tool's own argument names.
    #[test]
    fn arbitrary_tool_arguments(tool in prop::sample::select(tool_names()), values in prop::collection::vec(arb_json(), 0..6)) {
        let s = server();
        let tools = plancraft_mcp::tools::all_tools();
        let def = tools.iter().find(|t| t.name == tool).unwrap();
        let props: Vec<String> =
            def.input_schema.get("properties").and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let mut args = Map::new();
        for (i, v) in values.into_iter().enumerate() {
            if let Some(name) = props.get(i * 7 % props.len().max(1)) {
                args.insert(name.clone(), v);
            }
        }
        let before = panics();
        let r = call(&s, tool, Value::Object(args.clone()));
        prop_assert_eq!(panics(), before, "{} panicked on {}", tool, Value::Object(args));
        prop_assert!(r.get("result").is_some() || r.get("error").is_some());
        if let Some(res) = r.get("result") {
            prop_assert!(res["content"].is_array());
        }
        still_alive(&s);
    }
}

#[test]
fn huge_and_deep_inputs() {
    let s = server();
    let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    assert!(s.handle_text(&deep).is_some());
    let long = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{}"}}"#, "x".repeat(1_000_000));
    assert!(s.handle_text(&long).is_some());
    let many: Vec<Value> = (0..2_000).map(|i| json!({ "name": format!("T{i}"), "duration": "1d" })).collect();
    let r = call(&s, "create_tasks", json!({ "tasks": many }));
    assert!(r.get("result").is_some());
    still_alive(&s);
}

/// The number of random cases: `default`, or `PROPTEST_CASES` when set (to search harder locally).
fn cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Fuzz-ish sweeps: no frame may crash the server, stop it answering, damage a shared plan, or
//! even panic inside a guard.
//!
//! Panics on the server's threads are caught and turned into errors (a last resort), so they
//! would not show up as failures by themselves: a panic hook counts every panic in the process,
//! and any count above zero fails the test.

mod common;

use std::path::Path;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use plancraft_server::client::Client;
use plancraft_server::protocol::{CatchUp, ClientId, ClientMsg, ServerMsg};
use plancraft_server::server::Running;
use proptest::prelude::*;
use serde_json::{Value, json};

static PANICS: AtomicUsize = AtomicUsize::new(0);

fn count_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICS.fetch_add(1, Ordering::SeqCst);
            default(info);
        }));
    });
}

fn junk_values() -> Vec<Value> {
    let mut v = vec![
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
        json!("../../etc/passwd"),
        json!("Plan.plancraft"),
        json!("..\\Plan.plancraft"),
        json!("\u{0}\u{1b}[31m"),
        json!("00000000-0000-0000-0000-000000000000"),
        json!("x".repeat(70_000)),
        json!([]),
        json!([1, 2, 3]),
        json!({}),
        json!({"epoch": "00000000-0000-0000-0000-000000000000", "version": u64::MAX}),
        json!({"epoch": 5, "version": -1}),
    ];
    // Deep nesting.
    let mut deep = json!(0);
    for _ in 0..200 {
        deep = json!([deep]);
    }
    v.push(deep);
    v
}

/// Changes that look nearly right.
fn junk_changes() -> Vec<Value> {
    let id = "11111111-2222-3333-4444-555555555555";
    let patches = vec![
        json!({}),
        json!({"fields": {"sched": {"set": 1}}}),
        json!({"fields": {"id": {"set": "x"}}}),
        json!({"fields": {"name": {"set": "x".repeat(5000)}}}),
        json!({"fields": {"name": "unset"}}),
        json!({"fields": {"start": {"set": "+262143-12-31"}}}),
        json!({"fields": {"tasks": {"set": [{"id": 1}, {"id": 1}]}}}),
        json!({"lists": {"tasks": {"added": {"1": {"id": 1}}}}}),
        json!({"lists": {"tasks": {"added": {"999999": {"id": 999999, "name": "x"}}}}}),
        json!({"lists": {"tasks": {"added": {"4294967295": {"id": 4294967295u64, "name": "x"}}}}}),
        json!({"lists": {"tasks": {"changed": {"1": {"id": {"set": 7}}}}}}),
        json!({"lists": {"tasks": {"changed": {"1": {"duration": {"set": -5}}}}}}),
        json!({"lists": {"tasks": {"changed": {"1": {"duration": {"set": 1e300}}}}}}),
        json!({"lists": {"tasks": {"changed": {"424242": {"name": {"set": "ghost"}}}}}}),
        json!({"lists": {"tasks": {"removed": ["1", "2", "3", "x", "424242"]}}}),
        json!({"lists": {"tasks": {"order": vec!["1"; 20_000]}}}),
        json!({"lists": {"tasks": {"order": [], "moved": true}}}),
        json!({"lists": {"tasks": {"parents": {"1": "1"}}}}),
        json!({"lists": {"tables": {"added": {"\"x\"": {"name": "y"}}}}}),
        json!({"lists": {"assignments": {"added": {"9000": {"id": 9000, "task": 424242, "resource": 1}}}}}),
        json!({"lists": {"nonsense": {}}}),
    ];
    let mut out: Vec<Value> = patches.into_iter().map(|p| json!({"id": id, "base": 1, "label": "junk", "patch": p})).collect();
    for base in [json!(0), json!(u64::MAX), json!(-1), json!("1")] {
        out.push(json!({"id": id, "base": base, "patch": {}}));
    }
    out.push(json!({"id": "not a uuid", "base": 1, "patch": {}}));
    out.push(json!({"base": 1}));
    out
}

/// Every message type with junk in each field, and some frames that are no message at all.
fn junk_messages() -> Vec<Vec<u8>> {
    let fields: Vec<(&str, Vec<&str>)> = vec![
        ("hello", vec!["protocol", "app"]),
        ("login", vec!["user", "password"]),
        ("list", vec![]),
        ("open", vec!["plan", "client", "known", "in_flight"]),
        ("change", vec!["change"]),
        ("lease", vec![]),
        ("resync", vec![]),
        ("presence", vec!["task"]),
        ("ping", vec![]),
        ("no_such_type", vec!["x"]),
    ];
    let mut out = Vec::new();
    for (kind, names) in &fields {
        out.push(serde_json::to_vec(&json!({"type": kind})).unwrap());
        for name in names {
            for junk in junk_values() {
                out.push(serde_json::to_vec(&json!({"type": kind, *name: junk})).unwrap());
            }
        }
    }
    for change in junk_changes() {
        out.push(serde_json::to_vec(&json!({"type": "change", "change": change})).unwrap());
    }
    for raw in [&b""[..], b"null", b"[]", b"{", b"\xff\xfe", b"{\"type\":", b"{\"type\":\"ping\"}trailing"] {
        out.push(raw.to_vec());
    }
    out
}

/// A connection at each stage: fresh, signed in, and with the plan open.
fn connections(server: &Running) -> Vec<(&'static str, Client)> {
    let fresh = Client::connect(server.addr(), &plancraft_server::wire::Trust::FirstUse, "junk").unwrap();
    let signed = signed_in(server, "ana");
    let mut open = signed_in(server, "ben");
    open.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    vec![("fresh", fresh), ("signed in", signed), ("open", open)]
}

/// The server answers, the plan still opens, and it reads back as a valid plan.
fn still_fine(server: &Running) {
    let mut c = signed_in(server, "ana");
    assert!(matches!(c.request(&ClientMsg::Ping).unwrap(), ServerMsg::Pong));
    let opened = c.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
    let CatchUp::Snapshot { snapshot } = opened.catch_up else { panic!() };
    let (mut p, notes) = plancraft_core::io::from_json_with_notes(&snapshot.plan, Path::new("x")).unwrap();
    assert!(notes.is_empty(), "the shared plan stayed within limits: {notes:?}");
    plancraft_core::schedule::recalculate(&mut p);
    let mut copy = p.clone();
    plancraft_core::limits::check_edit(&mut copy).unwrap();
}

#[test]
fn junk_in_every_field_at_every_stage() {
    count_panics();
    let s = setup();
    let server = s.start();
    let messages = junk_messages();
    for (stage, _) in connections(&server) {
        // A connection is closed after too many requests that are not understood (and some
        // junk ends it on purpose), so each stage gets a fresh one when needed.
        let mut conns = connections(&server);
        let pick = conns.iter().position(|(s, _)| *s == stage).unwrap();
        let (_, mut c) = conns.swap_remove(pick);
        drop(conns);
        for (i, msg) in messages.iter().enumerate() {
            if c.send_raw(msg).is_err() || c.request(&ClientMsg::Ping).is_err() {
                let mut again = connections(&server);
                let pick = again.iter().position(|(s, _)| *s == stage).unwrap();
                c = again.swap_remove(pick).1;
                assert_eq!(
                    PANICS.load(Ordering::SeqCst),
                    0,
                    "panic at {stage} message {i}: {}",
                    String::from_utf8_lossy(msg).chars().take(300).collect::<String>()
                );
                continue;
            }
            assert_eq!(
                PANICS.load(Ordering::SeqCst),
                0,
                "panic at {stage} message {i}: {}",
                String::from_utf8_lossy(msg).chars().take(300).collect::<String>()
            );
        }
    }
    still_fine(&server);
    assert_eq!(PANICS.load(Ordering::SeqCst), 0);
    server.stop();
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Value::from),
        "[a-z0-9 .\"\\-]{0,12}".prop_map(Value::from),
        prop_oneof![Just("Plan.plancraft"), Just("tasks"), Just("set"), Just("unset"), Just("1"), Just("name")].prop_map(Value::from),
    ];
    leaf.prop_recursive(4, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::btree_map(
                prop_oneof![Just("fields".to_string()), Just("lists".to_string()), Just("tasks".to_string()), "[a-z_]{1,8}"],
                inner,
                0..6
            )
            .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn random_changes_never_break_the_server(patches in prop::collection::vec(json_value(), 1..8)) {
        count_panics();
        let s = setup();
        let server = s.start();
        let mut c = signed_in(&server, "ana");
        c.open("Plan.plancraft", ClientId::new(), None, None).unwrap();
        for patch in patches {
            let msg = json!({"type": "change", "change": {"id": uuid_like(), "base": 1, "patch": patch}});
            if c.send_raw(&serde_json::to_vec(&msg).unwrap()).is_err() {
                break;
            }
        }
        let _ = c.request(&ClientMsg::Ping);
        prop_assert_eq!(PANICS.load(Ordering::SeqCst), 0);
        still_fine(&server);
        server.stop();
    }
}

fn uuid_like() -> String {
    uuid::Uuid::new_v4().to_string()
}

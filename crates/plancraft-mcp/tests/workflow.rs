//! End-to-end project workflows driven through `handle_message`.

mod common;

use std::sync::atomic::Ordering;

use common::{Client, created_id};
use serde_json::{Value, json};

fn task<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["tasks"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("task {name} not in {list}"))
}

#[test]
fn full_project_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("website.plancraft");
    let c = Client::new();

    // --- Document -----------------------------------------------------------
    let proj = c.call("new_project", json!({ "name": "Website", "start": "2026-10-19", "path": path.to_str().unwrap(), "manager": "Rob" }));
    assert_eq!(proj["project"]["name"], "Website");
    assert_eq!(proj["project"]["manager"], "Rob");
    assert!(path.exists());
    let docs = c.call("list_documents", json!({}));
    assert_eq!(docs["documents"].as_array().unwrap().len(), 1);
    c.call("update_project", json!({ "hours_per_point": 4, "currency": "€", "company": "ACME", "status_date": "2026-10-20" }));
    let info = c.call("get_project", json!({}));
    assert_eq!(info["settings"]["currency"], "€");
    assert_eq!(info["settings"]["hours_per_point"], 4.0);
    assert_eq!(info["project"]["status_date"], "2026-10-20");

    // --- Resources ----------------------------------------------------------
    let ana = c.call("create_resource", json!({ "name": "Ana Lima", "standard_rate": 80, "group": "Design", "email": "ana@example.com" }));
    assert_eq!(ana["initials"], "AL");
    assert_eq!(ana["max_units_text"], "100%");
    let bob = c.call("create_resource", json!({ "name": "Bob", "standard_rate": 100 }));
    let build_co = c.call("create_resource", json!({ "name": "Build Co", "category": "partner_company", "max_units": "200%" }));
    assert_eq!(build_co["category"], "partner_company");
    assert_eq!(build_co["max_units"], 2.0);
    c.call("update_resource", json!({ "resource": "Build Co", "cost_per_use": 500, "notes": "Contract #12" }));
    c.call_err("create_resource", json!({ "name": "bob" }));

    // --- Bulk WBS -----------------------------------------------------------
    let created = c.call(
        "create_tasks",
        json!({
            "tasks": [
                { "name": "Design", "children": [
                    { "name": "Wireframes", "key": "wf", "duration": "3d", "resources": "Ana Lima" },
                    { "name": "Visual design", "key": "vd", "duration": 2, "predecessors": [{ "task": "wf" }], "resources": "Ana Lima" }
                ]},
                { "name": "Build", "children": [
                    { "name": "Frontend", "key": "fe", "duration": "5d", "predecessors": [{ "task": "vd", "type": "FS" }] },
                    { "name": "Backend", "key": "be", "estimate": { "points": 4 }, "predecessors": [{ "task": "wf", "type": "SS", "lag": "1d" }] }
                ]},
                { "name": "Launch", "milestone": true, "members": ["fe", "be"], "deliverable": "Live site", "deadline": "2026-11-13" }
            ]
        }),
    );
    assert_eq!(created["created"], 7);
    let design = created_id(&created, "Design");
    let wf = created_id(&created, "Wireframes");
    let vd = created_id(&created, "Visual design");
    let fe = created_id(&created, "Frontend");
    let be = created_id(&created, "Backend");
    let launch = created_id(&created, "Launch");
    assert_eq!(task(&created, "Wireframes")["key"], "wf");
    assert_eq!(task(&created, "Wireframes")["wbs"], "1.1");
    assert_eq!(task(&created, "Backend")["wbs"], "2.2");
    assert_eq!(task(&created, "Launch")["level"], 1);

    let wf_t = c.call("get_task", json!({ "task": wf }));
    assert_eq!(wf_t["start"], "2026-10-19 08:00");
    assert_eq!(wf_t["finish"], "2026-10-21 17:00");
    assert_eq!(wf_t["duration"], "3 days");
    assert_eq!(wf_t["duration_minutes"], 1440);
    assert_eq!(wf_t["resources"], "Ana Lima");
    assert_eq!(wf_t["parent"]["id"], design);
    let vd_t = c.call("get_task", json!({ "task": "Visual design" }));
    assert_eq!(vd_t["start"], "2026-10-22 08:00");
    assert_eq!(vd_t["predecessors"], "2");
    let fe_t = c.call("get_task", json!({ "task": fe }));
    assert_eq!(fe_t["start"], "2026-10-26 08:00");
    assert_eq!(fe_t["finish"], "2026-10-30 17:00");
    assert!(fe_t["member_of_milestones"].as_array().unwrap().iter().any(|m| m["id"] == launch));
    let be_t = c.call("get_task", json!({ "task": be }));
    assert_eq!(be_t["start"], "2026-10-20 08:00");
    assert_eq!(be_t["predecessor_links"][0]["type"], "SS");
    assert_eq!(be_t["estimate"]["points"], 4.0);
    assert_eq!(be_t["estimate"]["expected_work_minutes"], 960);
    let launch_t = c.call("get_task", json!({ "task": launch }));
    assert_eq!(launch_t["kind"], "milestone");
    assert_eq!(launch_t["members"].as_array().unwrap().len(), 2);
    assert_eq!(launch_t["finish"], "2026-10-30 17:00");
    assert_eq!(launch_t["deliverable"], "Live site");
    let design_t = c.call("get_task", json!({ "task": design }));
    assert_eq!(design_t["kind"], "summary");
    assert_eq!(design_t["children"].as_array().unwrap().len(), 2);

    // --- Single task creation & links --------------------------------------
    let qa = c.call(
        "create_task",
        json!({ "name": "QA", "parent": "Build", "duration": "2d", "predecessors": [fe], "notes": "Regression suite" }),
    );
    let qa_id = qa["id"].as_u64().unwrap();
    assert_eq!(qa["wbs"], "2.3");
    assert_eq!(qa["start"], "2026-11-02 08:00");
    c.call("link_tasks", json!({ "predecessor": be, "successor": qa_id, "type": "FF", "lag": "-1d" }));
    let deps = c.call("list_dependencies", json!({ "task": qa_id }));
    assert_eq!(deps["count"], 2);
    c.call_err("link_tasks", json!({ "predecessor": qa_id, "successor": wf }));
    let e = c.call_err("link_tasks", json!({ "predecessor": qa_id, "successor": qa_id }));
    assert!(e.contains("itself"), "{e}");
    c.call("unlink_tasks", json!({ "predecessor": be, "successor": qa_id }));
    assert_eq!(c.call("list_dependencies", json!({ "task": qa_id }))["count"], 1);

    // --- Assignments --------------------------------------------------------
    let asg = c.call("assign_resource", json!({ "task": "Backend", "resource": bob["id"] }));
    assert_eq!(asg["assignment"]["work_minutes"], 960);
    assert_eq!(asg["task"]["duration_minutes"], 960);
    let asg2 = c.call("assign_resource", json!({ "task": be, "resource": "Ana Lima", "units": "100%" }));
    let ana_be = asg2["assignment"]["id"].as_u64().unwrap();
    let upd = c.call("update_assignment", json!({ "assignment": ana_be, "contour": "back_loaded" }));
    assert_eq!(upd["assignment"]["contour"], "back_loaded");
    // A back-loaded 100% assignment cannot fit an 8h day at its peak; go back to flat.
    let upd = c.call("update_assignment", json!({ "task": "Backend", "resource": "Ana Lima", "contour": "flat", "units": 1 }));
    assert_eq!(upd["assignment"]["contour"], "flat");
    let list = c.call("list_assignments", json!({ "resource": "Ana Lima" }));
    assert_eq!(list["count"], 3);
    let tmp = c.call("assign_resource", json!({ "task": "QA", "resource": "Build Co", "units": 50 }));
    assert_eq!(tmp["assignment"]["units_text"], "50%");
    c.call("unassign_resource", json!({ "task": "QA", "resource": "Build Co" }));
    c.call_err("assign_resource", json!({ "task": "Build", "resource": "Bob" }));

    // --- Workload & overallocation -----------------------------------------
    let over = c.call("get_overallocations", json!({}));
    assert_eq!(over["overallocated_resources"], 1, "{over}");
    assert_eq!(over["resources"][0]["resource"], "Ana Lima");
    let wl = c.call("get_workload", json!({ "from": "2026-10-19", "to": "2026-10-23", "granularity": "day" }));
    assert_eq!(wl["periods"].as_array().unwrap().len(), 5);
    let ana_wl = wl["resources"].as_array().unwrap().iter().find(|r| r["name"] == "Ana Lima").unwrap();
    assert_eq!(ana_wl["overallocated"], true);
    assert_eq!(ana_wl["load_hours"][0], 8.0);
    assert_eq!(ana_wl["capacity_hours"][0], 8.0);
    assert!(ana_wl["load_hours"][1].as_f64().unwrap() > 8.0);
    assert!(wl["unassigned_total_hours"].as_f64().unwrap() >= 0.0);
    let weekly = c.call("get_workload", json!({}));
    assert_eq!(weekly["granularity"], "week");

    let res_list = c.call("list_resources", json!({ "overallocated_only": true }));
    assert_eq!(res_list["count"], 1);

    let lev = c.call("level_resources", json!({}));
    assert!(!lev["delayed_tasks"].as_array().unwrap().is_empty(), "{lev}");
    assert!(lev["remaining_overallocated_resources"].as_array().unwrap().is_empty(), "{lev}");
    assert!(lev["finish_after"].as_str().unwrap() >= lev["finish_before"].as_str().unwrap());
    assert_eq!(c.call("get_overallocations", json!({}))["overallocated_resources"], 0);
    let cleared = c.call("clear_leveling", json!({}));
    assert!(cleared["cleared_tasks"].as_u64().unwrap() > 0);

    // --- Time tracking ------------------------------------------------------
    let logged = c.call("log_time", json!({ "task": wf, "date": "2026-10-19", "hours": 6, "note": "Sketches" }));
    assert_eq!(logged["entry"]["resource"], "Ana Lima");
    let entry_id = logged["entry"]["id"].as_u64().unwrap();
    c.call("log_time", json!({ "task": "Backend", "resource": "Bob", "date": "2026-10-20", "duration": "4h" }));
    c.call_err("log_time", json!({ "task": "Backend", "date": "2026-10-20", "hours": 1 }));
    let entries = c.call("list_time_entries", json!({ "from": "2026-10-19", "to": "2026-10-31" }));
    assert_eq!(entries["count"], 2);
    assert_eq!(entries["total_hours"], 10.0);
    assert_eq!(entries["hours_by_resource"]["Ana Lima"], 6.0);
    let prog = c.call("update_progress_from_time", json!({}));
    assert!(prog["updated"].as_u64().unwrap() >= 1);
    let wf_t = c.call("get_task", json!({ "task": wf }));
    assert_eq!(wf_t["percent_complete_entered"], 25);
    assert_eq!(wf_t["actual_start"], "2026-10-19 08:00");
    c.call("delete_time_entry", json!({ "entry": entry_id }));
    assert_eq!(c.call("list_time_entries", json!({}))["count"], 1);

    // --- Updates ------------------------------------------------------------
    let upd = c.call(
        "update_task",
        json!({
            "task": vd, "notes": "Use brand kit", "priority": 800, "deadline": "2026-10-23",
            "constraint_type": "snet", "constraint_date": "2026-10-22", "percent_complete": 50, "fixed_cost": 250,
            "show_on_timeline": true, "bar_color": "#3366ff"
        }),
    );
    assert_eq!(upd["priority"], 800);
    assert_eq!(upd["constraint_type"], "snet");
    assert_eq!(upd["deadline"], "2026-10-23 17:00");
    assert_eq!(upd["percent_complete_entered"], 50);
    assert_eq!(upd["bar_color"], "#3366ff");
    assert!(upd["actual_start"].is_string());
    let upd = c.call("update_task", json!({ "task": vd, "deadline": null, "constraint_type": "asap" }));
    assert!(upd["deadline"].is_null());
    let bulk = c.call("update_tasks", json!({ "updates": [{ "task": fe, "notes": "React" }, { "task": "QA", "duration": "3d" }] }));
    assert_eq!(bulk["updated"], 2);
    assert_eq!(c.call("get_task", json!({ "task": "QA" }))["duration_minutes"], 1440);
    let e = c.call_err("update_task", json!({ "task": design, "duration": "4d" }));
    assert!(e.contains("summary"), "{e}");
    let e = c.call_err("update_tasks", json!({ "updates": [{ "task": fe, "notes": "ok" }, { "task": 4242, "notes": "bad" }] }));
    assert!(e.contains("4242"), "{e}");
    assert_eq!(c.call("get_task", json!({ "task": fe }))["notes"], "React");

    // --- Calendars ----------------------------------------------------------
    let cal = c.call("add_calendar_exception", json!({ "name": "Company day", "start": "2026-10-28" }));
    assert_eq!(cal["exception_list"].as_array().unwrap().len(), 1);
    assert_eq!(c.call("get_task", json!({ "task": fe }))["finish"], "2026-11-02 17:00");
    let day = c.call("get_calendar", json!({ "from": "2026-10-28", "to": "2026-10-29" }));
    assert_eq!(day["days"][0]["working"], false);
    assert_eq!(day["days"][1]["hours"], 8.0);
    let part = c.call("create_calendar", json!({ "name": "Part time", "copy_from": "Standard" }));
    let part = c.call("set_work_week", json!({ "calendar": part["id"], "days": ["friday"], "working_times": [] }));
    assert_eq!(part["work_week"]["friday"], json!([]));
    assert_eq!(part["weekly_hours"], 32.0);
    c.call("set_work_week", json!({ "calendar": "Part time", "days": "weekend", "working_times": ["09:00-13:00"] }));
    c.call_err("set_work_week", json!({ "calendar": "Part time", "days": "all", "working_times": [] }));
    c.call("update_resource", json!({ "resource": "Bob", "calendar": "Part time" }));
    c.call("add_resource_exception", json!({ "resource": "Bob", "name": "Vacation", "start": "2026-11-09", "end": "2026-11-13" }));
    let bob_r = c
        .call("set_resource_availability", json!({ "resource": "Bob", "periods": [{ "from": "2026-12-01", "to": null, "units": "50%" }] }));
    assert_eq!(bob_r["calendar"]["name"], "Part time");
    assert_eq!(bob_r["exceptions"][0]["name"], "Vacation");
    assert_eq!(bob_r["availability"][0]["units_text"], "50%");
    c.call("remove_resource_exception", json!({ "resource": "Bob", "name": "Vacation" }));
    assert_eq!(c.call("list_calendars", json!({}))["calendars"].as_array().unwrap().len(), 4);
    c.call_err("delete_calendar", json!({ "calendar": "Standard" }));
    c.call("remove_calendar_exception", json!({ "index": 0 }));
    assert_eq!(c.call("get_task", json!({ "task": fe }))["finish"], "2026-10-30 17:00");

    // --- Baseline, variance, statistics ---------------------------------------
    let bl = c.call("set_baseline", json!({}));
    assert_eq!(bl["tasks_with_baseline"], 8);
    c.call("update_task", json!({ "task": fe, "duration": "6d" }));
    let var = c.call("get_variance", json!({ "only_slipped": true }));
    let fe_var = var["tasks"].as_array().unwrap().iter().find(|t| t["id"] == fe).unwrap();
    assert_eq!(fe_var["finish_variance_minutes"], 480);
    assert_eq!(fe_var["finish_variance"], "1 day");
    let stats = c.call("project_statistics", json!({}));
    assert_eq!(stats["milestones"], 1);
    assert_eq!(stats["summary_tasks"], 2);
    assert!(stats["earned_value"].is_object());
    assert_eq!(stats["milestones_status"][0]["members"], 2);
    let ms = c.call("milestone_status", json!({}));
    assert_eq!(ms["milestones"][0]["name"], "Launch");
    let crit = c.call("critical_path", json!({ "near_critical_slack": "5d" }));
    assert!(crit["critical_count"].as_u64().unwrap() >= 3);
    let sched = c.call_raw("get_schedule", json!({}));
    let table = sched["content"][0]["text"].as_str().unwrap();
    assert!(table.contains("Wireframes") && table.contains("milestone"), "{table}");
    assert_eq!(sched["structuredContent"]["tasks"].as_array().unwrap().len(), 8);
    let late = c.call("list_tasks", json!({ "filter": "milestones" }));
    assert_eq!(late["total"], 1);
    let found = c.call("list_tasks", json!({ "search": "end", "include_schedule": false }));
    assert_eq!(found["total"], 2); // Frontend, Backend

    // --- Milestone membership & outline editing ------------------------------
    let ms = c.call("create_milestone", json!({ "name": "Design approved", "members": [wf, vd], "after": vd }));
    assert_eq!(ms["level"], 2);
    assert_eq!(ms["parent"]["id"], design);
    let ms_id = ms["id"].as_u64().unwrap();
    let m2 = c.call("update_milestone_members", json!({ "milestone": ms_id, "remove": [wf] }));
    assert_eq!(m2["members"].as_array().unwrap().len(), 1);
    let moved = c.call("move_task", json!({ "task": "QA", "after": "Launch" }));
    assert_eq!(moved["level"], 1);
    assert!(moved["parent"].is_null());
    let moved = c.call("move_task", json!({ "task": "QA", "parent": "Build" }));
    assert_eq!(moved["level"], 2);
    c.call_err("move_task", json!({ "task": "Build", "parent": fe }));
    c.call("outdent_tasks", json!({ "tasks": ["QA"] }));
    assert_eq!(c.call("get_task", json!({ "task": "QA" }))["level"], 1);
    c.call("indent_tasks", json!({ "tasks": ["QA"] }));
    assert_eq!(c.call("get_task", json!({ "task": "QA" }))["level"], 2);
    let chain = c.call("create_tasks", json!({ "after": "Launch", "tasks": [{ "name": "Docs A", "level": 1 }, { "name": "Docs B", "level": 2 }, { "name": "Docs C", "level": 2 }] }));
    let a_id = created_id(&chain, "Docs A");
    let b_id = created_id(&chain, "Docs B");
    let c_id = created_id(&chain, "Docs C");
    c.call("link_chain", json!({ "tasks": [b_id, c_id], "lag": "1d" }));
    assert_eq!(c.call("get_task", json!({ "task": c_id }))["predecessor_links"][0]["lag"]["text"], "+1 day");
    let del = c.call("delete_tasks", json!({ "tasks": [a_id] }));
    assert_eq!(del["deleted"].as_array().unwrap().len(), 3);

    // --- History ------------------------------------------------------------
    let hist = c.call("get_history", json!({ "limit": 5 }));
    assert_eq!(hist["undo_stack"][0]["label"], "Delete tasks");
    assert_eq!(hist["undo_stack"][0]["source"], "agent");
    let undone = c.call("undo", json!({}));
    assert_eq!(undone["undone"][0], "Delete tasks");
    assert_eq!(c.call("get_task", json!({ "task": a_id }))["name"], "Docs A");
    c.call("redo", json!({}));
    c.call_err("get_task", json!({ "task": a_id }));
    c.call_err("redo", json!({}));

    // --- Save, close, reopen ------------------------------------------------
    let saved = c.call("save_project", json!({}));
    assert_eq!(saved["saved"], true);
    let before = c.call("list_tasks", json!({}));
    c.call("close_project", json!({}));
    assert_eq!(c.call("list_documents", json!({}))["documents"].as_array().unwrap().len(), 0);
    let reopened = c.call("open_project", json!({ "path": path.to_str().unwrap() }));
    assert_eq!(reopened["project"]["name"], "Website");
    assert_eq!(reopened["project"]["dirty"], false);
    let after = c.call("list_tasks", json!({}));
    assert_eq!(before["tasks"], after["tasks"]);
    assert_eq!(c.call("get_resource", json!({ "resource": "Bob" }))["calendar"]["name"], "Part time");
    c.call_err("undo", json!({}));

    // Save as + dirty-close protection.
    c.call("create_task", json!({ "name": "After reopen" }));
    let e = c.call_err("close_project", json!({}));
    assert!(e.contains("unsaved"), "{e}");
    let copy = dir.path().join("copy");
    let saved = c.call("save_project", json!({ "path": copy.to_str().unwrap() }));
    assert!(saved["path"].as_str().unwrap().ends_with("copy.plancraft"));
    assert!(dir.path().join("copy.plancraft").exists());

    assert!(c.changes.load(Ordering::SeqCst) > 30);
}

#[test]
fn multiple_documents_and_selection() {
    let c = Client::new();
    c.call("new_project", json!({ "name": "Alpha", "start": "2026-10-19" }));
    c.call("create_task", json!({ "name": "A1" }));
    c.call("new_project", json!({ "name": "Beta", "start": "2026-10-19" }));
    c.call("create_task", json!({ "name": "B1" }));
    c.call("create_task", json!({ "name": "B2" }));
    let docs = c.call("list_documents", json!({}));
    assert_eq!(docs["documents"].as_array().unwrap().len(), 2);
    assert_eq!(docs["active"], 2);
    assert_eq!(c.call("list_tasks", json!({ "document": "Alpha" }))["total"], 1);
    assert_eq!(c.call("list_tasks", json!({ "document": 2 }))["total"], 2);
    c.call("set_active_document", json!({ "document": "alpha" }));
    assert_eq!(c.call("list_tasks", json!({}))["total"], 1);
    let e = c.call_err("list_tasks", json!({ "document": "Gamma" }));
    assert!(e.contains("Alpha") && e.contains("Beta"), "{e}");
    c.call("close_project", json!({ "document": 1, "discard_changes": true }));
    assert_eq!(c.call("get_project", json!({}))["project"]["name"], "Beta");
}

#[test]
fn cross_document_external_links() {
    let dir = tempfile::tempdir().unwrap();
    let a_path = dir.path().join("platform.plancraft");
    let b_path = dir.path().join("sub").join("app.plancraft");
    std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
    let c = Client::new();

    c.call("new_project", json!({ "name": "Platform", "start": "2026-10-19", "path": a_path.to_str().unwrap() }));
    let api = c.call("create_task", json!({ "name": "API ready", "duration": "10d" }));
    let api_id = api["id"].as_u64().unwrap();
    assert_eq!(api["finish"], "2026-10-30 17:00");
    c.call("save_project", json!({}));

    c.call("new_project", json!({ "name": "App", "start": "2026-10-19", "path": b_path.to_str().unwrap() }));
    c.call("create_task", json!({ "name": "Integrate", "duration": "2d" }));
    let ext = c.call("add_external_project", json!({ "path": a_path.to_str().unwrap() }));
    assert_eq!(ext["external_project"]["name"], "Platform");
    assert_eq!(ext["external_project"]["path"], "../platform.plancraft");
    let list = c.call("list_external_projects", json!({}));
    assert_eq!(list["external_projects"][0]["exists"], true);
    let tasks = c.call("list_external_tasks", json!({ "external_project": "Platform" }));
    assert_eq!(tasks["tasks"][0]["name"], "API ready");
    assert_eq!(tasks["tasks"][0]["id"], api_id);

    let linked = c.call("link_external_task", json!({ "external_project": "Platform", "task": "API ready", "successor": "Integrate" }));
    assert_eq!(linked["successor"]["start"], "2026-11-02 08:00");
    let integ = c.call("get_task", json!({ "task": "Integrate" }));
    assert_eq!(integ["predecessor_links"][0]["external"], true);
    assert_eq!(integ["predecessor_links"][0]["external_project"], "Platform");
    assert_eq!(integ["predecessors"], format!("Platform\\{api_id}"));
    let ms =
        c.call("create_milestone", json!({ "name": "Platform done", "members": [{ "external_project": "Platform", "task": api_id }] }));
    assert_eq!(ms["finish"], "2026-10-30 17:00");
    c.call_err("link_external_task", json!({ "external_project": "Platform", "task": 999, "successor": "Integrate" }));
    c.call_err("link_external_task", json!({ "external_project": "Nope", "task": 1, "successor": "Integrate" }));
    c.call("save_project", json!({}));

    // The other document changes: its new dates flow in on refresh.
    c.call("update_task", json!({ "document": "Platform", "task": api_id, "duration": "12d" }));
    c.call("save_project", json!({ "document": "Platform" }));
    let r = c.call("refresh_external_links", json!({ "document": "App" }));
    assert_eq!(r["warnings"], json!([]));
    assert_eq!(c.call("get_task", json!({ "document": "App", "task": "Integrate" }))["start"], "2026-11-04 08:00");

    // Reopen the dependent document from disk.
    c.call("save_project", json!({ "document": "App" }));
    c.call("close_project", json!({ "document": "App" }));
    c.call("close_project", json!({ "document": "Platform" }));
    c.call("open_project", json!({ "path": b_path.to_str().unwrap() }));
    let integ = c.call("get_task", json!({ "task": "Integrate" }));
    assert_eq!(integ["start"], "2026-11-04 08:00");
    let deps = c.call("list_dependencies", json!({}));
    assert_eq!(deps["links"][0]["external"], true);
    assert_eq!(deps["milestone_memberships"].as_array().unwrap().len(), 1);

    let removed = c.call("remove_external_project", json!({ "external_project": "Platform" }));
    assert_eq!(removed["links_removed"], 2);
    assert_eq!(c.call("get_task", json!({ "task": "Integrate" }))["start"], "2026-10-19 08:00");
}

#[test]
fn file_access_can_be_disabled() {
    use plancraft_core::workspace::Workspace;
    use plancraft_mcp::{McpServer, ServerOptions};
    let server = McpServer::with_options(Workspace::default().shared(), ServerOptions { allow_file_access: false, ..Default::default() });
    let r = server.call_tool("open_project", &json!({ "path": "/etc/passwd" }));
    assert_eq!(r["isError"], true);
    let r = server.call_tool("new_project", &json!({ "name": "X", "path": "/tmp/x.plancraft" }));
    assert_eq!(r["isError"], true);
    let r = server.call_tool("new_project", &json!({ "name": "X" }));
    assert_eq!(r["isError"], false);
}

#[test]
fn workspace_from_files_handles_new_and_existing() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = dir.path().join("fresh");
    let ws = plancraft_mcp::workspace_from_files(std::slice::from_ref(&fresh)).unwrap();
    {
        let w = ws.lock().unwrap();
        assert_eq!(w.docs.len(), 1);
        assert_eq!(w.docs[0].project.name, "fresh");
        assert!(w.docs[0].path.as_ref().unwrap().ends_with("fresh.plancraft"));
    }
    assert!(!dir.path().join("fresh.plancraft").exists());
    let server = plancraft_mcp::McpServer::new(ws);
    assert_eq!(server.call_tool("save_project", &json!({}))["isError"], false);
    assert!(dir.path().join("fresh.plancraft").exists());
    let ws2 = plancraft_mcp::workspace_from_files(&[dir.path().join("fresh.plancraft")]).unwrap();
    assert!(!ws2.lock().unwrap().docs[0].dirty);
    assert!(plancraft_mcp::workspace_from_files(&[dir.path().join("missing-dir/x.plancraft")]).is_err());
    assert!(plancraft_mcp::workspace_from_files::<&str>(&[]).unwrap().lock().unwrap().docs.is_empty());
}

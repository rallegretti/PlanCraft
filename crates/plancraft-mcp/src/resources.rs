//! MCP resources: read-only views of open documents.
//!
//! * `plancraft://documents` — list of open documents (JSON)
//! * `plancraft://document/{handle}/project.json` — full serialized project
//! * `plancraft://document/{handle}/summary` — human-readable status summary
//! * `plancraft://document/{handle}/schedule` — Gantt-like text table
//!
//! `{handle}` may also be `active`.

use plancraft_core::io;
use plancraft_core::workspace::{Document, Workspace};
use serde_json::{Value, json};

use crate::protocol::{INTERNAL_ERROR, RESOURCE_NOT_FOUND, RpcError};
use crate::server::McpServer;
use crate::views;

const KINDS: [(&str, &str, &str); 3] = [
    ("project.json", "application/json", "Full PlanCraft document (JSON, same format as the .plancraft file)"),
    ("summary", "text/markdown", "Human-readable project status summary"),
    ("schedule", "text/plain", "Gantt-like table of all tasks"),
];

pub fn list(server: &McpServer) -> Value {
    let ws = server.lock();
    let mut out = vec![json!({
        "uri": "plancraft://documents",
        "name": "documents",
        "title": "Open documents",
        "description": "Project documents open in this PlanCraft session",
        "mimeType": "application/json",
    })];
    for d in &ws.docs {
        for (kind, mime, desc) in KINDS {
            out.push(json!({
                "uri": format!("plancraft://document/{}/{kind}", d.handle),
                "name": format!("{}-{kind}", d.title().trim_end_matches('*')),
                "title": format!("{} — {kind}", d.project.name),
                "description": desc,
                "mimeType": mime,
            }));
        }
    }
    json!({ "resources": out })
}

pub fn templates() -> Value {
    json!({
        "resourceTemplates": KINDS.iter().map(|(kind, mime, desc)| json!({
            "uriTemplate": format!("plancraft://document/{{handle}}/{kind}"),
            "name": format!("document-{kind}"),
            "title": desc,
            "description": format!("{desc}. {{handle}} is a document handle from list_documents, or 'active'."),
            "mimeType": mime,
        })).collect::<Vec<_>>()
    })
}

fn find_doc<'a>(ws: &'a Workspace, key: &str) -> Option<&'a Document> {
    if key == "active" {
        return ws.active_doc();
    }
    ws.find(key).and_then(|i| ws.docs.get(i))
}

pub fn read(server: &McpServer, uri: &str) -> Result<Value, RpcError> {
    let not_found = || {
        let mut e = RpcError::new(RESOURCE_NOT_FOUND, format!("Resource not found: {uri}"));
        e.data = Some(json!({ "uri": uri }));
        e
    };
    let ws = server.lock();
    let (mime, text) = if uri == "plancraft://documents" {
        let docs: Vec<Value> = ws.docs.iter().enumerate().map(|(i, d)| views::document_brief(d, ws.active == Some(i))).collect();
        ("application/json", serde_json::to_string_pretty(&json!({ "documents": docs })).unwrap_or_default())
    } else {
        let rest = uri.strip_prefix("plancraft://document/").ok_or_else(not_found)?;
        let (handle, kind) = rest.split_once('/').ok_or_else(not_found)?;
        let doc = find_doc(&ws, handle).ok_or_else(not_found)?;
        match kind {
            "project.json" => ("application/json", io::to_json(&doc.project).map_err(|e| RpcError::new(INTERNAL_ERROR, e.to_string()))?),
            "summary" => ("text/markdown", views::summary_text(doc)),
            "schedule" => {
                let rows: Vec<usize> = (0..doc.project.tasks.len()).collect();
                ("text/plain", views::schedule_table(&doc.project, &rows))
            }
            _ => return Err(not_found()),
        }
    };
    Ok(json!({ "contents": [{ "uri": uri, "mimeType": mime, "text": text }] }))
}

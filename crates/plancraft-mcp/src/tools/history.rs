//! Undo / redo and change history.

use serde_json::json;

use super::*;
use crate::args::*;
use crate::views;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "undo",
            "Undo",
            "Undo the last change(s) to the document (changes made by the user in the app or by the agent).",
            schema(json!({ "steps": integer("Number of steps to undo (default 1).") }), &[]),
            undo,
        ),
        Tool::new("redo", "Redo", "Redo the last undone change(s).", schema(json!({ "steps": integer("Number of steps to redo (default 1).") }), &[]), redo),
        Tool::new(
            "get_history",
            "Change history",
            "List the undoable changes (most recent first) with who made them (user or agent), plus redo availability, revision number and unsaved-changes flag.",
            schema(json!({ "limit": integer("Maximum entries (default 50).") }), &[]),
            get_history,
        )
        .read_only(),
    ]
}

fn step(ctx: &Ctx, a: &Args, redo: bool) -> ToolResult {
    let steps = a.u64("steps")?.unwrap_or(1).clamp(1, 200);
    let out = ctx.with_doc(a, |doc| {
        let mut done = Vec::new();
        for _ in 0..steps {
            let label = if redo {
                doc.redo_label()
            } else {
                doc.undo_label()
            }
            .map(str::to_string);
            let Some(label) = label else { break };
            let ok = if redo { doc.redo() } else { doc.undo() };
            if !ok {
                break;
            }
            done.push(label);
        }
        if done.is_empty() {
            return err(if redo {
                "nothing to redo"
            } else {
                "nothing to undo"
            });
        }
        let mut v = json!({
            "can_undo": doc.can_undo(),
            "can_redo": doc.can_redo(),
            "next_undo": doc.undo_label(),
            "next_redo": doc.redo_label(),
            "revision": doc.revision,
            "project_finish": fmt_dt(doc.project.sched.finish),
        });
        v[if redo { "redone" } else { "undone" }] = json!(done);
        Ok(v)
    })?;
    ctx.mark_changed();
    Ok(out)
}

fn undo(ctx: &Ctx, a: &Args) -> ToolResult {
    step(ctx, a, false)
}

fn redo(ctx: &Ctx, a: &Args) -> ToolResult {
    step(ctx, a, true)
}

fn get_history(ctx: &Ctx, a: &Args) -> ToolResult {
    let limit = a.u64("limit")?.unwrap_or(50) as usize;
    ctx.read(a, |doc| {
        let hist = doc.history();
        Ok(json!({
            "undo_stack": hist.iter().take(limit).map(|(l, s)| json!({ "label": l, "source": views::source_str(*s) })).collect::<Vec<_>>(),
            "undo_steps": hist.len(),
            "can_redo": doc.can_redo(),
            "next_redo": doc.redo_label(),
            "last_change": doc.last_change.as_ref().map(|(l, s)| json!({ "label": l, "source": views::source_str(*s) })),
            "revision": doc.revision,
            "dirty": doc.dirty,
        }))
    })
}

//! Task Board tools: the board's columns and cards. A task's column (Board Status) and whether it
//! shows on the board are task fields (`update_task`: `board_status`, `show_on_board`).

use plancraft_core::ids::BoardColumnId;
use plancraft_core::prelude::*;
use serde_json::{Value, json};

use super::*;
use crate::args::*;

fn column_ref(desc: &str) -> Value {
    json!({ "type": ["string", "integer"], "description": format!("{desc} Column name or id.") })
}

fn percent_prop() -> Value {
    json!({ "type": ["integer", "null"], "description": "Set % Complete: tasks moved into the column take this % complete (0-100); null for none." })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "get_board",
            "Get the Task Board",
            "The Task Board: its columns in order (name, id, Set % Complete, card count) with the cards in each (task id, row, name, % complete, resources, finish, milestone), at most 'limit' per column. A task's card sits in its Board Status column (the first column when it has none); summary tasks and tasks with show_on_board false have no card. Move cards with update_task (board_status) or move_cards.",
            schema(
                json!({
                    "column": column_ref("Only this column."),
                    "limit": integer("Most cards listed per column (default 100; 'count' always gives the total)."),
                }),
                &[],
            ),
            get_board,
        )
        .read_only(),
        Tool::new(
            "move_cards",
            "Move cards on the Task Board",
            "Move tasks' cards to a column (sets their Board Status). A column with a Set % Complete value also sets their % complete. Summary tasks have no card and are refused.",
            schema(json!({ "tasks": task_refs("Tasks to move."), "column": column_ref("Target column.") }), &["tasks", "column"]),
            move_cards,
        ),
        Tool::new(
            "add_board_column",
            "Add a Task Board column",
            "Add a column to the Task Board (to the right of the last one by default). Names are unique; a board has at most 100 columns.",
            schema(
                json!({
                    "name": string("Column name."),
                    "position": integer("Position from 1 (default: last)."),
                    "percent_complete": percent_prop(),
                }),
                &["name"],
            ),
            add_board_column,
        ),
        Tool::new(
            "update_board_column",
            "Change a Task Board column",
            "Rename a Task Board column, move it to another position, or change its Set % Complete value. Omitted settings keep their values.",
            schema(
                json!({
                    "column": column_ref("The column."),
                    "name": string("New name."),
                    "position": integer("New position from 1."),
                    "percent_complete": percent_prop(),
                }),
                &["column"],
            ),
            update_board_column,
        ),
        Tool::new(
            "delete_board_column",
            "Delete a Task Board column",
            "Delete a Task Board column; its cards move to the first remaining column (their tasks are kept). The last column can't be deleted.",
            schema(json!({ "column": column_ref("The column.") }), &["column"]),
            delete_board_column,
        )
        .destructive(),
    ]
}

/// A board column named by a value: a JSON number is an id; text is a name or "B3".
pub fn column_arg(p: &Project, v: &Value, key: &str) -> ToolResult<BoardColumnId> {
    let found = match v {
        Value::String(s) => p.find_board_column(s),
        Value::Number(n) => p.board_column_by_id(&n.to_string()),
        other => return err(format!("'{key}' must be a column name or id, got {other}")),
    };
    found.ok_or_else(|| {
        let text = v.as_str().map_or_else(|| v.to_string(), str::to_string);
        let names: Vec<&str> = p.board_columns.iter().map(|c| c.name.as_str()).collect();
        ToolError(format!("'{key}': no board column '{}' (columns: {})", text.trim(), names.join(", ")))
    })
}

fn percent_arg(a: &Args) -> ToolResult<Option<Option<u8>>> {
    if a.is_clear("percent_complete") {
        return Ok(Some(None));
    }
    match a.u64("percent_complete")? {
        Some(v) if v <= 100 => Ok(Some(Some(v as u8))),
        Some(_) => err("'percent_complete' must be from 0 to 100"),
        None => Ok(None),
    }
}

fn position_arg(a: &Args) -> ToolResult<Option<usize>> {
    match a.u64("position")? {
        Some(0) => err("'position' counts from 1"),
        Some(n) => Ok(Some(usize::try_from(n - 1).unwrap_or(usize::MAX))),
        None => Ok(None),
    }
}

fn board_json(p: &Project, only: Option<BoardColumnId>) -> Value {
    board_json_limited(p, only, 100)
}

fn board_json_limited(p: &Project, only: Option<BoardColumnId>, limit: usize) -> Value {
    let columns: Vec<Value> = p
        .board()
        .into_iter()
        .filter(|(c, _)| only.is_none_or(|o| o == c.id))
        .map(|(c, rows)| {
            let count = rows.len();
            let cards: Vec<Value> = rows
                .iter()
                .take(limit)
                .filter_map(|&i| p.tasks.get(i).map(|t| (i, t)))
                .map(|(i, t)| {
                    json!({
                        "id": t.id.0,
                        "row": i + 1,
                        "name": t.name,
                        "percent_complete": round2(t.sched.percent_complete as f64),
                        "resources": p.format_resource_names(t.id),
                        "finish": fmt_dt(t.sched.finish),
                        "milestone": t.is_milestone(),
                    })
                })
                .collect();
            json!({ "id": c.id.0, "name": c.name, "percent_complete": c.percent_complete, "count": count, "truncated": count > cards.len(), "cards": cards })
        })
        .collect();
    json!({ "columns": columns })
}

fn get_board(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let only = a.get("column").filter(|v| !v.is_null()).map(|v| column_arg(p, v, "column")).transpose()?;
        let limit = a.u64("limit")?.map_or(100, |n| usize::try_from(n).unwrap_or(usize::MAX));
        Ok(board_json_limited(p, only, limit))
    })
}

fn move_cards(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Move Cards",
        |p, _| {
            let ids = task_list_arg(p, a, "tasks")?;
            if ids.is_empty() {
                return err("'tasks' must name at least one task");
            }
            let col = column_arg(p, a.get("column").ok_or_else(|| missing("column"))?, "column")?;
            p.set_board_status(&ids, col)?;
            Ok(col)
        },
        |doc, col| Ok(board_json(&doc.project, Some(col))),
    )
}

fn add_board_column(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Add Board Column",
        |p, _| {
            let name = a.req_str("name")?;
            let percent = percent_arg(a)?.flatten();
            Ok(p.add_board_column(name, position_arg(a)?, percent)?)
        },
        |doc, _| Ok(board_json(&doc.project, None)),
    )
}

fn update_board_column(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Change Board Column",
        |p, _| {
            let col = column_arg(p, a.get("column").ok_or_else(|| missing("column"))?, "column")?;
            if !["name", "percent_complete", "position"].iter().any(|k| a.present(k)) {
                return err("nothing to change: give 'name', 'position' or 'percent_complete'");
            }
            if let Some(name) = a.str("name")? {
                p.rename_board_column(col, name)?;
            }
            if let Some(percent) = percent_arg(a)? {
                p.set_board_column_percent(col, percent)?;
            }
            if let Some(to) = position_arg(a)? {
                p.move_board_column(col, to)?;
            }
            Ok(())
        },
        |doc, ()| Ok(board_json(&doc.project, None)),
    )
}

fn delete_board_column(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Delete Board Column",
        |p, _| {
            let col = column_arg(p, a.get("column").ok_or_else(|| missing("column"))?, "column")?;
            Ok(p.delete_board_column(col)?)
        },
        |doc, moved| {
            let mut v = board_json(&doc.project, None);
            set(&mut v, "cards_moved_to_first_column", json!(moved));
            Ok(v)
        },
    )
}

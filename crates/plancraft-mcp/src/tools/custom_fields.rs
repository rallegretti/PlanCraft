//! Custom field tools: define user fields (text with an optional pick-list, number, cost, date,
//! duration, flag). Task values are set with `update_task`'s `custom` argument.

use plancraft_core::custom::{CustomFieldDef, CustomFieldKind};
use plancraft_core::ids::{CustomFieldId, TaskId};
use plancraft_core::model::{Project, Task};
use serde_json::{Map, Value, json};

use super::*;
use crate::args::*;

const KINDS: [&str; 6] = ["text", "number", "cost", "date", "duration", "flag"];

fn choices_prop() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Text fields only: the allowed values (a pick-list), e.g. [\"To do\", \"In progress\", \"Done\"]. [] allows any text."
    })
}

fn field_ref(desc: &str) -> Value {
    json!({ "type": ["integer", "string"], "description": format!("{desc} Custom field id or name.") })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "list_custom_fields",
            "List custom fields",
            "List the project's custom fields (id, name, type, pick-list choices and how many tasks have a value). Set values with update_task's 'custom' argument.",
            schema(json!({}), &[]),
            list_custom_fields,
        )
        .read_only(),
        Tool::new(
            "create_custom_field",
            "Create a custom field",
            "Define a custom task field: text (optionally limited to a pick-list), number, cost, date, duration or flag (yes/no). Names are unique, ignoring case.",
            schema(
                json!({
                    "name": string("Field name, e.g. \"Status\" or \"Risk\"."),
                    "type": string_enum("Value type (default text).", &KINDS),
                    "choices": choices_prop(),
                }),
                &["name"],
            ),
            create_custom_field,
        ),
        Tool::new(
            "update_custom_field",
            "Update a custom field",
            "Rename a custom field or replace a text field's pick-list. Values already set are kept.",
            schema(json!({ "field": field_ref("Field to change."), "name": string("New name."), "choices": choices_prop() }), &["field"]),
            update_custom_field,
        ),
        Tool::new(
            "delete_custom_field",
            "Delete a custom field",
            "Delete a custom field, its value on every task and its columns in saved tables.",
            schema(json!({ "field": field_ref("Field to delete.") }), &["field"]),
            delete_custom_field,
        )
        .destructive(),
    ]
}

fn kind_str(k: CustomFieldKind) -> &'static str {
    match k {
        CustomFieldKind::Text => "text",
        CustomFieldKind::Number => "number",
        CustomFieldKind::Cost => "cost",
        CustomFieldKind::Date => "date",
        CustomFieldKind::Duration => "duration",
        CustomFieldKind::Flag => "flag",
    }
}

fn field_json(p: &Project, f: &CustomFieldDef) -> Value {
    json!({
        "id": f.id.0,
        "name": f.name,
        "type": kind_str(f.kind),
        "choices": f.choices,
        "tasks_with_value": p.tasks.iter().filter(|t| t.custom.contains_key(&f.id)).count(),
    })
}

fn fields_json(p: &Project) -> Value {
    json!({ "custom_fields": p.custom_fields.iter().map(|f| field_json(p, f)).collect::<Vec<_>>() })
}

/// Resolves a custom field given by id or name.
pub fn resolve_field(p: &Project, v: &Value) -> ToolResult<CustomFieldId> {
    let key = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return err("a custom field is given by id or name"),
    };
    match p.find_custom_field(&key) {
        Some(f) => Ok(f.id),
        None if p.custom_fields.is_empty() => {
            err(format!("custom field '{key}' not found; the project has no custom fields (create_custom_field)"))
        }
        None => err(format!(
            "custom field '{key}' not found; fields: {}",
            p.custom_fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn choices_arg(a: &Args) -> ToolResult<Option<Vec<String>>> {
    match a.array("choices")? {
        None => Ok(None),
        Some(items) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string).ok_or_else(|| ToolError("'choices' must be an array of strings".into())))
            .collect::<ToolResult<Vec<_>>>()
            .map(Some),
    }
}

fn list_custom_fields(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| Ok(fields_json(&doc.project)))
}

fn create_custom_field(ctx: &Ctx, a: &Args) -> ToolResult {
    let name = a.req_str("name")?.to_string();
    let kind = match a.str("type")? {
        None => CustomFieldKind::Text,
        Some(k) => CustomFieldKind::parse(k).ok_or_else(|| ToolError(format!("'type' must be one of {}", KINDS.join(", "))))?,
    };
    let choices = choices_arg(a)?.unwrap_or_default();
    ctx.edit(
        a,
        &format!("Add custom field '{}'", name.trim()),
        |p, _| Ok(p.add_custom_field(&name, kind, &choices)?),
        |doc, id| Ok(doc.project.custom_field(id).map(|f| field_json(&doc.project, f)).unwrap_or(Value::Null)),
    )
}

fn update_custom_field(ctx: &Ctx, a: &Args) -> ToolResult {
    let choices = choices_arg(a)?;
    let name = a.str("name")?.map(str::to_string);
    if choices.is_none() && name.is_none() {
        return err("nothing to change: pass 'name' and/or 'choices'");
    }
    ctx.edit(
        a,
        "Change custom field",
        |p, _| {
            let id = resolve_field(p, a.get("field").unwrap_or(&Value::Null))?;
            if let Some(n) = &name {
                p.rename_custom_field(id, n)?;
            }
            if let Some(c) = &choices {
                p.set_custom_field_choices(id, c)?;
            }
            Ok(id)
        },
        |doc, id| Ok(doc.project.custom_field(id).map(|f| field_json(&doc.project, f)).unwrap_or(Value::Null)),
    )
}

fn delete_custom_field(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Delete custom field",
        |p, _| {
            let id = resolve_field(p, a.get("field").unwrap_or(&Value::Null))?;
            let name = p.custom_field(id).map(|f| f.name.clone()).unwrap_or_default();
            p.delete_custom_field(id)?;
            Ok(name)
        },
        |doc, name| {
            let mut v = fields_json(&doc.project);
            set(&mut v, "deleted", json!(name));
            Ok(v)
        },
    )
}

/// Schema of `update_task`/`create_task`'s `custom` argument.
pub fn custom_values_prop() -> Value {
    json!({
        "type": "object",
        "additionalProperties": { "type": ["string", "number", "boolean", "null"] },
        "description": "Custom field values by field name or id (see list_custom_fields): text (a pick-list entry for fields that have one), a number, a \"YYYY-MM-DD\" date, a duration like \"3d\", or true/false for flags; null clears. Other fields keep their values."
    })
}

/// Applies a `custom` argument to a task.
pub fn apply_custom_values(p: &mut Project, id: TaskId, values: &Map<String, Value>) -> ToolResult<()> {
    for (key, v) in values {
        let field = resolve_field(p, &Value::String(key.clone()))?;
        let text = match v {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
            _ => return err(format!("custom value for '{key}' must be a string, number, boolean or null")),
        };
        let value = p.parse_custom_value(field, &text)?;
        p.set_custom_value(id, field, value)?;
    }
    Ok(())
}

/// A task's custom values as `{field name: text}`.
pub fn custom_values_json(p: &Project, t: &Task) -> Value {
    let mut m = Map::new();
    for (id, v) in &t.custom {
        if let Some(f) = p.custom_field(*id) {
            m.insert(f.name.clone(), json!(p.custom_value_text(*id, v)));
        }
    }
    Value::Object(m)
}

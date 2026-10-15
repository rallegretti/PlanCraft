//! Tool registry and JSON Schema helpers.
//!
//! Tools are grouped by domain; each module exposes `tools()` returning its
//! definitions. Every tool accepts an optional `document` argument.

use serde_json::{Map, Value, json};

use crate::args::{Args, ToolError, ToolResult};
use crate::server::Ctx;

pub mod analysis;
pub mod assignments;
pub mod calendars;
pub mod dependencies;
pub mod documents;
pub mod history;
pub mod milestones;
pub mod resources;
pub mod tasks;
pub mod time;

/// A tool handler.
pub type Handler = fn(&Ctx, &Args) -> ToolResult;

/// An MCP tool definition.
#[derive(Clone)]
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub read_only: bool,
    pub destructive: bool,
    /// When set, the text content of the result is this string field instead of JSON.
    pub text_field: Option<&'static str>,
    pub handler: Handler,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool").field("name", &self.name).finish()
    }
}

impl Tool {
    pub fn new(name: &'static str, title: &'static str, description: &'static str, input_schema: Value, handler: Handler) -> Self {
        Self { name, title, description, input_schema, read_only: false, destructive: false, text_field: None, handler }
    }

    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    pub fn text(mut self, field: &'static str) -> Self {
        self.text_field = Some(field);
        self
    }

    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": {
                "readOnlyHint": self.read_only,
                "destructiveHint": self.destructive,
                "idempotentHint": self.read_only,
                "openWorldHint": false,
            },
        })
    }

    /// Checks for unknown and missing required arguments.
    pub fn validate(&self, args: &Map<String, Value>) -> ToolResult<()> {
        let props = self.input_schema.get("properties").and_then(Value::as_object);
        if let Some(props) = props {
            let unknown: Vec<&String> = args.keys().filter(|k| !props.contains_key(k.as_str()) && !k.starts_with('_')).collect();
            if !unknown.is_empty() {
                return Err(ToolError(format!(
                    "unknown argument(s) {} for tool '{}'; valid arguments: {}",
                    unknown.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", "),
                    self.name,
                    props.keys().cloned().collect::<Vec<_>>().join(", ")
                )));
            }
        }
        if let Some(req) = self.input_schema.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if args.get(r).is_none_or(Value::is_null) {
                    return Err(ToolError(format!("missing required argument '{r}' for tool '{}'", self.name)));
                }
            }
        }
        Ok(())
    }
}

/// All tools, in a stable order.
pub fn all_tools() -> Vec<Tool> {
    let mut v = Vec::new();
    v.extend(documents::tools());
    v.extend(tasks::tools());
    v.extend(milestones::tools());
    v.extend(dependencies::tools());
    v.extend(resources::tools());
    v.extend(assignments::tools());
    v.extend(calendars::tools());
    v.extend(time::tools());
    v.extend(analysis::tools());
    v.extend(history::tools());
    v
}

// ---------------------------------------------------------------------------
// Schema helpers
// ---------------------------------------------------------------------------

/// The `document` selector accepted by every tool.
pub fn document_prop() -> Value {
    json!({
        "type": ["integer", "string"],
        "description": "Target document: handle, file path/name or project name. Default: active document."
    })
}

/// Object schema with the standard `document` property added.
pub fn schema(props: Value, required: &[&str]) -> Value {
    let mut props = match props {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    props.insert("document".into(), document_prop());
    let mut s = json!({ "type": "object", "properties": props, "additionalProperties": false });
    if !required.is_empty() {
        s["required"] = json!(required);
    }
    s
}

pub fn task_ref(desc: &str) -> Value {
    json!({ "type": ["integer", "string"], "description": format!("{desc} Task id (preferred), exact name, or \"#N\" (row N).") })
}

pub fn task_refs(desc: &str) -> Value {
    json!({ "type": "array", "items": { "type": ["integer", "string"] }, "description": format!("{desc} Task ids or names.") })
}

pub fn resource_ref(desc: &str) -> Value {
    json!({ "type": ["integer", "string"], "description": format!("{desc} Resource id or name.") })
}

pub fn calendar_ref(desc: &str) -> Value {
    json!({ "type": ["integer", "string"], "description": format!("{desc} Calendar id or name.") })
}

pub fn date(desc: &str) -> Value {
    json!({ "type": "string", "description": format!("{desc} \"YYYY-MM-DD[ HH:MM]\".") })
}

pub fn date_or_null(desc: &str) -> Value {
    json!({ "type": ["string", "null"], "description": format!("{desc} \"YYYY-MM-DD[ HH:MM]\"; null clears.") })
}

pub fn duration(desc: &str) -> Value {
    json!({ "type": ["string", "number"], "description": format!("{desc} e.g. \"3d\", \"4h\", \"2w\", \"3d?\" (estimated) or a number of days.") })
}

pub fn work(desc: &str) -> Value {
    json!({ "type": ["string", "number"], "description": format!("{desc} e.g. \"16h\", \"2d\" or a number of hours.") })
}

pub fn lag(desc: &str) -> Value {
    json!({ "type": ["string", "number"], "description": format!("{desc} e.g. \"2d\", \"-1d\" (lead), \"50%\", \"3ed\" (elapsed days).") })
}

pub fn link_type() -> Value {
    json!({ "type": "string", "enum": ["FS", "SS", "FF", "SF"], "description": "Dependency type: FS finish-to-start (default), SS start-to-start, FF finish-to-finish, SF start-to-finish." })
}

pub fn units(desc: &str) -> Value {
    json!({ "type": ["string", "number"], "description": format!("{desc} \"50%\" or 0.5 (numbers > 10 are percentages).") })
}

pub fn string(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

pub fn boolean(desc: &str) -> Value {
    json!({ "type": "boolean", "description": desc })
}

pub fn number(desc: &str) -> Value {
    json!({ "type": "number", "description": desc })
}

pub fn integer(desc: &str) -> Value {
    json!({ "type": "integer", "description": desc })
}

pub fn string_enum(desc: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values, "description": desc })
}

pub fn time_ranges(desc: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string", "pattern": "^\\d{1,2}:\\d{2}-\\d{1,2}:\\d{2}$" }, "description": format!("{desc} e.g. [\"08:00-12:00\", \"13:00-17:00\"]; [] = non-working.") })
}

/// Schema of a predecessor list (array form preferred, string form uses row numbers).
pub fn predecessors(desc: &str) -> Value {
    json!({
        "type": ["array", "string"],
        "items": {
            "type": ["integer", "string", "object"],
            "properties": {
                "task": { "type": ["integer", "string"], "description": "Task id/name (or batch key); with external_project: task id in that project." },
                "external_project": { "type": "string" },
                "type": { "type": "string", "enum": ["FS", "SS", "FF", "SF"] },
                "lag": { "type": ["string", "number"] }
            }
        },
        "description": format!("{desc} Array of task ids/names or {{\"task\": 12, \"type\": \"SS\", \"lag\": \"2d\"}} (preferred), or MS Project syntax with ROW numbers, e.g. \"3,5SS+2d\". Replaces existing predecessors; [] removes all.")
    })
}

/// Schema for an estimate object.
pub fn estimate() -> Value {
    json!({
        "type": ["object", "null"],
        "description": "Effort estimate (sizes work before/at first assignment): {\"points\": 5} (complexity points × hours_per_point), {\"optimistic\": \"8h\", \"most_likely\": \"16h\", \"pessimistic\": \"40h\"} (PERT), or {\"work\": \"24h\"}. null clears.",
        "properties": {
            "points": { "type": "number" },
            "optimistic": { "type": ["string", "number"] },
            "most_likely": { "type": ["string", "number"] },
            "pessimistic": { "type": ["string", "number"] },
            "work": { "type": ["string", "number"] }
        }
    })
}

/// Properties shared by create/update task tools.
pub fn task_field_props() -> Map<String, Value> {
    let v = json!({
        "name": string("Task name."),
        "duration": duration("Planned duration (ignored for summary tasks)."),
        "estimated": boolean("Mark the duration as a tentative estimate (shown with '?')."),
        "mode": string_enum("Scheduling mode: 'auto' (engine computes dates) or 'manual' (uses start/finish as entered).", &["auto", "manual"]),
        "active": boolean("Inactive tasks stay in the plan but do not affect the schedule or workload."),
        "task_type": string_enum("Which of units/duration/work stays fixed when another changes.", &["fixed_units", "fixed_duration", "fixed_work"]),
        "effort_driven": boolean("When true, adding resources keeps total work constant and shortens the task."),
        "start": date("Desired start. Auto tasks get a 'Start No Earlier Than' constraint; manual tasks get this as their manual start."),
        "finish": date("Desired finish. Auto tasks get a 'Finish No Earlier Than' constraint; manual tasks get this as their manual finish."),
        "constraint_type": string_enum("Date constraint.", &["asap", "alap", "snet", "snlt", "fnet", "fnlt", "mso", "mfo"]),
        "constraint_date": date_or_null("Date for the constraint (required for all types except asap/alap)."),
        "deadline": date_or_null("Deadline (a warning is raised when the task finishes later; does not move the task)."),
        "priority": integer("Leveling priority 0-1000 (default 500; higher is leveled first)."),
        "percent_complete": number("Progress 0-100. Sets actual start/finish automatically."),
        "actual_start": date_or_null("Actual start date."),
        "actual_finish": date_or_null("Actual finish date."),
        "manual_start": date_or_null("Manually entered start (used by manual tasks)."),
        "manual_finish": date_or_null("Manually entered finish (used by manual tasks)."),
        "calendar": { "type": ["integer", "string", "null"], "description": "Task calendar id or name; null uses the project/resource calendars." },
        "ignore_resource_calendars": boolean("Schedule using only the task/project calendar."),
        "notes": string("Free-form notes."),
        "fixed_cost": number("Fixed cost added to the task's cost."),
        "estimate": estimate(),
        "deliverable": string("For milestones: the output achieved at this milestone."),
        "show_on_timeline": boolean("Show this task on the timeline view."),
        "is_milestone": boolean("Convert to a milestone (true) or a normal task (false)."),
        "predecessors": predecessors("Predecessor links."),
        "resources": string("Resource assignments in 'Resource Names' syntax, e.g. \"Ana[50%], Bob\". Unknown names create new work resources. Replaces existing assignments; \"\" removes all."),
        "bar_color": { "type": ["string", "null"], "description": "Gantt bar color as \"#RRGGBB\"; null resets." }
    });
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Merges two property maps into a schema object.
pub fn schema_with(mut base: Map<String, Value>, extra: Value, required: &[&str]) -> Value {
    if let Value::Object(e) = extra {
        base.extend(e);
    }
    schema(Value::Object(base), required)
}

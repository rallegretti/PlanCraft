//! Resource tools: people, teams, companies, equipment, materials and costs.

use std::collections::HashSet;

use plancraft_core::calendar::CalendarException;
use plancraft_core::ids::ResourceId;
use plancraft_core::model::*;
use plancraft_core::workload;
use serde_json::{Map, Value, json};

use super::*;
use crate::args::*;
use crate::views;

const KINDS: [&str; 3] = ["work", "material", "cost"];
const CATEGORIES: [&str; 8] = ["team_member", "team", "partner_company", "vendor", "contractor", "equipment", "facility", "other"];

fn resource_field_props() -> Map<String, Value> {
    let v = json!({
        "name": string("Resource name."),
        "kind": string_enum("work = people/teams/equipment spending time; material = consumables measured in units; cost = fixed cost items.", &KINDS),
        "category": string_enum("Who or what the resource is.", &CATEGORIES),
        "max_units": units("Maximum capacity (100% = one full-time person; a team of 3 = 300%)."),
        "standard_rate": number("Cost per hour of work (per unit for material resources)."),
        "overtime_rate": number("Overtime cost per hour."),
        "cost_per_use": number("Cost added each time the resource is used on a task."),
        "group": string("Group/department."),
        "email": string("E-mail address."),
        "initials": string("Initials (can be used to reference the resource)."),
        "material_label": string("Unit label for material resources, e.g. \"tons\"."),
        "calendar": { "type": ["integer", "string", "null"], "description": "Base calendar id or name; null uses the project calendar." },
        "notes": string("Notes."),
    });
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn exception_props() -> Value {
    json!({
        "name": string("Name of the exception, e.g. \"Vacation\" or \"Part-time Fridays\"."),
        "start": date("First day."),
        "end": date("Last day (defaults to start)."),
        "working_times": time_ranges("Altered working hours on these days. Omit or [] for non-working time (vacation/holiday)."),
        "yearly": boolean("Repeat every year on the same dates."),
    })
}

pub fn tools() -> Vec<Tool> {
    let mut update_props = resource_field_props();
    update_props.insert("resource".into(), resource_ref("Resource to update."));
    let mut exc = match exception_props() {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    exc.insert("resource".into(), resource_ref("Resource."));
    vec![
        Tool::new(
            "list_resources",
            "List resources",
            "List resources with kind, category, capacity (max units), rate, total/actual work, cost, assignment count and an overallocated flag.",
            schema(
                json!({
                    "kind": string_enum("Only resources of this kind.", &KINDS),
                    "category": string_enum("Only resources of this category.", &CATEGORIES),
                    "group": string("Only resources in this group."),
                    "overallocated_only": boolean("Only overallocated resources."),
                }),
                &[],
            ),
            list_resources,
        )
        .read_only(),
        Tool::new(
            "get_resource",
            "Get resource details",
            "Full details of a resource: rates, calendar, personal exceptions (time off), availability periods, assignments, logged time and overallocated days.",
            schema(json!({ "resource": resource_ref("Resource.") }), &["resource"]),
            get_resource,
        )
        .read_only(),
        Tool::new(
            "create_resource",
            "Create a resource",
            "Create a resource (team member, team, partner company, vendor, contractor, equipment, facility, material or cost item).",
            schema(Value::Object(resource_field_props()), &["name"]),
            create_resource,
        ),
        Tool::new(
            "update_resource",
            "Update a resource",
            "Change resource fields; only the given fields change.",
            schema(Value::Object(update_props), &["resource"]),
            update_resource,
        ),
        Tool::new(
            "delete_resources",
            "Delete resources",
            "Delete resources together with their assignments and logged time.",
            schema(json!({ "resources": { "type": "array", "items": { "type": ["integer", "string"] }, "description": "Resource ids or names." } }), &["resources"]),
            delete_resources,
        )
        .destructive(),
        Tool::new(
            "add_resource_exception",
            "Add resource time off / altered hours",
            "Add a personal calendar exception to a resource: time off (vacation, sick leave) when working_times is omitted, or altered working hours (e.g. part-time days). Tasks assigned to the resource are rescheduled around it.",
            schema(Value::Object(exc), &["resource", "start"]),
            add_resource_exception,
        ),
        Tool::new(
            "remove_resource_exception",
            "Remove resource exception",
            "Remove a personal calendar exception of a resource by index (from get_resource) or name.",
            schema(
                json!({
                    "resource": resource_ref("Resource."),
                    "index": integer("Exception index from get_resource."),
                    "name": string("Exception name (all exceptions with this name are removed)."),
                }),
                &["resource"],
            ),
            remove_resource_exception,
        ),
        Tool::new(
            "set_resource_availability",
            "Set resource availability",
            "Set date-bounded capacity of a resource (e.g. 50% in March, 200% after hiring). Replaces all availability periods; [] clears them. Optionally also set the default max_units.",
            schema(
                json!({
                    "resource": resource_ref("Resource."),
                    "periods": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": { "type": ["string", "null"], "description": "First date (YYYY-MM-DD) or null for open start." },
                                "to": { "type": ["string", "null"], "description": "Last date (YYYY-MM-DD) or null for open end." },
                                "units": { "type": ["string", "number"], "description": "Capacity, e.g. \"50%\" or 0.5." }
                            },
                            "required": ["units"]
                        },
                        "description": "Availability periods; later periods win where they overlap."
                    },
                    "max_units": units("Default capacity outside the periods."),
                }),
                &["resource"],
            ),
            set_resource_availability,
        ),
    ]
}

fn overallocated_set(p: &Project) -> HashSet<ResourceId> {
    workload::overallocations(p).into_iter().map(|(r, _)| r).collect()
}

fn list_resources(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let p = &doc.project;
        let over = overallocated_set(p);
        let kind: Option<ResourceType> = a.get("kind").map(|v| parse_enum(v, "kind", &KINDS)).transpose()?;
        let cat: Option<ResourceCategory> = a.get("category").map(|v| parse_enum(v, "category", &CATEGORIES)).transpose()?;
        let group = a.str("group")?;
        let only_over = a.bool("overallocated_only")?.unwrap_or(false);
        let list: Vec<Value> = p
            .resources
            .iter()
            .filter(|r| kind.is_none_or(|k| r.kind == k))
            .filter(|r| cat.is_none_or(|c| r.category == c))
            .filter(|r| group.is_none_or(|g| r.group.eq_ignore_ascii_case(g)))
            .filter(|r| !only_over || over.contains(&r.id))
            .map(|r| views::resource_row(p, r, &over))
            .collect();
        Ok(json!({ "count": list.len(), "currency": p.currency(), "resources": list }))
    })
}

pub fn resource_view(p: &Project, id: ResourceId) -> Value {
    let Some(r) = p.resource(id) else {
        return Value::Null;
    };
    let days = workload::overallocations(p).into_iter().find(|(rid, _)| *rid == id).map(|(_, d)| d).unwrap_or_default();
    views::resource_detail(p, r, &days)
}

fn get_resource(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.read(a, |doc| {
        let id = resource_arg(&doc.project, a, "resource")?;
        Ok(resource_view(&doc.project, id))
    })
}

fn apply_resource_fields(p: &mut Project, id: ResourceId, a: &Args) -> ToolResult<()> {
    let kind: Option<ResourceType> = a.get("kind").map(|v| parse_enum(v, "kind", &KINDS)).transpose()?;
    let cat: Option<ResourceCategory> = a.get("category").map(|v| parse_enum(v, "category", &CATEGORIES)).transpose()?;
    let max_units = a.get("max_units").map(|v| parse_units_value(v, "max_units")).transpose()?;
    let calendar =
        if a.is_clear("calendar") { Some(None) } else { a.get("calendar").map(|v| resolve_calendar(p, v).map(Some)).transpose()? };
    if let Some(n) = a.str("name")? {
        let n = n.trim();
        if n.is_empty() {
            return err("'name' must not be empty");
        }
        if p.resources.iter().any(|r| r.id != id && r.name.eq_ignore_ascii_case(n)) {
            return err(format!("a resource named '{n}' already exists"));
        }
    }
    let r = p.resource_mut(id).ok_or_else(|| ToolError(format!("resource {} not found", id.0)))?;
    if let Some(n) = a.str("name")? {
        r.name = n.trim().to_string();
    }
    if let Some(k) = kind {
        r.kind = k;
    }
    if let Some(c) = cat {
        r.category = c;
    }
    if let Some(u) = max_units {
        r.max_units = u;
    }
    for (key, field) in
        [("standard_rate", &mut r.standard_rate), ("overtime_rate", &mut r.overtime_rate), ("cost_per_use", &mut r.cost_per_use)]
    {
        if let Some(v) = a.f64(key)? {
            if v < 0.0 {
                return err(format!("'{key}' must not be negative"));
            }
            *field = v;
        }
    }
    for (key, field) in [
        ("group", &mut r.group),
        ("email", &mut r.email),
        ("initials", &mut r.initials),
        ("material_label", &mut r.material_label),
        ("notes", &mut r.notes),
    ] {
        if let Some(v) = a.str(key)? {
            *field = v.to_string();
        }
    }
    if let Some(c) = calendar {
        r.calendar = c;
    }
    Ok(())
}

fn create_resource(ctx: &Ctx, a: &Args) -> ToolResult {
    let name = a.req_str("name")?.trim().to_string();
    if name.is_empty() {
        return err("'name' must not be empty");
    }
    ctx.edit(
        a,
        &format!("Add resource '{name}'"),
        |p, _| {
            if p.resources.iter().any(|r| r.name.eq_ignore_ascii_case(&name)) {
                return err(format!("a resource named '{name}' already exists"));
            }
            let id = p.add_resource(name.clone());
            apply_resource_fields(p, id, a)?;
            Ok(id)
        },
        |doc, id| Ok(resource_view(&doc.project, id)),
    )
}

fn update_resource(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Update resource",
        |p, _| {
            let id = resource_arg(p, a, "resource")?;
            apply_resource_fields(p, id, a)?;
            Ok(id)
        },
        |doc, id| Ok(resource_view(&doc.project, id)),
    )
}

fn delete_resources(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Delete resources",
        |p, _| {
            let mut ids = Vec::new();
            for v in a.list("resources") {
                ids.push(resolve_resource(p, v)?);
            }
            if ids.is_empty() {
                return err("'resources' must not be empty");
            }
            let names: Vec<String> = ids.iter().filter_map(|id| p.resource(*id).map(|r| r.name.clone())).collect();
            p.delete_resources(&ids)?;
            Ok(names)
        },
        |doc, names| Ok(json!({ "deleted": names, "remaining_resources": doc.project.resources.len() })),
    )
}

/// Builds a calendar exception from `name/start/end/working_times/yearly` arguments.
pub fn exception_from_args(a: &Args) -> ToolResult<CalendarException> {
    let start = parse_date(a.get("start").ok_or_else(|| ToolError("missing 'start'".into()))?, "start")?;
    let end = match a.get("end") {
        Some(v) => parse_date(v, "end")?,
        None => start,
    };
    if end < start {
        return err("'end' is before 'start'");
    }
    let working = parse_time_ranges(a.get("working_times"), "working_times")?;
    let name =
        a.str("name")?.map(str::to_string).unwrap_or_else(|| if working.is_empty() { "Time off".into() } else { "Altered hours".into() });
    Ok(CalendarException { name, start, end, working, yearly: a.bool("yearly")?.unwrap_or(false) })
}

fn add_resource_exception(ctx: &Ctx, a: &Args) -> ToolResult {
    let exc = exception_from_args(a)?;
    ctx.edit(
        a,
        &format!("Add '{}' to resource", exc.name),
        |p, _| {
            let id = resource_arg(p, a, "resource")?;
            p.resource_mut(id).ok_or_else(|| missing("resolved"))?.exceptions.push(exc);
            Ok(id)
        },
        |doc, id| Ok(resource_view(&doc.project, id)),
    )
}

fn remove_resource_exception(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Remove resource exception",
        |p, _| {
            let id = resource_arg(p, a, "resource")?;
            let index = a.u64("index")?;
            let name = a.str("name")?.map(str::to_string);
            let r = p.resource_mut(id).ok_or_else(|| missing("resolved"))?;
            let before = r.exceptions.len();
            match (index, name) {
                (Some(i), _) => {
                    if (i as usize) >= r.exceptions.len() {
                        return err(format!("exception index {i} out of range (resource has {})", r.exceptions.len()));
                    }
                    r.exceptions.remove(i as usize);
                }
                (None, Some(n)) => r.exceptions.retain(|e| !e.name.eq_ignore_ascii_case(n.trim())),
                (None, None) => return err("give 'index' or 'name'"),
            }
            if r.exceptions.len() == before {
                return err("no matching exception");
            }
            Ok(id)
        },
        |doc, id| Ok(resource_view(&doc.project, id)),
    )
}

fn set_resource_availability(ctx: &Ctx, a: &Args) -> ToolResult {
    ctx.edit(
        a,
        "Set resource availability",
        |p, _| {
            let id = resource_arg(p, a, "resource")?;
            let max_units = a.get("max_units").map(|v| parse_units_value(v, "max_units")).transpose()?;
            let periods = match a.array("periods")? {
                Some(items) => {
                    let mut out = Vec::new();
                    for it in items {
                        let pa = Args::from_value(it, "each period")?;
                        let from = pa.get("from").map(|v| parse_date(v, "from")).transpose()?;
                        let to = pa.get("to").map(|v| parse_date(v, "to")).transpose()?;
                        if let (Some(f), Some(t)) = (from, to)
                            && t < f
                        {
                            return err("period 'to' is before 'from'");
                        }
                        let units =
                            parse_units_value(pa.get("units").ok_or_else(|| ToolError("each period needs 'units'".into()))?, "units")?;
                        out.push(AvailabilityPeriod { from, to, units });
                    }
                    Some(out)
                }
                None => None,
            };
            if periods.is_none() && max_units.is_none() {
                return err("give 'periods' and/or 'max_units'");
            }
            let r = p.resource_mut(id).ok_or_else(|| missing("resolved"))?;
            if let Some(ps) = periods {
                r.availability = ps;
            }
            if let Some(u) = max_units {
                r.max_units = u;
            }
            Ok(id)
        },
        |doc, id| Ok(resource_view(&doc.project, id)),
    )
}

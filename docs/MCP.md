# PlanCraft MCP server

PlanCraft includes a [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server so AI agents such as
Claude Code can create, inspect and edit PlanCraft projects: tasks and WBS outlines, milestones, dependencies
(including links between documents), resources, assignments, calendars, time tracking, workload, resource
leveling, baselines, statistics and undo history.

The server is implemented in pure Rust in the `plancraft-mcp` crate (hand-rolled JSON-RPC 2.0, `tiny_http` for HTTP).
It supports MCP protocol revisions `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`.

## Ways to connect

| Mode | When to use | Command |
|---|---|---|
| stdio, via the desktop app binary | Work on files without the GUI open | `claude mcp add plancraft -- /path/to/plancraft mcp /path/to/project.plancraft` |
| stdio, standalone binary | Headless / CI / no GUI installed | `claude mcp add plancraft -- plancraft-mcp /path/to/project.plancraft` |
| HTTP, live connection to the running app | See the agent's changes appear live in the GUI and undo them there | `claude mcp add --transport http plancraft http://127.0.0.1:7878/mcp` |

### stdio

The process reads newline-delimited JSON-RPC from stdin and writes responses to stdout (logs go to stderr).

```sh
# Build the standalone server
cargo build --release -p plancraft-mcp

# Register it with Claude Code (one or more documents; a missing file becomes a new, unsaved project at that path)
claude mcp add plancraft -- /abs/path/to/target/release/plancraft-mcp /abs/path/to/project.plancraft

# Several documents at once (cross-project work)
claude mcp add plancraft -- plancraft-mcp ~/plans/platform.plancraft ~/plans/app.plancraft

# No document: the agent can call new_project / open_project
claude mcp add plancraft -- plancraft-mcp
```

Use absolute paths: Claude Code may start the server from a different working directory.
Changes are kept in memory until the agent calls `save_project`.

### Live HTTP connection to the desktop app

When the HTTP server is enabled from the app's UI, the app serves MCP at `http://127.0.0.1:7878/mcp` and shares
its open documents with the agent: every agent edit appears immediately, is labelled as an agent change in the
undo history and can be undone with Ctrl+Z like any other edit.

```sh
claude mcp add --transport http plancraft http://127.0.0.1:7878/mcp
```

The standalone binary can serve HTTP too:

```sh
plancraft-mcp project.plancraft --http 127.0.0.1:7878
```

HTTP details (MCP "Streamable HTTP", JSON responses only):

* `POST /mcp` (or `/`) with a JSON-RPC message or batch → `200 application/json`, or `202 Accepted` when the body
  only contained notifications. An `Mcp-Session-Id` header is issued on `initialize`.
* `GET /mcp` → `405` (no server-initiated SSE stream); `DELETE /mcp` → `200`.
* Security: bind to `127.0.0.1` (the default). Requests whose `Origin` header is not a localhost origin are rejected
  with `403`, and when bound to a loopback address the `Host` header must name a loopback host (DNS-rebinding
  protection). There is no authentication, so do not bind to a public interface.

### Command line

```text
plancraft-mcp [FILE...] [--http [ADDR]] [--no-file-access]

  FILE...             .plancraft documents to open
  --http [ADDR]       serve HTTP on ADDR (default 127.0.0.1:7878) instead of stdio
  --no-file-access    refuse tools that open/create/link arbitrary paths
                      (saving open documents to their own path still works)
```

## Embedding (for the desktop app)

```rust
use std::sync::Arc;
use plancraft_mcp::{McpServer, ServerOptions, serve_http, run_stdio, workspace_from_files, HttpHandle};

// Live connection: share the app's workspace and repaint on agent changes.
let server = McpServer::with_options(shared_workspace.clone(), ServerOptions::default())
    .with_change_callback(Arc::new(move || egui_ctx.request_repaint()));
let handle: HttpHandle = serve_http(Arc::new(server), "127.0.0.1:7878")?; // stops on drop / handle.shutdown()
println!("{}", handle.url()); // http://127.0.0.1:7878/mcp

// `plancraft mcp FILE...` subcommand: stdio server.
let ws = workspace_from_files(&files)?;           // Result<SharedWorkspace, String>
run_stdio(&McpServer::new(ws))?;                    // std::io::Result<()>
```

`McpServer::handle_message(serde_json::Value) -> Option<serde_json::Value>` is the transport-agnostic entry point
(single messages and batches; `None` for notifications). The workspace mutex is held only while one request is
processed. Every mutation goes through `Document::try_edit(label, ChangeSource::Agent, …)`: it is undoable,
triggers recalculation, bumps the document revision and is rolled back entirely if the tool fails.

## Conventions for agents

These are also sent to the client as `instructions` in the `initialize` result.

* **Document selection** — every tool accepts an optional `document`: handle number (from `list_documents`),
  file path, file name/stem, or project name. Default: the active document.
* **Task references** — unique `id` (number; stable for the task's lifetime, preferred), exact name, or `"#N"`
  for row N. Outputs give both `id` and `row` (current position), plus `wbs`, outline `level`, kind
  (task/summary/milestone), critical flag, slack, % complete, resource names and predecessor text.
  Resources and calendars are referenced by id or name.
* **Dates** — `"YYYY-MM-DD"` or `"YYYY-MM-DD HH:MM"`. A bare date means the start of the working day (end of
  the working day for finish-type fields such as `finish`, `deadline`, `actual_finish`). Outputs use
  `"YYYY-MM-DD HH:MM"`.
* **Durations** — `"3d"`, `"4h"`, `"2w"`, `"1mo"`, `"1.5 days"`, `"3d?"` (estimated) or a number of working
  days. **Work** — `"16h"`, `"2d"` or a number of hours. **Lags** — `"2d"`, `"-1d"` (lead), `"50%"`, `"3ed"`
  (elapsed days). **Units** — `"50%"` or `0.5` (numbers above 10 are read as percentages).
  Outputs include human strings and machine values, e.g. `"duration": "5 days", "duration_minutes": 2400`.
* **Predecessors** — preferably an array of ids/names or `{"task": 12, "type": "SS", "lag": "2d"}` objects
  (`{"external_project": "Platform", "task": 7}` for cross-document links); a string uses Microsoft Project's
  Predecessors-column syntax with **row numbers** (`"3,5SS+2d"`).
* **Bulk creation** — `create_tasks` builds a whole WBS in one call and one undo step: nest with `children`
  (or flat items with relative `level`), give items a `key` and reference keys in other items' `predecessors`
  or milestone `members`.
* **Errors** — tool failures are returned as results with `isError: true` and an explanation (unknown
  arguments are reported with the list of valid ones); nothing is changed when a tool fails.

Example `create_tasks` call:

```json
{
  "tasks": [
    { "name": "Design", "children": [
      { "name": "Wireframes", "key": "wf", "duration": "3d", "resources": "Ana" },
      { "name": "Visual design", "key": "vd", "duration": "2d", "predecessors": [{ "task": "wf" }] }
    ]},
    { "name": "Build", "children": [
      { "name": "Frontend", "key": "fe", "duration": "5d", "predecessors": [{ "task": "vd" }] },
      { "name": "Backend", "key": "be", "estimate": { "points": 4 }, "predecessors": [{ "task": "wf", "type": "SS", "lag": "1d" }] }
    ]},
    { "name": "Launch", "milestone": true, "members": ["fe", "be"], "deliverable": "Live site" }
  ]
}
```

## Resources and prompts

| URI | Content |
|---|---|
| `plancraft://documents` | Open documents (JSON) |
| `plancraft://document/{handle}/project.json` | Full project in the `.plancraft` JSON format |
| `plancraft://document/{handle}/summary` | Markdown status summary (dates, progress, milestones, warnings) |
| `plancraft://document/{handle}/schedule` | Gantt-like text table |

`{handle}` may be `active`. Prompts: `plan_project` (goal, start, team) and `status_report` (document).

## Tools

All tools also accept the optional `document` argument (omitted below). Mutating tools return the updated
entity; read-only tools are annotated with `readOnlyHint`.

### Documents

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `list_documents` *(read-only)* | – | List the project documents currently open in this PlanCraft session, with their handle numbers, file paths, unsaved-change flag and which one is active. Use the handle as the 'document' argument of other tools. |
| `new_project` | author, company, manager, **name**, notes, overwrite, path, start | Create a new empty project document and make it the active document. Optionally give a file path to save it immediately (required before it can be linked from other documents). |
| `open_project` | **path** | Open a .plancraft project file (or activate it if it is already open) and make it the active document. |
| `save_project` | path | Save the document to its file. With 'path', save as a new file (relative links to external projects are rewritten so they keep working). |
| `close_project` | discard_changes | Close a document. Fails when it has unsaved changes unless discard_changes is true. |
| `set_active_document` | – | Make another open document the active one (the default target of all tools and the document shown in the app). |
| `get_project` *(read-only)* | – | Get project information (name, dates, manager, file), settings (hours per day, currency, points, scheduling options), calendars and key statistics (task counts, late/critical tasks, work, cost, % complete, warnings). |
| `update_project` | author, calendar, company, critical_slack, currency, days_per_month, default_finish_time, default_start_time, duration_unit, hours_per_day, hours_per_point, hours_per_week, manager, minutes_per_point, name, new_tasks_auto, notes, progress_from_time, start, status_date, use_resource_calendars | Update project properties and settings. Only the given fields change. |

### Tasks

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `list_tasks` *(read-only)* | filter, include_schedule, limit, max_level, offset, parent, search | List tasks in outline order with id, row, WBS, level, kind, duration, start/finish, % complete, critical flag, slack, work, predecessors and resources. Filter by category, search text or subtree. |
| `get_task` *(read-only)* | **task** | Get every detail of one task: schedule (early/late dates, slack), constraint, deadline, progress and actuals, estimate, cost, predecessors and successors, assignments, milestone membership, parent/children, baseline and variance, notes and warnings. |
| `create_task` | active, actual_finish, actual_start, after, bar_color, before, calendar, constraint_date, constraint_type, deadline, deliverable, duration, effort_driven, estimate, estimated, finish, fixed_cost, ignore_resource_calendars, index, is_milestone, manual_finish, manual_start, mode, **name**, notes, parent, percent_complete, predecessors, priority, resources, show_on_timeline, start, task_type | Create one task. Position it with parent/after/before/index (default: appended at the top level). New tasks default to an estimated 1-day duration; set 'duration', 'estimate', 'predecessors', 'resources' etc. in the same call. For many tasks use create_tasks. |
| `create_tasks` | after, before, index, parent, **tasks** | Create a whole work breakdown structure in one call (one undo step). Items nest via 'children' (or flat items with 'level'); each item accepts every create_task field. Use 'key' on items and reference them in other items' 'predecessors' (e.g. [{"task": "design", "type": "FS"}]) or milestone 'members'. Returns the created ids. |
| `update_task` | active, actual_finish, actual_start, bar_color, calendar, constraint_date, constraint_type, deadline, deliverable, duration, effort_driven, estimate, estimated, finish, fixed_cost, ignore_resource_calendars, is_milestone, manual_finish, manual_start, mode, name, notes, percent_complete, predecessors, priority, resources, show_on_timeline, start, **task**, task_type | Change any editable field of a task; only the given fields change. 'start'/'finish' create SNET/FNET constraints for auto-scheduled tasks. Returns the updated task. |
| `update_tasks` | **updates** | Apply updates to several tasks in one call (one undo step). Each item is {"task": id, ...any update_task fields}. |
| `delete_tasks` | **tasks** | Delete tasks together with their subtasks, links, assignments and time entries. |
| `move_task` | after, before, index, parent, **task** | Move a task (with its subtasks) in the outline: before/after another task (taking that task's level), as the last child of a parent, or to a row index. |
| `indent_tasks` | **tasks** | Demote tasks one outline level, making them subtasks of the task above. |
| `outdent_tasks` | **tasks** | Promote tasks one outline level. |

### Milestones

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `create_milestone` | after, before, date, deadline, deliverable, index, members, **name**, notes, parent, predecessors, show_on_timeline | Create a milestone: a zero-duration checkpoint that is reached when all of its member tasks (local or in other documents) and predecessors finish. Members do not need to be linked individually. |
| `update_milestone_members` | add, **milestone**, remove | Add and/or remove member tasks of a milestone (local tasks or tasks in external projects). |
| `milestone_status` *(read-only)* | – | Status of every milestone: date, deadline, baseline, member count and completion, work-weighted progress, late and complete flags. |

### Dependencies & cross-project links

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `link_tasks` | lag, **predecessor**, **successor**, type | Create (or update) a dependency predecessor → successor with a type (FS/SS/FF/SF) and optional lag/lead. Circular dependencies are rejected. |
| `unlink_tasks` | predecessor, successor, tasks | Remove the dependency predecessor → successor, or with 'tasks' remove all links among the given tasks (a single task: all of its links). |
| `link_chain` | lag, **tasks**, type | Link the tasks in the given order (each one a predecessor of the next), like Microsoft Project's 'Link Tasks'. |
| `list_dependencies` *(read-only)* | task | List dependency links (all, or those of one task) with predecessor/successor ids, names, rows, type and lag. External predecessors are included. |
| `add_external_project` | **path** | Register another .plancraft document so its tasks can be used as predecessors or milestone members (cross-project dependencies). Save the current document first so a relative path can be stored. |
| `remove_external_project` | **external_project** | Remove an external project and every link/milestone membership that references its tasks. |
| `list_external_projects` *(read-only)* | – | List the external projects registered in this document, with resolved paths, whether the file exists, last sync time and the cached tasks currently referenced. |
| `list_external_tasks` *(read-only)* | **external_project**, search | Read the other document from disk and list its tasks (id, name, dates, % complete, milestone flag) so you can pick one to link. |
| `link_external_task` | **external_project**, lag, milestone, successor, **task**, type | Use a task from an external project as predecessor of a local task (successor) and/or as a member of a local milestone. |
| `refresh_external_links` | – | Re-read every external project from disk, update the cached dates of referenced tasks and recalculate. Returns warnings for missing files or tasks. |

### Resources

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `list_resources` *(read-only)* | category, group, kind, overallocated_only | List resources with kind, category, capacity (max units), rate, total/actual work, cost, assignment count and an overallocated flag. |
| `get_resource` *(read-only)* | **resource** | Full details of a resource: rates, calendar, personal exceptions (time off), availability periods, assignments, logged time and overallocated days. |
| `create_resource` | calendar, category, cost_per_use, email, group, initials, kind, material_label, max_units, **name**, notes, overtime_rate, standard_rate | Create a resource (team member, team, partner company, vendor, contractor, equipment, facility, material or cost item). |
| `update_resource` | calendar, category, cost_per_use, email, group, initials, kind, material_label, max_units, name, notes, overtime_rate, **resource**, standard_rate | Change resource fields; only the given fields change. |
| `delete_resources` | **resources** | Delete resources together with their assignments and logged time. |
| `add_resource_exception` | end, name, **resource**, **start**, working_times, yearly | Add a personal calendar exception to a resource: time off (vacation, sick leave) when working_times is omitted, or altered working hours (e.g. part-time days). Tasks assigned to the resource are rescheduled around it. |
| `remove_resource_exception` | index, name, **resource** | Remove a personal calendar exception of a resource by index (from get_resource) or name. |
| `set_resource_availability` | max_units, periods, **resource** | Set date-bounded capacity of a resource (e.g. 50% in March, 200% after hiring). Replaces all availability periods; [] clears them. Optionally also set the default max_units. |

### Assignments

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `assign_resource` | contour, cost, **resource**, **task**, units, work | Assign a resource to a task (or update the existing assignment). Units default to 100%. Without 'work', work follows the task duration, or the task's estimate for the first assignment of an estimated task. Effort-driven tasks redistribute work among resources. |
| `update_assignment` | actual_work, assignment, contour, cost, resource, task, units, work | Change an assignment's units, work, actual work, contour or cost. Identify it by 'assignment' id or by 'task' + 'resource'. |
| `unassign_resource` | assignment, resource, task | Remove a resource from a task. Identify the assignment by id or by 'task' + 'resource'. |
| `list_assignments` *(read-only)* | resource, task | List assignments (all, or for a task or a resource) with units, work, actual and remaining work, cost and dates. |

### Calendars

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `list_calendars` *(read-only)* | – | List working-time calendars with weekly hours, number of exceptions and how many tasks/resources use them. The project calendar is flagged. |
| `get_calendar` *(read-only)* | calendar, from, to | Show a calendar's work week and exceptions (holidays, altered days). With from/to, also list the effective working times of each day in that range (max 400 days). |
| `create_calendar` | copy_from, **name**, set_as_project_calendar, week | Create a calendar, copied from another calendar or from the standard Monday–Friday 08:00–12:00/13:00–17:00 week. Optionally set its work week in the same call. |
| `set_work_week` | calendar, days, name, week, working_times | Set the regular working times of some weekdays of a calendar. Give 'days' (e.g. ["monday", "friday"], or "weekdays", "weekend", "all") with 'working_times' ([] = non-working), and/or a full 'week' object. Can also rename the calendar. |
| `add_calendar_exception` | calendar, end, name, **start**, working_times, yearly | Add an exception to a calendar: non-working days (holiday, shutdown) when working_times is omitted, or special working hours for those days. Set yearly for recurring holidays. |
| `remove_calendar_exception` | calendar, index, name | Remove a calendar exception by index (from get_calendar) or by name. |
| `delete_calendar` | **calendar** | Delete a calendar. Tasks and resources using it fall back to the project calendar. The project calendar cannot be deleted. |

### Time tracking

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `log_time` | date, duration, hours, note, resource, **task**, update_progress | Record time a resource spent on a task (timesheet entry). Sets the task's actual start. Give 'hours' or 'duration'. If the task has exactly one assigned resource, 'resource' may be omitted. |
| `list_time_entries` *(read-only)* | from, limit, resource, task, to | List logged time with optional task/resource/date filters, with totals per resource and per task. |
| `delete_time_entry` | **entry** | Delete a logged time entry by id. |
| `update_progress_from_time` | – | Set each task's % complete from tracked work (actual ÷ planned), capped at 99% until the task is marked finished. |

### Analysis

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `get_schedule` *(read-only)* | hide_completed, max_level | Compact Gantt-like table of the whole outline: row, id, WBS, indented name, duration, start, finish, % complete, predecessors (row syntax), resources and flags (summary/milestone/critical/manual/warning). Best first call to understand a project. |
| `critical_path` *(read-only)* | near_critical_slack | List the critical tasks (zero or critical-threshold total slack) in start order, plus optional near-critical tasks, with the project finish date. |
| `get_workload` *(read-only)* | from, granularity, include_assignments, resources, to | Time-phased workload per resource: planned load vs capacity (hours) and utilization per day/week/month, overallocated periods and days, total/actual work, complexity points, and estimated work of unassigned tasks. Defaults to the project span by week. |
| `get_overallocations` *(read-only)* | max_days, resource | Days on which resources have more work than capacity, with load, capacity and the tasks causing it. |
| `level_resources` | clear_existing, max_delay_days, resources | Resolve overallocations by delaying tasks (leveling delay), visiting tasks by start, priority and slack. Started, manual and must-start/finish tasks are not moved. Undoable. |
| `clear_leveling` | – | Remove all leveling delays. |
| `project_statistics` *(read-only)* | – | Project statistics: dates, duration, work, cost, % complete, task counts (complete, in progress, late, critical), overallocated resources, baseline variance, milestone status and earned value (SPI/CPI, needs a baseline). |
| `set_baseline` | tasks | Save the current start, finish, duration, work and cost as the baseline of all tasks (or the given tasks) for later variance tracking. |
| `clear_baseline` | tasks | Remove the baseline from all tasks (or the given tasks). |
| `get_variance` *(read-only)* | only_slipped, tasks | Compare current schedule with the baseline per task: start/finish variance (working time), duration, work and cost variance. |

### History

| Tool | Arguments (required in **bold**) | Description |
|---|---|---|
| `undo` | steps | Undo the last change(s) to the document (changes made by the user in the app or by the agent). |
| `redo` | steps | Redo the last undone change(s). |
| `get_history` *(read-only)* | limit | List the undoable changes (most recent first) with who made them (user or agent), plus redo availability, revision number and unsaved-changes flag. |

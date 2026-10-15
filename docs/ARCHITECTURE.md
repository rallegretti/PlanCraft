# PlanCraft architecture

PlanCraft is a Cargo workspace with three crates:

```
plancraft-app  ──►  plancraft-mcp  ──►  plancraft-core
     └──────────────────────────────────────┘
```

## plancraft-core

This crate is pure logic with no UI. Every other component builds on it.

| Module | Responsibility |
|---|---|
| `model` | `Project`, `Task`, `Resource`, `Assignment`, `TimeEntry`, `ExternalProject`, `TaskRef`, `Dependency`, `Constraint`, `Estimate`, `Baseline`, and computed `TaskSchedule` / `ProjectSchedule` |
| `ids` | Stable unique ids (`TaskId`, `ResourceId`, …) and their monotonic counters |
| `duration` | Working-minute durations: formatting and parsing (`3d`, `4h`, `2w`, `3d?`) and lags (`2d`, `-1d`, `50%`, `3ed`) |
| `calendar` | Working weeks, exceptions (including yearly ones), and `WorkCal` date arithmetic: add/sub working time, snapping, and working minutes between two times |
| `schedule` | The CPM engine (`recalculate`) |
| `ops` | All editing operations: outline, links, milestones, the duration/work/units rules, assignments, progress, time logging, baselines, calendars, and text parsing for the table columns |
| `workload` | Time-phased distribution (with contours), capacity, overallocation, and workload reports |
| `leveling` | Serial resource leveling that writes `leveling_delay` |
| `report` | Statistics, milestone status, lateness, earned value |
| `io` | `.plancraft` JSON load/save, relative paths, external-link refresh |
| `workspace` | Open documents with undo/redo snapshots and change attribution (user vs agent) |
| `sample` | The demo project |

### Time model

- Everything is stored in **working minutes**. Days, weeks and months are display units derived from `TimeSettings` (default 8 h/day, 40 h/week, 20 days/month).
- Dates are `NaiveDateTime` in the project's local time.
- A task that finishes at the end of a working day finishes at **17:00, not at 08:00 the next day**. Successors snap forward to the next working minute.

### Calendars

A `WorkCal` is the **intersection** of one or more layers. Each layer is a base calendar plus personal exceptions. A task's scheduling calendar is built like this:

- It has the task calendar, if one is set.
- For each assigned *work* resource, it adds that resource's base calendar plus its time off. This step applies unless `ignore_resource_calendars` is set or the project option is off.
- If the combination contains no working time at all, it falls back to the task or project calendar.

So a vacation of any assigned person pauses the task. Lags and summary durations use the project calendar.

### Scheduling (`schedule::recalculate`)

1. **Mark summaries.** A task with deeper rows below it is a summary task.
2. **Roll up tracking.** Work, actual work and cost per leaf come from assignments, time entries and estimates.
3. **Build the leaf graph.** It contains:
   - Each task's own predecessors.
   - Its ancestors' predecessors (a link on a summary applies to all of its children).
   - Milestone **members**, as implicit finish-to-start predecessors.
   - Summary predecessors, expanded to their leaves. An SS or SF link from a summary uses the earliest leaf start.
   - External predecessors, read from cached snapshots.
4. **Topological sort** (Kahn). Cycles are reported and broken.
5. **Forward pass:**
   - Applies all four link types with lags.
   - Applies constraints: SNET/FNET push the task later; MSO/MFO pin it; SNLT/FNLT are honored and flagged when violated.
   - Adds leveling delays.
   - Uses actual start and finish when they are set.
   - Manual tasks keep their entered dates.
6. **Backward pass.** It computes late dates, limited by the project finish, deadlines and constraints.
7. **ALAP pass.** As-late-as-possible tasks are pinned to their late start, and the passes run again so successors stay consistent.
8. **Slack.** Total and free slack are computed per leaf. A task is critical when total slack ≤ the threshold.
9. **Roll up summaries and the project:** dates, work, cost and duration-weighted % complete.

### Duration, work and units

Assignments follow Microsoft Project's task types (Fixed Units, Fixed Duration, Fixed Work) and the effort-driven rule (`ops::assign`, `set_duration`, `set_assignment_*`).

A task with an **estimate** carries `sched.work` before any assignment. The first work assignment of an estimated task derives its work from the estimate, and its duration from that work divided by the units.

### Cross-document links

`ExternalProject` stores:

- the other document's project id
- its path (relative to this document when possible)
- **snapshots** of referenced tasks (dates, % complete)

Scheduling only ever reads the snapshots, so a file opens and schedules even when the linked file is missing. `io::refresh_external` reloads the linked files and updates the snapshots. The workspace calls it when a document opens; you can also trigger it with *Refresh Links*. `Document::save_as` rewrites relative paths so links survive moving a file.

### Undo and attribution

`Document::edit` and `Document::try_edit` snapshot the project, apply the change, recalculate and bump `revision`. `try_edit` rolls back on error. Each snapshot records whether the user or an agent made the change. Projects are small, so whole-project snapshots are simple and reliable.

## plancraft-mcp

The MCP server is a hand-rolled JSON-RPC 2.0 server. It has no async runtime, which keeps it small, predictable and pure Rust.

- **`server`:** initialize and version negotiation, `tools/*`, `resources/*`, `prompts/*`.
  - Tool errors come back as `isError` results.
  - Each request locks the shared workspace briefly.
  - The change callback runs after the lock is released.
- **`tools/*`:** 67 tools grouped by domain.
  - All mutations go through `Document::try_edit(…, ChangeSource::Agent, …)`.
  - Unknown arguments are rejected so agents find their typos.
- **Transports:**
  - `run_stdio`: newline-delimited JSON.
  - `serve_http`: Streamable HTTP via `tiny_http`, on a background thread. It rejects non-local `Origin` headers, and checks `Host` on loopback addresses.
- **`cli`:** shared by the `plancraft-mcp` binary and `plancraft mcp`.

## plancraft-app

The desktop app uses egui and eframe 0.36 with the wgpu renderer.

- **`app`:** layout (title band, command bar, document tabs, status bar, vertical view label), keyboard shortcuts, command execution, dialogs, the unsaved-changes prompt, and agent-change detection.
- **`menubar`:** the title band and the two-row command bar (a view selector spanning both rows, plain tabs with a File dropdown, and a single toolbar row).
- **`backstage`:** the Start Page (New, Open, Recent, Info).
- **`commands`:** the `Command` enum shared by the menu bar, toolbar, context menus and shortcuts.
- **`widgets::grid`:** a virtualized spreadsheet grid with in-place text and choice editors, column resizing, keyboard navigation, and blank rows for entering new items.
- **`widgets::timescale`:** the two-tier timescale.
- **`views/*`:** the ten views. The Gantt view combines the task grid with `gantt_chart`, which draws bars, links, milestone connectors and baselines and handles drag-to-move, resize, progress and link.
- **`columns`:** task table definitions. Each column defines how to display, edit and commit its value. Tables: Entry, Schedule, Tracking, Work, Cost, Estimates, Variance, Summary.
- **`dialogs/*`:** Task, Resource and Project Information, Change Working Time, Assign Resources, External Links, Statistics, History, About, and the MCP panel.
- **`mcp_host`:** starts and stops the HTTP MCP server on the app's `SharedWorkspace`, and repaints the window when agents make changes.
- **`logo`:** the mark as geometry, painted with egui and rasterized for the window icon.

The UI locks the workspace once per frame. Views either edit the active `Document` directly or emit `Command`s that the app executes after layout.

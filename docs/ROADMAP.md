# Roadmap

## Done (v0.1)

- **Core model:** tasks, summaries, milestones with deliverables and member tasks, resources (work, material and cost; people, teams, partners, vendors, …) and assignments
- **Scheduling engine:**
  - calendar-aware CPM with FS, SS, FF and SF links and working, elapsed or percent lags
  - constraints, deadlines and manual tasks
  - ALAP, inactive tasks, actuals, slack and the critical path
- **Calendars:** project, task and resource calendars, exceptions (including yearly ones), resource time off and availability periods
- **Cross-file dependencies:** links to tasks in other documents, with cached snapshots and refresh
- **Workload:** estimates (complexity points, three-point PERT and work), time-phased workload, overallocation detection and resource leveling
- **Tracking:** time entries, the weekly timesheet, progress from tracked time, baselines, variance and earned value
- **Desktop app:** two-row command bar, ten views, the major dialogs, undo/redo with history, multiple open documents, CSV export
- **Custom fields and tables:**
  - Custom task fields: text with an optional pick-list, number, cost, date, duration and flag.
  - Table columns you can insert, hide and move, saved with the plan.
  - Sort within the outline, and group by any field.
- **MCP server:** 71 tools, resources and prompts over stdio and Streamable HTTP. A live mode works inside the running app (bearer token per session, `plancraft mcp --attach` bridge), and the app binary has a headless `plancraft mcp` mode.
- **Robustness:** no panics in shipped code (clippy-enforced), guarded edits with rollback, limits on hostile files and arguments, atomic saves, recovery copies, owner-only files.
- **Quality gates:** `cargo xtask ci`, run locally on Linux, macOS and Windows (fmt, clippy, tests, cargo deny), a scheduler speed gate, hostile-input sweeps and parity tests against Microsoft's published scheduling examples.

## Next

- **Interchange:** import CSV and Excel (pure Rust).
- **Network Diagram view:** PERT boxes laid out automatically, with critical highlighting.
- **Printing and PDF export** of views, plus reports: a project overview dashboard, cost overview and a milestone report.
- **Saved filters and multi-level sort.** Also custom-field formulas and summary rollups.
- **Recurring tasks, task splitting and the remaining-work model:**
  - Reschedule incomplete work after the status date.
  - Implement "Update Project".
- **Leveling options:** level within slack only, a priority-driven order, splitting, and a leveling preview (MS Project's "Leveling Gantt").
- **Resource pool shared across projects:** one resources file that several projects link to.
- **Overtime work and cost-rate tables:** effective-dated rates and cost accrual.
- **Collaboration:** file watching, so the app reloads when another tool or agent saves the file. A merge-friendly JSON layout already exists.
- **MCP:**
  - `notifications/resources/updated` subscriptions
  - per-tool read-only mode
- **Theming:** a high-contrast mode, keyboard-only navigation of the menu bar and toolbar, and accessibility labels.
- **Packaging:** code signing and notarization.

## Later

- **Microsoft Project XML (MSPDI) import and export.** This comes near the end. It is approved, under Microsoft's published schema and Open Specification Promise, to implement once the work reaches it.

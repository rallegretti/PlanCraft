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
- **Tracking:** time entries, the weekly timesheet, progress from tracked time, Baseline and Baseline1–10, interim plans, variance and earned value
  - Status date, Update Project (progress through a date; reschedule uncompleted work after it) and Mark on Track.
  - Stop and Resume dates: a task in progress can pause and resume later (a split bar); each reschedule adds a pause and keeps the earlier ones. Resource usage and leveling leave the pauses free. Actual and remaining duration entry.
- **Desktop app:** two-row command bar, twelve views, the major dialogs, undo/redo with history, multiple open documents, CSV export
- **Network Diagram:** a box per task, laid out automatically (each path along its own row, optionally critical path first), summaries beside their subtasks, collapsed summaries as one box, critical boxes and links highlighted, in-progress and complete marks, link labels, ID-only boxes, drag-to-link.
- **Custom fields and tables:**
  - Custom task fields: text with an optional pick-list, number, cost, date, duration and flag.
  - Table columns you can insert, hide and move, saved with the plan.
  - Sort within the outline, and group by any field.
- **Printing and PDF export:**
  - The Print window: a live preview of the exact printed pages, Page Setup saved with the plan, printing through the system print service (CUPS) or the PDF viewer on Windows.
  - PDF export written by PlanCraft itself (pure Rust, embedded font subset, searchable text).
  - Gantt Chart, Tracking Gantt, Multiple Baselines Gantt, task table and Resource Sheet; pages across and down, repeated columns, legend, header and footer codes.
- **MCP server:** 78 tools, resources and prompts over stdio and Streamable HTTP. A live mode works inside the running app (bearer token per session, `plancraft mcp --attach` bridge), and the app binary has a headless `plancraft mcp` mode.
- **Robustness:** no panics in shipped code (clippy-enforced), guarded edits with rollback, limits on hostile files and arguments, atomic saves, recovery copies, owner-only files.
- **Quality gates:** `cargo xtask ci`, run locally on Linux, macOS and Windows (fmt, clippy, tests, cargo deny), a scheduler speed gate, hostile-input sweeps and parity tests against Microsoft's published scheduling examples.

## Next

- **Interchange:** import CSV and Excel (pure Rust).
- **Reports:** a project overview dashboard, cost overview and a milestone report (printable). Printing of the usage, calendar and Network Diagram views. Network Diagram box styles and manual box positioning.
- **Saved filters and multi-level sort.** Also custom-field formulas and summary rollups.
- **Recurring tasks and task splitting:** splitting tasks that have not started, dragging split parts on the Gantt, and the status-date calculation options (move completed parts back to the status date, remaining parts forward).
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

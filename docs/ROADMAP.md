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
- **MCP server:** 67 tools, resources and prompts over stdio and Streamable HTTP. A live mode works inside the running app, and the app binary has a headless `plancraft mcp` mode.

## Next

- **Interchange:** import and export Microsoft Project XML (MSPDI), so `.mpp` users can move plans in via Project's "Save as XML". Import CSV and Excel (pure Rust).
- **Network Diagram view:** PERT boxes laid out automatically, with critical highlighting.
- **Printing and PDF export** of views, plus reports: a project overview dashboard, cost overview and a milestone report.
- **Custom fields, text/flag/number columns, and a column picker.** Saved filters, groups and sort order.
- **Recurring tasks, task splitting and the remaining-work model:**
  - Reschedule incomplete work after the status date.
  - Implement "Update Project".
- **Leveling options:** level within slack only, a priority-driven order, splitting, and a leveling preview (MS Project's "Leveling Gantt").
- **Resource pool shared across projects:** one resources file that several projects link to.
- **Overtime work and cost-rate tables:** effective-dated rates and cost accrual.
- **Collaboration:** file watching, so the app reloads when another tool or agent saves the file. A merge-friendly JSON layout already exists.
- **MCP:**
  - `notifications/resources/updated` subscriptions
  - an optional bearer token for the HTTP server
  - per-tool read-only mode
- **Theming:** a dark theme and high-contrast mode, keyboard-only navigation of the menu bar and toolbar, and accessibility labels.
- **Packaging:** release builds for Linux, Windows and macOS, a `.plancraft` file association, and app icons in all sizes.

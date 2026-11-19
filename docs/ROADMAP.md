# Roadmap

## Done (v0.1)

- **Core model:** tasks, summaries, milestones with deliverables and member tasks, resources (work, material and cost; people, teams, partners, vendors, …) and assignments
- **Scheduling engine:**
  - calendar-aware CPM with FS, SS, FF and SF links and working, elapsed or percent lags
  - constraints, deadlines and manual tasks, including manually scheduled summary tasks (top-down planning)
  - working or elapsed task durations (`33ed` runs around the clock)
  - ALAP, inactive tasks, actuals, slack and the critical path
- **Calendars:** project, task and resource calendars, exceptions (including yearly ones), resource time off and availability periods
- **Cross-file dependencies:** links to tasks in other documents, with cached snapshots and refresh
- **Workload:** estimates (complexity points, three-point PERT and work), time-phased workload, overallocation detection and resource leveling
- **Tracking:** time entries, the weekly timesheet, progress from tracked time, Baseline and Baseline1–10, interim plans, variance and earned value
  - Status date, Update Project (progress through a date; reschedule uncompleted work after it) and Mark on Track.
  - Stop and Resume dates: a task in progress can pause and resume later (a split bar); each reschedule adds a pause and keeps the earlier ones. Resource usage and leveling leave the pauses free. Actual and remaining duration entry.
- **Split tasks:** Split Task on the Gantt Chart (click a bar), as many parts as needed; drag a later part to move it or back to rejoin, drag a part's end to change its work. Splits move with the task.
- **Task Board:** tasks as cards in status columns (Not Started, Next up, In progress, Done by default); drag cards between columns, rename, add, move and delete columns, Set % Complete per column, Show on Board, the Board Status column in tables.
- **Recurring tasks:** daily, weekly, monthly and yearly patterns, ending after a number of occurrences or by a date; an occurrence on a non-working day moves to the next working day; own calendar; resources assigned to the recurring task work every occurrence; leveling never delays occurrences.
- **Desktop app:** two-row command bar, thirteen views, the major dialogs, undo/redo with history, multiple open documents, CSV export (tasks, resources, assignments, logged time); in the task table, rows dragged by their number, Fill Down (Ctrl+D), Find and Replace (Ctrl+F, Ctrl+H), Go To (F5) and Multiple Task Information
- **Network Diagram:** a box per task, laid out automatically (each path along its own row, optionally critical path first), summaries beside their subtasks, collapsed summaries as one box, critical boxes and links highlighted, in-progress and complete marks, link labels, ID-only boxes, drag-to-link.
- **Custom fields and tables:**
  - Custom task fields: text with an optional pick-list, number, cost, date, duration and flag.
  - Table columns you can insert, hide and move, saved with the plan.
  - Sort within the outline, and group by any field.
- **Printing and PDF export:**
  - The Print window: a live preview of the exact printed pages, Page Setup saved with the plan, printing through the system print service (CUPS) or the PDF viewer on Windows.
  - PDF export written by PlanCraft itself (pure Rust, embedded font subsets, searchable text).
  - Gantt Chart, Tracking Gantt, Multiple Baselines Gantt, task table and Resource Sheet; pages across and down, repeated columns, legend, header and footer codes.
  - The Network Diagram and the Task Board, fitted to a number of pages across or at actual size.
- **Other scripts:** text the bundled font lacks (Chinese, Japanese, Korean, Hebrew, Thai and more) shows and prints in fallback fonts found on the system; right-to-left text reads right to left on screen and on paper, and Arabic letters join on screen. Not yet: Arabic letter joining and Indic shaping in PDFs, and right-to-left word order while editing.
- **Interchange:**
  - Microsoft Project XML (MSPDI) import and export, following Microsoft's published schema (under its Open Specification Promise), with notes on anything the other side has no place for.
  - Task lists from spreadsheets: CSV and tab-separated text and Excel (`.xlsx`, `.xlsm`, `.xlsb`), all pure Rust, read cell by cell within limits. The Import Spreadsheet window guesses what each column holds from its heading and lets you change it. The task list's CSV export reads back in.
- **Packages:** a universal macOS disk image (Developer ID signed and notarized), a portable Windows zip, and a Linux AppImage and installable tarball for x86_64 and arm64, each tested on a clean system before release.
- **MCP server:** 92 tools, resources and prompts over stdio and Streamable HTTP. A live mode works inside the running app (bearer token per session, `plancraft mcp --attach` bridge), and the app binary has a headless `plancraft mcp` mode.
- **Changes on disk:** open plans are watched (plain polling, pure Rust, works on network shares). When another PlanCraft, the headless MCP server, an agent, git or a sync tool writes the file, a plan without unsaved changes reloads by itself (one undo step, with a notice); a plan with unsaved changes shows a bar to Reload, Keep Mine (the next save asks before overwriting), Compare or Save As. A deleted or moved file keeps the plan open as unsaved work, and links to other plans refresh when those files change. MCP tools report a changed or missing file (`disk_notice`), and `reload_project` reloads it without ever discarding the user's unsaved changes.
- **Open in another PlanCraft:** an open plan holds a lock file next to it (`.NAME.lock`: user, computer, process, since when), refreshed every 30 seconds. Opening a plan that another PlanCraft (another window, the headless MCP server, another computer on a shared folder) has open shows who has it, in a bar above the plan and in MCP results (`open_elsewhere`); the lock is taken once that PlanCraft closes it. Stale locks (a crash, a computer asleep) are taken over after 2½ minutes, at once on Linux when the process is gone.
- **Robustness:** no panics in shipped code (clippy-enforced), guarded edits with rollback, limits on hostile files and arguments, atomic saves, recovery copies, owner-only files.
- **Quality gates:** `cargo xtask ci`, run locally on Linux, macOS and Windows (fmt, clippy, tests, cargo deny), a scheduler speed gate, hostile-input sweeps and parity tests against Microsoft's published scheduling examples.

## Next

- **Reports:** a project overview dashboard, cost overview and a milestone report (printable). Printing of the usage and calendar views. Network Diagram box styles and manual box positioning.
- **Saved filters and multi-level sort.** Also custom-field formulas and summary rollups.
- **Status-date calculation options:** move completed parts back to the status date and remaining parts forward.
- **Leveling options:** level within slack only, a priority-driven order, splitting, and a leveling preview (MS Project's "Leveling Gantt").
- **Resource pool shared across projects:** one resources file that several projects link to.
- **Overtime work and cost-rate tables:** effective-dated rates and cost accrual.
- **Shared editing** (opt-in; [design](COLLABORATION.md)): a host serves a folder of plans to 5–10 people over the office network, ordering and checking every change, with logins, a crash-safe journal, conflict reports and work kept through network drops. Phase 1, the sync engine (`plancraft_core::sync`), phase 2, the server (`plancraft-server`, also `plancraft serve`), and phase 3, PlanCraft as a client (*File › Open from Server…*), are done. Next: discovery and *Share Folder…*, then packaging and optional services.
- **MCP:**
  - `notifications/resources/updated` subscriptions
  - per-tool read-only mode
- **Theming:** a high-contrast mode, keyboard-only navigation of the menu bar and toolbar, and accessibility labels.
- **Packaging:** a Windows installer, and code signing for Windows.

## Later

- **More interchange:** legacy Excel `.xls` and OpenDocument `.ods` (they need a reader that streams cells, so a hostile file can't exhaust memory), importing resources (not only tasks) from spreadsheets, and choosing which columns the CSV export writes.

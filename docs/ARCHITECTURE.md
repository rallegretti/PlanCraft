# PlanCraft architecture

PlanCraft is a Cargo workspace with four crates:

```
plancraft-app  ──►  plancraft-mcp     ──►  plancraft-core
     │         ──►  plancraft-server  ──►       ▲
     └──────────────────────────────────────────┘
```

## plancraft-core

This crate is pure logic with no UI. Every other component builds on it.

| Module | Responsibility |
|---|---|
| `model` | `Project`, `Task`, `Resource`, `Assignment`, `TimeEntry`, `ExternalProject`, `TaskRef`, `Dependency`, `Constraint`, `Estimate`, `Baseline`, and computed `TaskSchedule` / `ProjectSchedule` |
| `ids` | Stable unique ids (`TaskId`, `ResourceId`, …) and their monotonic counters |
| `duration` | Working-minute durations: formatting and parsing (`3d`, `4h`, `2w`, `3d?`) and lags (`2d`, `-1d`, `50%`, `3ed`) |
| `custom` | Custom field definitions and values, and saved table layouts (column keys per table) |
| `print` | Page Setup, page layout of a table with a Gantt chart (pages across and down, timescale, bars, links, legend, header/footer), the Network Diagram and Task Board as drawings (`print::network`, `print::board`) tiled over pages by `print::canvas` (fit to N pages wide or actual size), PDF writer that embeds subsets of every font a page uses (`print::pdf`, `print::subset`, with the `subsetter` crate for TrueType and CFF). Text prints in the bundled UI font, or in a system fallback font for scripts it lacks (`print::fallback` finds them from their character maps, `print::font::pick` chooses per character). The app's Print Preview paints the same pages |
| `bidi` | Right-to-left text (Hebrew, Arabic) put in display order with the Unicode Bidirectional Algorithm, one paragraph per line. `visual` gives full display order for the PDF writer, which draws characters as given. `display` is for egui, which lays out each right-to-left run itself: runs and words go in display order, letters within a run stay in typing order |
| `network` | Network Diagram layout: columns by dependency depth (loops, found as strongly connected components, are released once everything leading into them is placed), rows by path (optionally critical first), summaries beside their subtasks, collapsed summaries as one box, right-angled link routes and hit testing |
| `timescale` | Timescale units and tiers shared by the chart, usage views, timeline and printing |
| `calendar` | Working weeks, exceptions (including yearly ones), and `WorkCal` date arithmetic: add/sub working time, snapping, and working minutes between two times. Each `WorkCal` caches the working time of whole months and years (with their leading and trailing days off), so `add`, `sub` and `between` step over whole months and years instead of walking every day; tests check the results equal a day-by-day walk. A recalculation makes one `WorkCal` per distinct calendar (resources on the same calendar without time off share one), and `schedule::TaskCalendars` does the same for drawing and printing |
| `schedule` | The CPM engine (`recalculate`) |
| `ops` | All editing operations: outline, links, milestones, the duration/work/units rules, assignments, progress, time logging, calendars, and text parsing for the table columns |
| `baseline` | Baseline and Baseline1–10, interim plans (Start1–10/Finish1–10): saving for the entire project or selected tasks with roll-up into summaries, copying between them, clearing, and per-plan totals |
| `split` | Split Task: pauses planned in a task (`Gap`: after some work, a pause of some working time) that move with it; the scheduler lays the parts out (`schedule::segments_of`) |
| `board` | The Task Board: its columns (Board Status values, each with an optional Set % Complete), which column a task's card is in, and moving cards and columns |
| `recurring` | Recurring tasks: the patterns, occurrence dates, and the summary-with-occurrences each one is |
| `workload` | Time-phased distribution (with contours), capacity, overallocation, and workload reports |
| `leveling` | Serial resource leveling that writes `leveling_delay`. It keeps loads incrementally and recalculates only when a delay can move other tasks. |
| `report` | Statistics, milestone status, lateness, earned value |
| `io` | `.plancraft` JSON load/save, relative paths, external-link refresh |
| `tabular` | Task lists from spreadsheets, and the CSV exports (tasks, resources, assignments, logged time). `csv` reads CSV/TSV (UTF-8, UTF-16, Windows-1252; comma, semicolon or tab), `workbook` streams `.xlsx`/`.xlsm`/`.xlsb` cells with `calamine`'s cell readers after checking the archive's declared unpacked size (never its whole-sheet `Range`, which allocates a grid spanning the outermost cells); both give a grid of text cells within a per-file budget of cells and text. `guess_columns` matches headings to fields, and `build` makes the plan: outline from Outline Level or WBS, links by the ID column (longest matching key), work and material resources from names, and the file's dates as Start/Finish No Earlier Than constraints only where the schedule alone would put a task earlier, added in rounds so a constraint early in a chain doesn't pin the tasks after it. Fields of the new tasks are set by row (no lookups by id), so 50,000 rows import in well under a second |
| `mspdi` | Microsoft Project XML (MSPDI) import and export, from Microsoft's published schema reference. `mspdi::xml` streams the file one record (task, resource, assignment, calendar) at a time on `quick-xml` with depth, size and entity limits; `read` maps records into a `Project` (recurring calendar exceptions expanded into dates, resource calendars into personal exceptions) and `write` writes elements in the schema's order. Both return notes on what did not carry over |
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
   - Applies constraints: SNET/FNET push the task later; MSO/MFO pin it; SNLT/FNLT are honored and flagged when violated. A Start No Earlier Than date on a summary drives all of its subtasks.
   - Adds leveling delays.
   - Uses actual start and finish when they are set.
   - A task with an elapsed duration (`33ed`) runs around the clock: it starts as soon as it may, even outside working time, and finishes the elapsed time later; its work is spread over every hour of that time.
   - Manual tasks keep their entered dates. A manually scheduled summary keeps its own dates too: links into it (and into the summaries around it) stop at it, its auto-scheduled subtasks start no earlier than its start, and its successors follow its own dates.
6. **Backward pass.** It computes late dates, limited by the project finish, deadlines (a task's own and its summaries') and constraints. Inactive successors are skipped. A link from a summary's start binds only the subtask that sets that start.
7. **ALAP pass.** As-late-as-possible tasks are pinned to their late start, and the passes run again so successors stay consistent.
8. **Slack.** Total and free slack are computed per leaf. A task is critical when total slack ≤ the threshold.
9. **Roll up summaries and the project:**
   - Dates, work and cost. A summary's own fixed cost and assignments are added on top of its subtasks'. A manually scheduled summary keeps its own dates; its subtasks' span goes to `rollup_start`/`rollup_finish`.
   - % complete, weighted by the durations of all subtasks below. Nested summaries and the project agree.
   - A missed summary deadline is flagged.

### Duration, work and units

Assignments follow Microsoft Project's task types (Fixed Units, Fixed Duration, Fixed Work) and the effort-driven rule (`ops::assign`, `unassign`, `set_duration`, `set_assignment_*`). Fixed Work tasks are always effort-driven. Adding or removing a person on an effort-driven task keeps total work and shares it by units; a Fixed Duration task keeps its duration and changes units instead. `tests/ms_parity.rs` checks this against the worked examples in Microsoft's published documentation, with each test citing its page.

A task with an **estimate** carries `sched.work` before any assignment. The first work assignment of an estimated task derives its work from the estimate, and its duration from that work divided by the units.

### Cross-document links

`ExternalProject` stores:

- the other document's project id
- its path (relative to this document when possible)
- **snapshots** of referenced tasks (dates, % complete)

Scheduling only ever reads the snapshots, so a file opens and schedules even when the linked file is missing. `io::refresh_external` reloads the linked files and updates the snapshots. The workspace calls it when a document opens; you can also trigger it with *Refresh Links*. `Document::save_as` rewrites relative paths so links survive moving a file.

### Changes on disk (`watch`)

Another PlanCraft, the headless MCP server, an agent, git or a sync tool may write an open plan's file. `io::DiskStamp` records the file's modification time and size (not the inode, which FUSE and some network mounts change on their own) when a document is loaded or saved; `Document::disk_status` compares the file with it (`InSync`, `Changed`, `Missing`, `NoFile`), and `save` refuses to overwrite a changed file. `watch::Watcher::poll` checks every open document's file and the files it links to, with plain `std::fs::metadata` polling (no platform notification APIs, so it behaves the same on every system and on network shares). It reports a change only after it held for two polls, reads the new file (`workspace::read_for_reload`, which reads again while the file is still changing) and reports each version once. PlanCraft's own saves record the new stamp, so they are never reported. `Document::reload_from` replaces the project as one undoable edit and marks the document as matching the file; `Document::refresh_external_from` updates link snapshots the way opening does (no undo step). `watch::compare` summarises how two versions differ (tasks and resources added, removed or changed).

### Lock files (`lock`)

An open plan holds an advisory lock file next to it, `.NAME.plancraft.lock` for `NAME.plancraft` (hidden on Windows too), with the user, computer, process id, a per-process instance id, when it was opened and the program. `Workspace::add`/`open` claim it (`Document::lock`: `Held`, `OpenElsewhere(owner)` or `None` when the folder can't hold one), `save`/`save_as` claim it for a new file (Save As gives the old one back), and `close` and dropping the workspace release it. `lock::spawn_keeper` (app and MCP server) runs `lock::maintain` every 30 seconds: it touches the locks held, takes over a stale lock (not touched for an hour by the clock, or for 150 seconds by the clock and 45 seconds while this PlanCraft watched it, since another computer's clock may run ahead; or from a process of this computer that is gone, which only Linux can tell) by moving it aside first, so two PlanCraft taking it over at once never both hold it (a lock written or touched since it was judged stale is put back: a hard link, or a new file with the same contents where the folder has no hard links), and takes the lock of a plan once the other PlanCraft has closed it, all without holding the workspace lock. Lock files are hostile input: at most 4 KiB is read, only regular files are read or written (`create_new`, never through a link), and the names are cleaned before they are shown.

### Shared editing (`sync`)

Plain logic, no networking; nothing uses it unless someone shares a plan (see [COLLABORATION.md](COLLABORATION.md)). `sync::diff` works out a `Patch` between two plans through their JSON form: top-level values, and each list's items matched by key, field by field (added, changed, removed, new order), without schedules or id counters. `sync::apply` applies one (refusing fixed values, taken keys and missing items); the caller checks and recalculates. `sync::rebase` moves a patch over a change made since: different fields combine, new items keep their places, and the same value changed twice, a change to a deleted item, or a list reordered by both are `Conflict`s. `Host` orders changes (version, a log of at most 1,000 entries and 64 MB, duplicate detection, the same checks as `Document::edit` plus any of the caller's own through `submit_checked`, id leases) and `Replica` is one person's copy (confirmed plan plus pending work, at most one change on its way, resync after missed changes, piece-by-piece salvage when merged work no longer fits). `sync::reverse` takes back one change on a plan that has others' changes since (undo in a shared plan).

A `Document` opened from a server (`Document::shared`, `Workspace::add_shared`) has no file and no lock file. Its edits still go through `try_edit`, which also remembers the plan before the first edit not handed on yet (`take_unsent`); `apply_remote` puts in the plan as the server's copy has it, without an undo step. Its undo steps keep the plan after each step too, so undo and redo take back only that step (`sync::reverse`) and are refused when someone changed the same values since (`try_undo_as`). Viewers' and signed-out plans refuse every change (`Shared::read_only`); `save`/`save_as` refuse (the server saves) and `save_copy` writes a standalone copy.

### Undo and attribution

`Document::edit` and `Document::try_edit` snapshot the project, apply the change, validate it against `limits`, recalculate and bump `revision`. Both return a `Result` and roll back on error. Each snapshot records whether the user or an agent made the change. Whole-project snapshots are simple and reliable. The history keeps up to 200 steps within an estimated 256 MiB per document, dropping the oldest first.

### Robustness

- **No panics in shipped code.** Workspace clippy lints deny `unwrap`, `expect`, `panic!` and indexing; code returns `Result`/`Option` and uses `get()` and checked or saturating arithmetic. Date arithmetic saturates (`calendar::shift`) and never walks past a horizon (1800–2500), so hostile durations can't hang the scheduler.
- **Guards as a last resort.** `guard::catch` turns a panic into an error. Edits roll back, `recalculate` leaves a warning, the MCP server answers with a tool error, and the app keeps running and writes recovery copies.
- **Hostile input.** `limits` caps counts (50,000 tasks, …), durations (5 years), dates (1900–2300), numbers and text. Loading clamps values and reports what it changed (`io::load_with_notes`); oversized files, too many items or duplicate ids are refused.
- **Atomic, private files.** `io::write_atomic` writes a temporary file in the same directory, fsyncs it, renames it over the target and fsyncs the directory. Per-user files are owner-only (0600/0700 on Unix).

## plancraft-mcp

The MCP server is a hand-rolled JSON-RPC 2.0 server. It has no async runtime, which keeps it small, predictable and pure Rust.

- **`server`:** initialize and version negotiation, `tools/*`, `resources/*`, `prompts/*`.
  - Tool errors come back as `isError` results.
  - Each request locks the shared workspace briefly.
  - The change callback runs after the lock is released.
- **`tools/*`:** 92 tools grouped by domain.
  - All mutations go through `Document::try_edit(…, ChangeSource::Agent, …)`.
  - Unknown arguments are rejected so agents find their typos.
- **Transports:**
  - `run_stdio`: newline-delimited JSON.
  - `serve_http`: Streamable HTTP via `tiny_http`, on a background thread. It rejects non-local `Origin` headers, and checks `Host` on loopback addresses.
- **`cli`:** shared by the `plancraft-mcp` binary and `plancraft mcp`.

## plancraft-server

PlanCraft Server shares a folder of plans for several people to edit at once (opt-in; see [COLLABORATION.md](COLLABORATION.md)). Plain threads, no async runtime: one reader and one writer per connection, one for upkeep.

- **`wire`:** the encrypted connection: Noise (`Noise_NX_25519_ChaChaPoly_BLAKE2s`, `snow` with pure-Rust crypto), the server's key pair and fingerprint, frames split over Noise messages and capped before they are read.
- **`protocol`:** the JSON messages (`ClientMsg`, `ServerMsg`), roles and frame limits.
- **`users`:** the owner-only users file with Argon2id hashes, roles per folder and per plan, slowing down failed sign-ins (`Throttle`), a cap on password checks at once (`Gate`), and `Users::change`, which makes each change to the file while holding a lock file beside it (the app and the command line take turns; passwords are hashed before, `NewPassword`).
- **`journal`, `plan`:** one shared plan: its `sync::Host`, its journal (changes and id leases on disk before anyone hears of them), saving, recovery after a crash, its lock file, and the people who have it open.
- **`server`:** accepting connections, signing in, opening plans (one per connection) and handing requests to them in order; **`client`:** the other end.
- **`link`:** a plan opened from a server, kept in step on two background threads (a reader, and a session that owns the `Replica`): edits in (the plan before and after, numbered), merging off the app's thread, pings, reconnecting with backoff and catching up, topping up id leases, and stopping when the server will not have this person back (`State::SignedOut`). Events out: the plan to show (with the number of the last edit it includes), status, people, dropped work, notices.
- **`cli`:** shared by the `plancraft-server` binary and `plancraft serve`. `--watch-stdin` prints a `Ready` line (address and fingerprint) on standard output and stops the server cleanly when standard input closes.
- **`discovery`:** mDNS (`mdns-sd`, pure Rust): a started server announces `_plancraft._tcp` (`Announcer`, withdrawn first thing when stopping), and the *Open from Server* window lists what it hears (`Browser`); announcements are untrusted input, cut short and checked before they are shown, counted so no one computer fills the list (`FoundList`: per host name and per address, trusted servers first), and never vouch for a key. Also this computer's addresses for *Share Folder* (container and VM bridges left out).
- **`sharing`:** a server run as a separate program for *Share Folder* (`plancraft serve FOLDER --watch-stdin`): its readiness, its log, stopping it, and reaping it when dropped.

## plancraft-app

The desktop app uses egui and eframe 0.36 with the wgpu renderer.

- **`app`:** layout (title band, command bar, document tabs, status bar, vertical view label), keyboard shortcuts, command execution, dialogs, the unsaved-changes prompt, and agent-change detection.
- **`menubar`:** the title band and the two-row command bar (a view selector spanning both rows, plain tabs with a File dropdown, and a single toolbar row).
- **`start_page`:** the Start Page, a centered launcher with start actions, recent projects (with file dates) and a summary of the open project.
- **`commands`:** the `Command` enum shared by the menu bar, toolbar, context menus and shortcuts.
- **`widgets::grid`:** a virtualized spreadsheet grid with in-place text and choice editors, column resizing, keyboard navigation, and blank rows for entering new items.
- **`widgets::timescale`:** the two-tier timescale (units from `plancraft_core::timescale`).
- **`fonts`:** adds the system fallback fonts to egui the first time a plan's text, typed text or a recent file name needs them.
- **`printing` / `dialogs::print`:** the printed sheet built from the current view (columns left of the divider, sort, group and filter as on screen), or the Network Diagram or Task Board as on screen (filter, search, collapsed summaries, view options), the Print window with its preview, and hand-off to the system print service (`lp`; the PDF viewer on Windows) in a background thread with a timeout.
- **`views/*`:** the thirteen views. The Gantt view (also the Tracking Gantt and the Multiple Baselines Gantt) combines the task grid with `gantt_chart`, which draws bars, links, milestone connectors and baselines and handles drag-to-move, resize, progress and link. `network` draws the core's Network Diagram layout (cached per edit) and handles selection, Task Information, drag-to-link, panning and zoom. `board` paints the Task Board's columns and cards and handles selection, dragging cards between columns and the column menu.
- **`columns`:** task table definitions. Each column defines how to display, edit and commit its value. Tables: Entry, Schedule, Tracking, Work, Cost, Estimates, Variance, Summary.
- **`dialogs/*`:** Task, Resource and Project Information, Change Working Time, Assign Resources, External Links, Statistics, History, About, and the MCP panel.
- **`disk_watch`:** a background thread polls the open plans' files every second (`plancraft_core::watch`), without the workspace lock, and queues what it finds. `app` then reloads a plan without unsaved changes (undoably, with a toast), shows a bar above a plan with unsaved changes (Reload, Keep Mine, Compare, Save As), keeps a plan whose file was deleted or moved as unsaved work, and refreshes link snapshots when a linked plan changes. A plan another PlanCraft has open (`Document::lock`) shows a bar saying who has it.
- **`mcp_host`:** starts and stops the HTTP MCP server on the app's `SharedWorkspace`, and repaints the window when agents make changes.
- **`remote` / `dialogs::server`:** plans opened from a PlanCraft Server. *Open from Server…* connects on a background thread (the server's key fingerprint is confirmed on the first visit and pinned in the settings; the password is never saved), and each plan keeps a `plancraft_server::link::Link` by document handle. `Remotes::sync` runs at the start and end of each frame: the links' plans go into their documents (only once they include every edit handed on), the documents' new edits go to their links, and the status, people and dropped work show in the status bar, the task table's indicators and a bar above the plan.
- **`dialogs::share`:** *File › Share Folder…*: starts a server for a folder as a separate program (`plancraft_server::sharing`; from an AppImage, the AppImage file), shows its addresses and key fingerprint, warns about the folder's plans open here (the server refuses them until they are closed), opens its plans here with the key already trusted, and manages the users file (people, roles, per-plan access, passwords hashed on a background thread). `ShareHost` keeps the server across the window opening and closing; quitting PlanCraft drops it, and the server saves its plans and stops.
- **`theme`:** light and dark palettes; all drawing reads colors from `theme::pal()`, and the Theme setting (System/Light/Dark) switches between them.
- **`logo`:** the mark as geometry, painted with egui and rasterized for the window icon.
- **`open_requests` and `mac_open` (macOS):** macOS hands a plan opened from Finder or the Dock to the app as an "open documents" event rather than on the command line, at launch and while it runs. `mac_open` installs the handler when AppKit is about to finish launching (winit owns the application delegate, so it watches that notification) and queues the files in `open_requests`, which the next frame opens like File ▸ Open. `Info.plist` declares the `.plancraft` type (`dev.plancraft.project`).
- **`bundled_libs` (Linux):** the packages carry a few window-system libraries a fresh desktop may lack (`lib/plancraft/<name>/`). At startup the app checks which of them the system lacks and, only if some are missing, restarts itself once with just those folders on `LD_LIBRARY_PATH`; the system's own copies are always preferred.

The UI locks the workspace once per frame. Views either edit the active `Document` directly or emit `Command`s that the app executes after layout.

<p align="center">
  <img src="assets/logo.svg" width="96" alt="PlanCraft logo">
</p>

<h1 align="center">PlanCraft</h1>

<p align="center">
  <b>Plan the work, then work the plan.</b><br>
  A desktop project planner for tasks, milestones, people and schedules,<br>
  with an AI assistant that can plan right alongside you.
</p>

<p align="center">
  <img alt="100% Rust" src="https://img.shields.io/badge/100%25-Rust-b7410e?style=flat-square&logo=rust">
  <img alt="Windows · macOS · Linux" src="https://img.shields.io/badge/Windows%20%C2%B7%20macOS%20%C2%B7%20Linux-native-1B3A8C?style=flat-square">
  <img alt="License: CC0 1.0" src="https://img.shields.io/badge/license-CC0%201.0-3a3a3a?style=flat-square">
  <img alt="Status: early preview" src="https://img.shields.io/badge/status-early%20preview-F2B11C?style=flat-square">
</p>

<p align="center">
  <a href="#what-you-can-do-with-plancraft">Features</a> ·
  <a href="#screenshots">Screenshots</a> ·
  <a href="#installing">Install</a> ·
  <a href="#getting-started">Get started</a> ·
  <a href="#planning-with-an-ai-assistant">AI assistants</a> ·
  <a href="#your-plans-are-safe">Your plans are safe</a>
</p>

<br>

![The Gantt chart with the critical path highlighted](docs/images/gantt.png)

<table>
  <tr>
    <td width="25%" valign="top">
      <h3>📋 Familiar by design</h3>
      Task list, Gantt chart, resource sheet and calendars where you expect them. If you've planned a project before, you already know your way around.
    </td>
    <td width="25%" valign="top">
      <h3>⚡ Native and fast</h3>
      A native app written in Rust. A 200-task plan reschedules in well under a millisecond, so dates update as you type. No browser, no Electron.
    </td>
    <td width="25%" valign="top">
      <h3>🔒 Offline and private</h3>
      Your plans are files on your own disk. No account, no telemetry, and nothing leaves your computer.
    </td>
    <td width="25%" valign="top">
      <h3>🤖 AI-ready</h3>
      A built-in MCP server lets an assistant such as Claude Code plan with you. You see every change live and can undo any of it.
    </td>
  </tr>
</table>

<br>

## What you can do with PlanCraft

**Plan your project**
- Break work down into tasks and subtasks, typing straight into the task list like a spreadsheet.
- See everything on a **Gantt chart**: drag bars to reschedule, stretch them to change a duration, or drag one task onto another to link them.
- Link tasks so one starts when another finishes, or overlap them with leads and lags. Dates update automatically when anything changes.
- Mark the deadlines that matter and see the **critical path**, the chain of tasks that sets your finish date.
- See how the work flows in the **Network Diagram**: a box per task, laid out automatically, with the critical path in red. Drag from one box to another to link them.

**Hit your milestones**
- A milestone is a checkpoint with a **deliverable**, for example "Signed project charter" or "Beta ready", and the tasks that must be done to reach it.
- The Milestones view shows each one's date, deliverable, progress, and whether it's on track, slipping or late.

**Manage people and partners**
- Add team members, whole teams, partner companies, vendors, contractors and equipment, with their hourly rates.
- Record vacations, holidays and part-time schedules. Work is automatically scheduled around them.
- Spot overbooked people instantly in **Resource Usage** and **Workload**, then let **Level All** spread the work out for you.
- Move work between people by dragging it in the **Team Planner**.

**Estimate and track**
- Estimate tasks in hours, with optimistic/likely/pessimistic ranges, or with simple complexity points before anyone is assigned.
- Log hours on a weekly **Timesheet** and turn tracked time into progress with one click.
- **Split** a task to pause its work and resume it later, and drag the parts around on the Gantt Chart.
- Add **recurring tasks** such as a weekly stand-up or a monthly report: daily, weekly, monthly or yearly.
- Work the plan on a **Task Board**: drag cards from Not Started to Done, add your own columns, and let a column set the task's % complete.
- Set a **status date** and run **Update Project**: bring progress up to date as scheduled, or move work that didn't get done past the status date. Tasks in progress pause and resume after the date, so the work already done stays where it happened. **Mark on Track** brings the selected tasks' progress up to the status date.
- Save a **baseline** and compare it with the current plan: what slipped, what costs more, and how the project is trending. Keep up to eleven baselines (for example one per phase) and compare them side by side in the **Multiple Baselines Gantt**, or save just the dates as interim plans.

**Work across projects**
- Make a task in one project depend on a task in another project file. When the other project slips, yours picks up the new dates the next time you open it or refresh links.
- Open several projects at once in tabs.

**Plan with AI**
- Connect an AI assistant such as **Claude Code**. It can read and change everything in your plan: build a whole task breakdown from a description, link tasks, assign people, log time, level workloads and report on status.
- You watch the changes appear live, and you can undo anything the AI did.

**Make it yours**
- Light and **dark mode**, following your system setting or chosen under **View ▸ Theme**.
- Thirteen views (Gantt Chart, Tracking Gantt, Multiple Baselines Gantt, Network Diagram, Task Board, Task Usage, Calendar, Milestones, Resource Sheet, Resource Usage, Workload, Team Planner and Timesheet), with filters, search and different column layouts.
- **Print** any Gantt chart, task table or resource sheet with a live print preview, or save it as a **PDF**. Choose paper, orientation, scale, how many pages wide the timescale spreads, the legend, and headers and footers.
- Undo and redo (up to 200 steps), plus export to CSV for spreadsheets.
- **Work with Microsoft Project files.** Open a Microsoft Project XML file (`.xml`) to import it, and use **File ▸ Export Microsoft Project XML…** to hand your plan to Microsoft Project or other planning tools. Tasks, outline, links, calendars, resources, assignments, baselines and custom fields come across; PlanCraft tells you anything the other side has no place for.

## Screenshots

| | |
|---|---|
| ![Dark mode](docs/images/gantt-dark.png) **Dark mode**, here with the critical path shown | ![Milestones](docs/images/milestones.png) **Milestones**, each with its deliverable, progress and status |
| ![Resource Usage](docs/images/resource-usage.png) **Resource Usage** shows hours per person per day; overbooked days stand out | ![Workload](docs/images/workload.png) **Workload** compares each person's planned hours with what they have available |
| ![Team Planner](docs/images/team-planner.png) **Team Planner**: drag work from one person to another | ![Calendar](docs/images/calendar.png) **Calendar** gives a month-at-a-glance view |
| ![Task Information](docs/images/task-information.png) **Task Information** collects every detail of a task in one place | ![Start Page](docs/images/start-page.png) **Start Page** opens your recent projects or starts a new one |

## Installing

Download the package for your computer from the [Releases page](https://github.com/rallegretti/PlanCraft/releases).

### Windows

1. Download `PlanCraft-<version>-windows-x64.zip` and extract it anywhere you like.
2. Double-click **PlanCraft.exe**.

PlanCraft isn't signed with a Microsoft certificate yet, so Windows may show *"Windows protected your PC"* the first time. Click **More info**, then **Run anyway**.

### macOS

1. Download `PlanCraft-<version>-macos-universal.dmg` and open it.
2. Drag **PlanCraft** into your **Applications** folder.
3. Open PlanCraft from Applications.

PlanCraft is signed and notarized by Apple, so it opens like any other app. It runs natively on both Apple Silicon and Intel Macs.

### Linux

PlanCraft runs on Ubuntu 22.04 or newer, and other 64-bit distributions from 2022 on. There are two packages.

**AppImage** (one file, nothing to install):

1. Download `PlanCraft-<version>-x86_64.AppImage`.
2. Make it executable and run it:
   ```bash
   chmod +x PlanCraft-*-x86_64.AppImage
   ./PlanCraft-*-x86_64.AppImage
   ```

If your system lacks FUSE (it says it can't mount the AppImage), run it with `--appimage-extract-and-run`.

**Install package** (adds PlanCraft to your applications menu):

1. Download `plancraft-<version>-linux-x86_64.tar.gz`, then extract it:
   ```bash
   tar xzf plancraft-*-linux-x86_64.tar.gz
   ```
2. Install it for your user. This adds PlanCraft to your applications menu and makes `.plancraft` files open with it:
   ```bash
   ./plancraft-*-linux-x86_64/install.sh
   ```

You can also run `bin/plancraft` directly without installing. To remove PlanCraft later, run `./install.sh --uninstall`.

### Building it yourself

If you have [Rust](https://rustup.rs) installed, you can build and run PlanCraft from source:

```bash
git clone https://github.com/rallegretti/PlanCraft.git
```

```bash
cd PlanCraft
```

```bash
cargo run --release -p plancraft-app
```

## Getting started

1. **Try the sample.** On the Start Page, choose **Sample project** to explore a ready-made product launch with milestones, a team and some progress.
2. **Start your own.** Choose **Blank project**, then type task names into the task list. Press **Insert** to add a task above the selected one.
3. **Organize.** Select tasks and press **Alt+Shift+→** to indent them under a summary task.
4. **Link.** Select tasks in order and press **Ctrl+F2**, or drag one bar onto another in the chart.
5. **Assign people.** Type names into the *Resource Names* column; new names are added to your team automatically. You can also use **Resource ▸ Assign Resources**.
6. **Add milestones.** Use **Task ▸ Milestone**, then open it (double-click) to set its deliverable and the tasks that make it up.
7. **Track progress.** Use the % complete buttons or **On Track** on the Task toolbar, log hours in the **Timesheet** view, or use **Project ▸ Update Project** to update everything as of the status date.

### Handy shortcuts

| Shortcut | Does |
|---|---|
| `Insert` / `Delete` | Add / remove a task |
| `F2`, or just start typing | Edit the selected cell |
| `Alt+Shift+→` / `Alt+Shift+←` | Indent / outdent |
| `Alt+Shift+↑` / `Alt+Shift+↓` | Move a task up / down |
| `Ctrl+F2` / `Ctrl+Shift+F2` | Link / unlink the selected tasks |
| `Shift+F2` | Task Information |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Ctrl+S` | Save |
| `Ctrl` + mouse wheel | Zoom the timeline in and out |
| `Shift` + mouse wheel | Scroll the chart through time |

On a Mac, use `Cmd` instead of `Ctrl`.

## Planning with an AI assistant

PlanCraft has built-in support for the **Model Context Protocol (MCP)**, the open standard AI assistants use to work with apps. Once connected, you can ask things like:

> *"Plan a website redesign with design, build and launch phases, about eight weeks, for Ana, Ben and our design agency."*

> *"Who's overbooked next week, and what would you move?"*

> *"Log 6 hours for Ben on Backend services yesterday and tell me how the Beta milestone is looking."*

**To connect Claude Code to the PlanCraft window:**
1. In PlanCraft, open **Project ▸ AI Agents** and click **Start server**. You can tick *Start automatically* so it's always ready.
2. Copy the command the panel shows you and run it once. It looks like this, with the path to PlanCraft on your computer:
   ```bash
   claude mcp add plancraft -- /path/to/plancraft mcp --attach
   ```
   With the AppImage, the path is the `.AppImage` file itself.

Changes the AI makes appear in the window as they happen. They're marked with ⚡ in **View ▸ History**, and **Ctrl+Z** undoes them like your own edits.

The connection is private to you: each time the server starts it creates a new secret key, kept in a file only your user account can read, and PlanCraft refuses requests that don't carry it. Other apps that connect over HTTP can use the address and key shown in the panel.

AI assistants can also work on project files without the window open. The AI Agents panel shows the exact command to use, and [docs/MCP.md](docs/MCP.md) lists everything an assistant can do.

> Only connect assistants you trust. They can change your plans and save files.

## Your plans are safe

- **Saving never corrupts a file.** PlanCraft writes the new version alongside the old one and swaps them in a single step, so a crash or power cut mid-save leaves you with either the old plan or the new one, never a broken file.
- **Recovery copies.** While you work, PlanCraft keeps a copy of unsaved changes. If it ever closes unexpectedly, the Start Page offers to restore them.
- **Built not to crash.** If something goes wrong inside PlanCraft, the action that failed is undone and you get a message instead of losing your work.
- **Files from others are checked.** A damaged or unusual file is opened as far as it safely can be: out-of-range values are corrected and PlanCraft tells you what it changed.
- **Private by default.** New plan files, settings and recovery copies are readable only by your user account on macOS and Linux, because plans hold people's names and rates.

## Your files

Projects are saved as `.plancraft` files. They are plain text, so they work well with backup tools and version control, and you can open the same project on any computer running PlanCraft.

To exchange plans with Microsoft Project, use Microsoft Project XML (`.xml`):

- **Importing:** open the `.xml` file with **File ▸ Open**. It opens as a new, unsaved plan; save it to keep it as a `.plancraft` file. A message lists anything PlanCraft could not carry over, such as a recurring task's repeat pattern (its occurrences are kept as ordinary tasks).
- **Exporting:** **File ▸ Export Microsoft Project XML…** writes your plan as `.xml`. PlanCraft-only details, such as three-point estimates, Task Board columns and planned splits, have no place in that format and are listed when you export.

## License

PlanCraft is free and open source, dedicated to the public domain under [CC0 1.0 Universal](LICENSE).

---

<sub>Developers: see [docs/BUILDING.md](docs/BUILDING.md) for building release packages and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how PlanCraft is put together.</sub>

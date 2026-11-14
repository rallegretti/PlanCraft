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
  <a href="#download"><b>Download</b></a> ·
  <a href="#features">Features</a> ·
  <a href="#getting-started">Get started</a> ·
  <a href="#planning-with-an-ai-assistant">AI assistants</a> ·
  <a href="#your-plans-are-safe">Your plans are safe</a>
</p>

<p align="center">
  <a href="https://github.com/rallegretti/PlanCraft/releases/latest"><b>⬇ Download PlanCraft for Windows, macOS or Linux</b></a>
</p>

<br>

![The Gantt Chart with the critical path highlighted](docs/images/gantt.png)

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

## Download

PlanCraft is free. Get the latest version from the **[Releases page](https://github.com/rallegretti/PlanCraft/releases/latest)**: scroll down to **Assets** and pick the file for your computer.

| Your computer | File to download | What you get |
|---|---|---|
| **Windows** 10 or 11 (64-bit) | `PlanCraft-<version>-windows-x64.zip` | A folder with PlanCraft in it, ready to run |
| **Mac** with Apple Silicon or Intel, macOS 11 or newer | `PlanCraft-<version>-macos-universal.dmg` | The PlanCraft app, to drag into Applications |
| **Linux** (64-bit), the easy way | `PlanCraft-<version>-x86_64.AppImage` | One file that runs PlanCraft, nothing to install |
| **Linux** (64-bit), in your applications menu | `plancraft-<version>-linux-x86_64.tar.gz` | An installer that adds PlanCraft to your menu |

Each file has a matching `.sha256` file. It's only for checking that a download arrived intact; you don't need it to install.

### Windows

1. Download the `.zip` file.
2. Right-click it in your Downloads folder, choose **Extract All…**, and pick where to keep PlanCraft (your Documents folder is fine).
3. Open the extracted folder and double-click **PlanCraft.exe**.

**The first time you open PlanCraft**, Windows may show a blue box saying *"Windows protected your PC"*. This is Windows SmartScreen, which warns about apps it hasn't seen often. Click **More info**, then **Run anyway**. You only need to do this the first time you open each new version.

To start PlanCraft more easily later, right-click **PlanCraft.exe** and choose **Pin to Start** or **Pin to taskbar**.

### macOS

1. Download the `.dmg` file and double-click it.
2. In the window that opens, drag **PlanCraft** onto the **Applications** folder.
3. Open PlanCraft from Applications, Launchpad or Spotlight.

The same download runs natively on Apple Silicon and Intel Macs.

### Linux

PlanCraft runs on Ubuntu 22.04 or newer, and on other 64-bit distributions from 2022 on.

**AppImage:** one file you can keep anywhere and run.

1. Download the `.AppImage` file.
2. Allow it to run as a program: right-click it, open **Properties ▸ Permissions** and tick **Allow executing file as program**. Or, in a terminal:
   ```bash
   chmod +x PlanCraft-*-x86_64.AppImage
   ```
3. Double-click it, or run `./PlanCraft-*-x86_64.AppImage`.

If it reports that FUSE is missing or it can't mount the AppImage, start it with `--appimage-extract-and-run` added to the command.

**Install package:** adds PlanCraft to your applications menu, and makes `.plancraft` files open with it.

1. Download the `.tar.gz` file and extract it:
   ```bash
   tar xzf plancraft-*-linux-x86_64.tar.gz
   ```
2. Run the installer. It installs for your user only, so no password is needed:
   ```bash
   ./plancraft-*-linux-x86_64/install.sh
   ```

To remove it later, run `install.sh --uninstall` from the same folder.

### Updating to a new version

Download the new version and replace the old one the same way: extract the new zip over the old folder on Windows, drag the new app into Applications on a Mac and choose **Replace**, or swap in the new AppImage (or run the new `install.sh`) on Linux. Your plans and settings are kept.

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

## Features

### Plan your project

- Break work down into tasks and subtasks, typing straight into the task list like a spreadsheet.
- See everything on a **Gantt chart**: drag bars to reschedule, stretch them to change a duration, or drag one task onto another to link them.
- Link tasks so one starts when another finishes, or overlap them with leads and lags. Dates update automatically when anything changes.
- Mark the deadlines that matter and see the **critical path**, the chain of tasks that sets your finish date.
- Add **recurring tasks**, such as a weekly stand-up or a monthly report, and **split** a task to pause its work and resume it later.

![Task Information collects every detail of a task in one place](docs/images/task-information.png)

### See how the work flows

The **Network Diagram** shows a box per task, laid out automatically from the links between them, with the critical path in red. Drag from one box to another to link two tasks.

![The Network Diagram](docs/images/network.png)

### Work the plan on a Task Board

Drag cards from *Not Started* to *Done*, add your own columns, and let a column set a task's % complete. Late tasks stand out in red.

![The Task Board](docs/images/board.png)

### Hit your milestones

A milestone is a checkpoint with a **deliverable**, such as "Signed project charter" or "Beta ready", and the tasks that must be done to reach it. The Milestones view shows each one's date, deliverable and progress, and whether it's on track, slipping or late.

![Milestones, each with its deliverable, progress and status](docs/images/milestones.png)

### Manage people and partners

- Add team members, whole teams, partner companies, vendors, contractors and equipment, with their rates.
- Record vacations, holidays and part-time schedules. Work is scheduled around them automatically.
- Spot overbooked people in **Resource Usage** and **Workload**, then let **Level All** spread the work out for you.
- Move work from one person to another by dragging it in the **Team Planner**.

| | |
|---|---|
| ![Resource Usage](docs/images/resource-usage.png) **Resource Usage** shows hours per person per day; overbooked days stand out | ![Team Planner](docs/images/team-planner.png) **Team Planner**: drag work from one person to another |

### Estimate and track progress

- Estimate in hours, with optimistic, likely and pessimistic ranges, or with simple complexity points before anyone is assigned.
- Log hours on a weekly **Timesheet** and turn the time into progress with one click.
- Set a **status date** and run **Update Project** to bring progress up to date, or move work that didn't get done to after the status date.
- Save a **baseline** and compare it with the current plan: what slipped, what costs more, and how the project is trending. Keep up to eleven baselines and compare them side by side.

![The Tracking Gantt compares progress with the baseline](docs/images/tracking.png)

### Print and share

Print any view with a live print preview, or save it as a **PDF**: the Gantt charts, task and resource tables, the Network Diagram and the Task Board. Choose the paper, orientation and scale, how many pages wide it spreads, and your own headers and footers.

![The Print window, here printing the Task Board](docs/images/print-preview.png)

### Write in any language

Name tasks, people and anything else in your own language: Chinese, Japanese, Korean, Hebrew, Russian, Greek, Thai and many more. PlanCraft uses the fonts already on your computer, on screen and in PDFs, and right-to-left text reads right to left.

![Task names in several languages](docs/images/languages.png)

### Work with Microsoft Project

Open a Microsoft Project XML file (`.xml`) to import it, and use **File ▸ Export Microsoft Project XML…** to hand your plan to Microsoft Project or other planning tools. Tasks, outline, links, calendars, resources, assignments, baselines and custom fields come across, and PlanCraft tells you about anything the other side has no place for.

### Work across projects

- Make a task in one project depend on a task in another project file. When the other project slips, yours picks up the new dates the next time you open it or refresh links.
- Open several projects at once in tabs.

### Make it yours

- Light and **dark mode**, following your system or chosen under **View ▸ Theme**.
- Thirteen views (Gantt Chart, Tracking Gantt, Multiple Baselines Gantt, Network Diagram, Task Board, Task Usage, Calendar, Milestones, Resource Sheet, Resource Usage, Workload, Team Planner and Timesheet), with filters, search, grouping and your own columns and custom fields.
- Undo and redo up to 200 steps, and export to CSV for spreadsheets.

| | |
|---|---|
| ![Dark mode](docs/images/gantt-dark.png) **Dark mode**, here with the critical path shown | ![Calendar](docs/images/calendar.png) **Calendar** gives a month-at-a-glance view |
| ![Workload](docs/images/workload.png) **Workload** compares each person's planned hours with the hours they have | ![Start Page](docs/images/start-page.png) **Start Page** opens your recent projects or starts a new one |

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

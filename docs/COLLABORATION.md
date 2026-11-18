# Shared editing (design)

Several people work on the same plans over the office network: one machine hosts a folder of
plans, and everyone else opens them from it. Changes go to the host, which puts them in one
order, checks and recalculates each one, saves it safely, and passes it on to everyone else.
The model is the multi-user setup of desktop accounting programs: one copy is the real one, and
any PlanCraft can be the host.

Status: phase 1 (the sync engine, `plancraft_core::sync`) is built. Phases 2–5 are planned below.

## Off unless someone turns it on

Shared editing is entirely optional and opt-in. Out of the box:

- nothing listens on the network (no server, no port);
- nothing announces itself or looks for servers on the network (no mDNS until someone shares a
  folder, or opens the *Open from Server* dialog);
- no service is installed or started;
- plans open, edit and save exactly as they do now.

A host runs only when someone starts one (*Share Folder…*, `plancraft serve <folder>` or
`plancraft-server <folder>`). Their PlanCraft connects only when they open a plan from a
server. The service installers run only when chosen, and say what they install and how to
remove it. `plancraft_core::sync` is plain logic with no networking; nothing calls it for a plan
opened from a file.

## Why one host, not peer-to-peer merging

PlanCraft's dates are worked out from the plan. If every copy merged changes on its own, the
merged plan could break even though each change was fine on its own: a circular link, a task
under a deleted summary, or two people's PlanCraft showing different finish dates. With one host:

- there is one order of changes;
- every change passes the same checks as a local edit (`limits::check_edit`, recalculation),
  in one place;
- logins and permissions have one place to live.

Any PlanCraft can be the host, and servers on the office network are found by themselves. So it
feels like peer-to-peer without the merge problems.

## The sync engine (phase 1, `plancraft_core::sync`)

### Changes are patches

Every edit already goes through `Document::edit`, which keeps the plan from before. A `Patch` is
what differs between two versions:

- top-level values;
- for each list (tasks, resources, assignments, calendars, time entries, linked projects, custom
  fields, tables, board columns), items matched by id, field by field: added items whole,
  changed fields, deleted items, and the new order when the order changed.

The schedule (`sched`) is left out, because every copy recalculates it from the same plan and
gets the same dates. Id counters are left out too (see *Ids*). A list whose items cannot be
matched by key (two items with one key, from a damaged file) is replaced whole.

`diff(before, after)` works out a patch. `apply(plan, patch)` applies one and returns a plan for
the caller to check and recalculate. Applying refuses:

- changes to fixed values: the plan's id, its format and version, schedules, an item's key;
- an added item whose key is taken or does not match its own key;
- a change to an item that does not exist;
- anything that does not read back as a plan.

### Rebasing and conflicts

A change made from version *n* while the host is at *n+k* is rebased over the *k* changes made
since (`rebase`). These combine:

- different fields of the same task (Ana changes the duration, Ben the name);
- different tasks;
- items added by both people. Each new item stays right after the item it followed, so new tasks
  added at the same time keep their places.

These are conflicts, and the later change is not applied:

| Kind | Example |
|---|---|
| `BothChanged` | both set the duration of one task, to different values |
| `ChangedDeleted` | you renamed a task Ana deleted |
| `DeletedChanged` | you deleted a task Ana changed |
| `BothMoved` | both reordered the task list |
| `SameNewKey` | both added an item with one key (a lease ran out) |
| `NoLongerFits` | your new assignment is to a task Ana deleted; your new subtask sat under a summary Ana deleted |

`Conflict::describe` turns each one into a sentence, e.g. *"Ana changed the duration of task
'Pour slab' at the same time"*. A value both people set to the same thing is no conflict, and
neither is the same move made by both (only the items both had before are compared).

A new item goes after the item it followed. A patch records the summary each new task sits
under (`ListPatch::parents`): if someone else deleted that summary, the task would land under
another one, so it is a `NoLongerFits` conflict instead. Deleting only a neighbouring task is no
conflict; the new task stays under its summary.

Merging works field by field, and a field holding a list counts as one value. Two people adding
different predecessors to the same task at the same time is a conflict (`BothChanged` on
`predecessors`), not a merge. That is safe but coarse; finer merges of such lists can come later
if they turn out to matter.

### Host and replicas

`Host` holds the agreed plan, its `Version` and the last 1,000 accepted changes. `submit`:

1. returns `AlreadyApplied` for a change it has seen (the same `ChangeId` sent twice; it
   remembers the ids of the last 100,000 accepted changes, `Host::was_applied`);
2. returns `Resync` when the base is unknown or older than the kept changes, and refuses a
   change whose lists hold more items than a plan may, or whose order names an item twice
   (`check_size`: rebasing costs time in proportion to them);
3. rebases the change over everything since its base;
4. refuses it as a whole on any conflict (`Conflicts`);
5. applies it, checks it and recalculates, behind `guard::catch`. A plan out of range, a damaged
   patch, a caught panic, or a new item with an id above the host's counters (one no lease
   handed out) is `Refused`, and nothing changes;
6. bumps the version and returns the `Entry` for everyone, the author included. The entry carries
   the change's whole effect, worked out after recalculation, so every replica ends up with the
   host's plan exactly.

After a restart, `Host::resume` continues at the version the plan was saved at, with the ids of
the changes accepted before that save (`Host::applied_ids`). `Host::replay` and
`Host::replay_lease` then apply the journal written since, entries and leases in order.

Each person's PlanCraft keeps a `Replica`:

- `confirmed`: the host's plan at its version;
- `pending`: everything this person changed that the host has not confirmed yet;
- `local`: `confirmed` with `pending` applied, which is what the person sees and edits.

A local edit hands the new plan to `edited`. `next_change` sends everything pending, at most one
change at a time; edits made while a change is on its way go with the next one. When the host's
entries come in (`receive`):

- someone else's change is applied to `confirmed`, and `pending` is rebased over it. Parts that
  clash are dropped and reported (`Merged::conflicts`); the rest of the person's work stays.
- our own change coming back confirms it; what remains pending is whatever the person did since.
- if the merged plan fails the checks (an assignment to a task someone deleted), the pending work
  is tried piece by piece. Pieces are a value, a new item, one item's changes, a deletion or an
  order, each tried twice so one that needs another goes in. Pieces that no longer fit are
  dropped and reported as `NoLongerFits`; all unconfirmed work is dropped when nothing fits or
  there are more than 200 pieces. Each piece is a full check and recalculation, so on a large
  plan this can take seconds: phase 3 runs merging off the user interface thread.
- if the host refuses a change for a reason this copy cannot see (nothing changed since it was
  sent, yet the host would not take it), all unconfirmed work is dropped rather than sent again
  and again.

A missing entry (`SyncError::Gap`), a `Resync` reply, or a reconnection calls `resync` with the
host's plan. The replica compares the old confirmed plan with the new one and rebases its
pending work over the difference, so work done while disconnected is kept, except where it
clashes. A change on its way when the connection dropped may have been applied, with the reply
lost: the replica asks the host (`Host::was_applied`) and passes the answer to `resync`, which
then treats that change as confirmed. Without that, someone who changed those values back
meanwhile would see their change silently undone when the old change was sent again. When the
host still has the missed entries, catching up with `Host::since` is better still: the
replica's own entry confirms its change by id.

### Ids

New tasks get their ids from the plan's counters. Two people adding a task at the same time
would take the same id. To prevent that:

- the host leases each replica a block of 1,000 ids of every kind (`Host::lease`), and moves its
  own counters past the block;
- the replica counts from its lease (`Replica::ids`) and asks for another block when
  `Replica::ids_left` runs low (phase 3). Its cursor never goes back, not even when an undo
  restores older counters, and it refuses a new block that starts below ids it already used;
- if a block runs out anyway, a clashing id is refused (`SameNewKey`, or `Exists` on apply), and
  two items are never merged into one;
- a lease past `limits::MAX_ID` is refused.

The host raises its counters above every id in use after each change (`fit_counters`), so the
saved file stays correct for later standalone use. A replica's own copy is not raised (that would
step into someone else's lease). A "save a copy" from a replica must call `fit_counters` first.

### Undo

Undo in a shared plan cannot restore a whole-project snapshot, because that would also undo
other people's changes made since. Phase 3 replaces it for shared plans with inverse patches:
undoing your change applies `diff(after, before)` of that change, rebased over later entries.
If someone changed the same values since, the undo is refused and the app says who changed them.

### Tests

- Unit tests (`sync/tests.rs`): round trips; field merges; each conflict kind; adds at the same
  time; duplicate sends; lost replies; hostile patches refused with the plan untouched (repeated
  or oversized orders, ids no lease handed out); resync; restart and replay; leases.
- Property sweep (`tests/hostile.rs` › `shared_editing`): three people make random edits (every
  edit in the hostile sweep, leveling and baselines included), and changes, replies and dropped
  connections arrive in random order. In the end, every copy equals the host's plan, schedules
  included; ids stay unique; the host's plan stays within limits; and an accepted change always
  applies on every copy. It checks that everyone agrees, not that nothing was undone: the unit
  tests cover changes that must not come back (lost replies).
- Speed (`tests/speed.rs`): one shared edit round on 2,000 tasks (edit, diff, host check and
  recalculation, replica update) takes about 90 ms here, with a 400 ms limit. The diff alone
  takes about 16 ms.

## Phase 2: the server

- **Program:** a new crate `plancraft-server` depending only on `plancraft-core`, with no user
  interface (a few MB), plus the same code behind `plancraft serve <folder>` in the app's binary.
  Plain threads (one per connection), like the MCP server.
- **Folder:** it serves the `.plancraft` files in one folder, given on the command line. Each
  plan opens on first use and is released when nobody has it open. It holds the plan's lock file
  while serving it, so anyone opening the file directly sees "open in PlanCraft Server on …".
- **Durability:** each accepted entry is appended to a journal next to the plan
  (`.NAME.journal`, fsync before the reply). The plan is saved with `io::write_atomic` every
  minute and when the last person leaves, then the journal is cleared. After a crash, the server
  reloads the plan and replays the journal.
  Required, not optional:
  - The version the plan was saved at and the accepted change ids (`Host::applied_ids`) are
    stored with it (`Host::resume`). Without the ids, a replica whose reply went missing could
    not tell that its change went in.
  - The journal records each lease before it is handed out (`Host::replay_lease`). Without it,
    a restarted host would refuse every new item from a lease it forgot, or lease the same block
    twice.
  - Requests on one connection are handled in order. After a reconnection, the host finishes or
    drops whatever the old connection had sent before it answers `was_applied`.
- **Protocol:** length-prefixed JSON frames with a size cap, read as hostile input, like files
  and MCP arguments:
  - `hello` and `login`;
  - `open` (returns the plan, its version and an id lease);
  - `change` and `entry`;
  - `rejected`, `lease` and `resync`;
  - `presence` (who is editing which task).

  The garbage sweep (`plancraft-mcp/tests/garbage.rs`) gets a sibling for every frame type.
- **Logins:**
  - a users file on the server, owner-only, with Argon2id password hashes (the `argon2` crate,
    pure Rust);
  - each user is a viewer, editor or admin, for the whole folder or for single plans;
  - failed logins are rate-limited;
  - `plancraft-server user add|remove|passwd` manages users, and later an admin panel in the app.
- **Encryption:** normal TLS through `rustls` needs `aws-lc-rs` or `ring`, which compile C and
  are banned by `deny.toml`. The plan is the Noise protocol (`snow`, with its pure-Rust crypto):
  - the server has a key pair, kept owner-only;
  - clients trust it on first connect and pin its fingerprint, as SSH does;
  - the password is checked inside the encrypted connection.

  `rustls` with the `rustls-rustcrypto` backend is the fallback if standard certificates are ever
  needed. Each crate is checked with `cargo deny check` when added.
- **Scope:** the office network or a VPN. It is not meant to be exposed to the internet.
  5–10 people at once is the target, and there is no hard limit below about 50.

## Phase 3: PlanCraft as a client

- *File › Open from Server…*: the address (or a server found on the network), then user name and
  password, then the plan.
- The plan opens as a normal document whose edits go through the replica:
  - a status mark shows connected, sending, or offline;
  - other people's changes appear as they come in;
  - a coloured marker shows who is editing which task, as a warning (a hard lock is optional);
  - conflicts and dropped work show in a bar, with a recovery copy of the pre-merge version.
- Offline edits wait and are sent on reconnection.
- Undo uses inverse patches (see *Undo*).
- MCP agents attached to that PlanCraft edit through the same replica, shown as "Claude for Ana".
- File watching is off for plans opened from a server.

## Phase 4: discovery and sharing

- mDNS (`mdns-sd`, pure Rust): a running server announces itself, and the *Open from Server*
  dialog lists the servers it finds. Neither happens unless someone shares a folder or opens
  the dialog.
- *File › Share Folder…* starts a server inside the app, or launches `plancraft-server`, for a
  chosen folder. It shows the address and fingerprint and has a user admin panel.

## Phase 5: packaging and services

- `cargo xtask dist` ships `plancraft-server` next to the app on every platform. On the Mac, it
  is signed and notarized inside the bundle with the app.
- *Start PlanCraft Server at login*, per user, with no admin rights:
  - a systemd user unit on Linux;
  - a LaunchAgent on macOS;
  - a Task Scheduler logon task on Windows.
- *Install PlanCraft Service*, system-wide for a dedicated office machine, with admin rights:
  - a systemd system unit through `pkexec` on Linux;
  - a LaunchDaemon on macOS;
  - a Windows service through the `windows-service` crate (pure Rust).
- Both can be removed from the same place they were installed from.

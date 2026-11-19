# Shared editing (design)

Several people work on the same plans over the office network: one machine hosts a folder of
plans, and everyone else opens them from it. Changes go to the host, which puts them in one
order, checks and recalculates each one, saves it safely, and passes it on to everyone else.
The model is the multi-user setup of desktop accounting programs: one copy is the real one, and
any PlanCraft can be the host.

Status: phase 1 (the sync engine, `plancraft_core::sync`) and phase 2 (the server, `plancraft-server`)
are built. Phases 3–5 are planned below.

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

## Phase 2: the server (`plancraft-server`)

### Running it

```text
plancraft-server user add ana --role admin      # asks for a password twice
plancraft-server "/srv/Plans"                    # shares the folder until stopped
plancraft serve "/srv/Plans"                     # the same server, from PlanCraft's own program
```

- A crate with no user interface that depends only on `plancraft-core`: `plancraft-server`, with
  the same code behind `plancraft serve`. Plain threads, like the MCP server: a reader and a
  writer per connection, plus one for upkeep.
- It listens on port 47340 on every network unless `--port` / `--bind` say otherwise, and only
  when started. It refuses to start while nobody can sign in.
- Ctrl+C, or a service manager's stop signal, stops it cleanly: every plan is saved and its lock
  given back, and everyone is told, before the program exits. A server killed outright recovers
  from the journals.
- Its private folder (`--config`, by default `server` in PlanCraft's settings folder) holds its key
  pair (`server-key.json`) and the users file (`users.json`), both owner-only.
- `plancraft-server user add|passwd|role|remove|list` manages people; `--password-stdin` reads
  the password from a script. `plancraft-server fingerprint` shows the key's fingerprint.

### The folder

- It serves the `.plancraft` files directly in the folder: no subfolders, hidden files, symbolic
  links or names spelled differently from the folder's own spelling (on Windows and the Mac,
  `plan.plancraft` and `Plan.plancraft` are one file and must never be shared twice).
- A plan opens when its first person asks for it and closes when its last person leaves (saved,
  lock given back). While shared, the server holds its lock file, so anyone opening the file
  directly sees "open in PlanCraft Server 0.6.0 on …". A plan that is open in PlanCraft elsewhere
  is not shared until it is closed there.

### Durability (`plan.rs`, `journal.rs`)

Each shared plan has a journal next to it (`.NAME.journal`, owner-only, one JSON record per line):

1. A header: the version the plan file holds, a hash (BLAKE2s) of that file, the run of the
   plan (`epoch`, see below) and the accepted change ids (`Host::applied_ids`).
2. Every accepted change, written and flushed to disk (`sync_data`) before anyone hears of it.
3. Every block of ids, written before it is handed out (`Host::replay_lease` on recovery).
4. A `saving` record with the new version and hash, just before the plan file is replaced.

The plan file is saved with `io::write_atomic` three seconds after changes stop, at least every
minute while they continue, once 64 MB were written to the journal, when the last person leaves
and when the server stops; the journal then starts afresh with a new header. It is written
through the file handle that created it, never by opening its name again, and the plan file is
saved with `Privacy::NoLinks`: a link planted in place of either is replaced, never followed.
Plan files, journals and lock files are opened with `io::open_regular`, which checks the file it
actually opened (not the name) and never blocks: a FIFO or device swapped in for a plan is
refused at once instead of hanging the plan's lock. Opening a plan:

- the file's hash matches the header: the file is at the header's version; replay what follows;
- it matches a `saving` record: the power failed after the plan was written but before the
  journal started afresh; the file is at that version, and only later records are replayed;
- neither: the file was changed outside the server (someone edited it while it was not shared).
  The plan starts a new run (`epoch`) at version 1, so no version from before is mistaken for one
  after; a copy reconnecting with an old epoch gets the whole plan. Changes in the journal that
  never reached the file are kept in a hidden `.NAME.journal unused <time>.journal` and logged.

A line cut short by a crash ends the journal. A damaged line (the header included) stops the
replay there; the journal is kept aside the same way, and the plan starts a new epoch, because
copies may know changes lost with the damaged part at versions this run will give to others. A
recovered plan is saved at once.

Any failed journal write (a full or failing disk), the save record and restarting the journal
included, ends all writing to it for good, from every connection: a half-written line followed by
more records would hide those records from recovery. The change is not confirmed, the plan is closed for everyone without
saving, and the next opening starts from what is on disk. If the plan file was changed on disk
while shared (someone ignored the lock), or replaced by a link, it is not overwritten: the shared
plan is saved to a new hidden `.NAME shared copy <time>.plancraft` instead (never through a link,
and not listed as a plan of its own, so it cannot bypass the plan's access rules), and the log
says so. A plan file that cannot be written is tried again after 30 seconds.

An editor cannot make a plan unusable:
- A change that would grow it beyond 56 MB as saved (below the 64 MB any plan file may hold) is
  refused (`Host::submit_checked`, before anything changes). Past a sixteenth of that, by an
  estimate counting each change eight times its size, the plan a change would make is measured
  exactly. Adding tasks grows a saved plan about 3.5 times the change; hostile minimal tasks
  about 40 times; overshooting would take over 120. The size starts from what the plan saves at
  when it opens, not the file's bytes (a compact file grows when saved), and a plan already
  over 56 MB as saved is not shared.
- While saves keep failing (a plan file made read-only), changes are refused once the journal
  holds 512 MB, half of what recovery reads, so the plan always opens again; until then they
  are accepted, and a save is tried at most every 30 seconds, not after each change.
- Each person gets 20 blocks of new ids in a row per plan, then one every 30 seconds, kept by the
  server across reopenings, so the ids (about a million blocks) cannot be used up.
- The host keeps at most 64 MB of recent changes for catching up (`sync::LOG_BYTES`), whatever
  their number.

### Protocol (`protocol.rs`)

One JSON object per frame, tagged by `type`; see the types for the fields.

- From PlanCraft: `hello`, `login`, `list`, `open` (with the version it already has and the change
  whose reply went missing), `change`, `lease`, `resync`, `presence`, `ping`.
- From the server: `welcome`, `logged_in`, `login_failed` (with how long to wait), `plans`,
  `opened` (the whole plan or only the missed changes, a lease for editors, whether the in-flight
  change went in, who is there), `entry` (to everyone, its author included), `rejected` (conflicts
  or a reason), `snapshot`, `leased`, `people`, `pong`, `error`, `closing`.
- A connection serves one plan once it has opened it. Its requests are handled one at a time, in
  order, under the plan's lock. A copy that reconnects (same `ClientId`) replaces its old
  connection, which is closed, and requests the old one still sends are dropped; so the answer to
  "did my change go in?" is final.
- Frames are capped before they are read: 64 KiB, except 65 MiB from an editor with a plan open
  (a plan's worth of new items), and 128 MiB from the server. Missed changes beyond 32 MB are sent
  as the whole plan instead.
- Signing in must take under 30 seconds in all (checked by upkeep, however slowly bytes trickle
  in) and 10 requests; at most half the connections may be waiting to sign in, and at most 4 from
  one address, counted before the connection's thread starts. 20 requests that
  are not understood close the connection. A connection idle for two minutes is closed (PlanCraft pings every 30
  seconds).
- Nothing that holds a plan waits on a connection: a connection whose messages pile up (4,096, or
  160 MB) is closed rather than holding everyone up, and catches up when it reconnects. When 512
  MB wait for everyone together, the connection holding the most is closed, not whoever sends
  next. A message to several people is held, and counted, once. So a reader that never reads, or asks for the whole plan over and over, only cuts itself
  off. Each frame counts its bytes back when it is dropped, sent or not.
- At most 64 connections, 16 from one address (each takes three file handles; macOS allows a
  program 256).
- A copy belongs to the person who opened it: another user cannot take over its connection.

### Signing in (`users.rs`)

- Argon2id hashes (`argon2`, pure Rust) in the users file; never the passwords, and never in debug
  output. The file is read again when it changes, so user commands take effect without a restart,
  on open connections too: someone removed or no longer allowed is disconnected within a moment,
  even if they only listen or ping.
- Roles: `viewer` (opens and watches), `editor` (also changes), `admin` (also manages people, from
  the app in phase 4), and `none`; one for the whole folder plus any per plan. A plan someone may
  not open is not listed, and opening it gives the same answer as a plan that does not exist.
- Per-plan rules match plan names ignoring case and Unicode form (macOS stores accented names
  decomposed, people type them composed), one rule per plan: `--plan` must be a plan file name,
  and replaces a rule written another way. If a hand-edited file holds two rules for one plan,
  the stricter applies.
- After five failed sign-ins for one name from one address, or from one address for any names,
  each try waits twice as long as the last (1 s up to 15 minutes). A try counts as failed from
  the moment it starts, so tries sent at once cannot all slip through; names count per address,
  so nobody can lock someone out from another computer. Addresses count exactly, IPv6 too: on
  an office network every computer shares one /64. After 30 failures for one name from all
  addresses together, each try for that name waits up to a minute, so someone guessing from many
  addresses gets about a try a minute. Addresses where that name signed in during the last 30
  days skip this wait (the server remembers them until it restarts), so the guesser cannot keep
  the real person out.
  Unknown names take as long to check as known ones; at most two passwords are checked at once.
- On Unix the key and users files are 0600 and made so again if loosened. On Windows they rely on
  the folder's permissions (owner-only access lists need platform code: phase 5).

### Encryption (`wire.rs`)

- Normal TLS through `rustls` needs `aws-lc-rs` or `ring`, which compile C and are banned by
  `deny.toml`. The server uses the Noise protocol instead: `Noise_NX_25519_ChaChaPoly_BLAKE2s`
  through `snow` with its pure-Rust crypto (no `ring`).
- The server has a long-term key pair (its two halves are checked to belong together on loading).
  A client learns its key in the handshake and checks it
  against the fingerprint it pinned on its first visit (`Trust::Pinned`), as SSH does; on a first
  visit (`Trust::FirstUse`) the person confirms the fingerprint the server prints at start-up.
- The password travels only inside the encrypted connection; tampered bytes end the connection.
- `rustls` with the `rustls-rustcrypto` backend stays the fallback if standard certificates are
  ever needed.

### Scope

The office network or a VPN; it is not meant to be exposed to the internet. 5–10 people at once is
the target, with no hard limit below the connection caps.

### Tests

- `plan/tests.rs`: crashes at each step of a save, a torn or damaged journal (a damaged header,
  a new epoch that sticks), a plan changed outside the server (with and without unsaved shared
  changes), a plan file changed or replaced by a link while shared, a plan open in PlanCraft
  elsewhere, reconnections and late requests, a copy taken over by someone else, a slow
  connection, a full disk for a change and for a save and nothing written after it (`/dev/full`,
  Linux), large journals saved early, catch-up too large, a plan grown too large and the size
  estimate's margin (hostile minimal items too), the shared queue budget (closing the connection
  holding the most, a broadcast counted once) and its byte counts, changes refused while saves
  fail and no save retried before its wait, the size measured as saved when a plan opens, a FIFO in place of the plan or journal, rationed leases across reopenings, access taken
  away.
- `tests/server.rs`: a real server over the network: sign-ins and access, refused requests,
  another protocol, a pinned client meeting another key, password guessing (one at a time and
  all at once), connections that never sign in or trickle bytes, a few computers holding every
  place for signing in, large frames from viewers,
  viewers flooding without reading, someone removed while signed in (requesting, listening or
  pinging, or kept out of one plan), two people editing, conflicts, viewers, saving when the last person leaves,
  restarts, stopping (also from another thread, as Ctrl+C does, with a large plan).
- `tests/garbage.rs`: every message type with junk in each field, at every stage of a connection,
  plus nearly-right changes and random patches. A panic hook fails the test on any panic in the
  process, even one a guard caught, and the shared plan must still read back within limits.
- `tests/cli.rs`: managing users and starting a server from the command line.

### Left for phase 3 (the client)

- Ping every 30 seconds; reconnect with `known` and `in_flight`; on a new `epoch`, start a fresh
  replica from the snapshot and ask for a lease.
- An edit that adds more new items than the lease has left (a paste of over 1,000 tasks) must ask
  for more ids first: the server refuses ids no lease handed out.
- Opening a plan file directly while its journal holds changes not yet saved (the server was
  stopped by a crash and not restarted) should warn and offer to start the server.
- A server running inside the app (phase 4) shares the app's lock identity, so it must refuse a
  plan the app has open in a window.

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

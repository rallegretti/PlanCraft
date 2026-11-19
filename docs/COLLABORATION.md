# Shared editing (design)

Several people work on the same plans over the office network: one machine hosts a folder of
plans, and everyone else opens them from it. Changes go to the host, which puts them in one
order, checks and recalculates each one, saves it safely, and passes it on to everyone else.
The model is the multi-user setup of desktop accounting programs: one copy is the real one, and
any PlanCraft can be the host.

Status: phase 1 (the sync engine, `plancraft_core::sync`), phase 2 (the server, `plancraft-server`)
and phase 3 (PlanCraft as a client) are built. Phases 4–5 are planned below.

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
| `DeletedChanged` | you deleted a task Ana changed, or a summary Ana just put a task under |
| `BothMoved` | both reordered the task list |
| `SameNewKey` | both added an item with one key (a lease ran out) |
| `NoLongerFits` | your new assignment is to a task Ana deleted; your new, moved or indented subtask sat under a summary Ana deleted |

`Conflict::describe` turns each one into a sentence, e.g. *"Ana changed the duration of task
'Pour slab' at the same time"*. A value both people set to the same thing is no conflict, and
neither is the same move made by both (only the items both had before are compared).

A new item goes after the item it followed. A patch records the summary each task sits under
when it is new or came to sit under another summary (moved, indented, outdented:
`ListPatch::parents`). Either way round, a deleted summary never quietly takes someone's task
somewhere else:

- if someone else deleted that summary, a new task would land under another one: it is left out
  (`NoLongerFits`); a moved or indented one keeps its old place, its move dropped with the
  outline levels it set;
- deleting a summary someone else just put a task under is `DeletedChanged`: the summary stays
  (the rest of the deletion, its other subtasks, still goes in).

Deleting only a neighbouring task is no conflict; the new task stays under its summary. When a
move is dropped (`BothMoved`, or the case above), the outline levels it set go with it: kept
alone they would put tasks under summaries nobody chose.

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
   host's plan exactly. A change with no effect makes no version (no journal record, no
   broadcast): it is answered as `AlreadyApplied`, and the author settles on the host's plan.

Values must come back from JSON exactly as they went: `serde_json` is built with
`float_roundtrip`. Without it some numbers (a resource's units of 300/1440, say) read back off in
the last place, the copy never matched the host's, and the replica sent its "change" for ever.
The `shared_editing` sweep sends every change, entry and whole plan through JSON.

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
other people's changes made since. Shared plans undo with inverse patches instead
(`sync::reverse`): each undo step keeps the plan before and after the step, and undoing applies
`diff(after, before)` rebased over everything that changed since. If someone changed any of the
same values since, the undo is refused, nothing changes, and the app says what was changed; that
step leaves the undo list (it can no longer be taken back without undoing theirs), so the steps
before it stay within reach. Redo works the same way.

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
says so. The journal's header names that copy (`saved_to`): until a person keeps one version as
the plan (and removes the journal if they keep the outside one), the server refuses to open the
plan again, rather than starting afresh from the outside file and leaving every shared change in
the copy unnoticed. A plan file that cannot be written is tried again after 30 seconds. The
file's stamp after a save is taken from the file as written, before it takes the plan's name, so
another program's write landing just after is never taken for the server's own.

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
- A change may set at most 256 values of one item (or of the plan), with names of at most 64
  characters, and 200,000 values in all (`sync::check_size`, before rebasing): rebasing costs
  time in proportion to them, and one change of 300,000 made-up fields held a plan for most of
  a minute.
- Large frames (over 1 MiB) from all connections together may hold at most 256 MB while they are
  read and handled (`server::MAX_INBOUND_BYTES`); one that does not fit closes its connection,
  and the client sends it again after reconnecting.

Nor can a viewer flood the others: who is where goes out from upkeep (every 200 ms at most),
not once per message, and one connection gets the whole plan at most every 2 seconds (asks
in between are answered once, from upkeep).

The server does not check whose block a new item's id came from: an undo may rightly bring back a
task with an id from someone else's block. Each copy refuses ids outside its own blocks instead
(`Replica::new_ids_outside_leases`).

### Protocol (`protocol.rs`)

One JSON object per frame, tagged by `type`; see the types for the fields.

- From PlanCraft: `hello`, `login`, `list`, `open` (with the version it already has and the change
  whose reply went missing), `change`, `lease`, `resync`, `presence`, `ping`.
- From the server: `welcome`, `logged_in`, `login_failed` (with how long to wait, and whether the name or password was wrong), `plans`,
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
  addresses gets about a try a minute. Networks where that name signed in during the last 30
  days skip this wait (an IPv4 address, or an IPv6 /64, as a computer's temporary IPv6 addresses
  change within it), so the guesser cannot keep the real person out. The list is kept, owner-only,
  in `trusted-sign-ins.json` in the server's private folder, so a restart does not lose it.
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

### Left for later phases

- A server running inside the app (phase 4) shares the app's lock identity, so it must refuse a
  plan the app has open in a window.
- Starting the server from the warning shown when a shared plan's file is opened directly
  (phase 4, *Share Folder…*).

## Phase 3: PlanCraft as a client

### Opening a plan

*File › Open from Server…* (also on the Start Page) asks for the server's address (`host`,
`host:port`, an IPv6 address; port 47340 when none is given), user name and password:

- On the first visit the server's key fingerprint is shown before any password is sent; the
  person checks it against the one the server prints, and PlanCraft pins it in its settings once
  they sign in with it. A server showing another key later is refused (as SSH does), with a way to
  forget the old key if whoever runs the server says it changed.
- The settings remember servers (address, user name, fingerprint, last plan; never a password).
  The password is held only by the open window and by each open plan's link, to sign in again
  after the connection drops.
- Connecting, signing in and opening run on a background thread; the window keeps drawing.

The plan opens as a document with no file (`Document::shared`): no lock file, no file watching,
no Save (the server saves every change; *Save As* writes a copy of one's own, with its id
counters raised so it stands alone). Opening the same plan again from the same server shows the
open one. A viewer's plan refuses every change.

### Keeping it in step (`plancraft_server::link`)

Each plan opened from a server has a `Link` with two threads: a reader, and a session that owns
the `Replica` and the connection. Merging never runs on the app's thread.

- The app hands on each local edit as the plan before and after it, numbered. Edits are noticed
  in `Document::try_edit` itself, so the UI, undo and MCP agents all go the same way; several
  edits made in one frame go as one.
- The link hands back the plan to show with the number of the last edit it includes. The app
  shows it only when that is every edit handed on: a plan worked out before the person's latest
  edit never hides that edit. An edit made on a plan that was already behind (other people's
  changes arrived meanwhile) is fitted onto the current one (`rebase`); where both changed the
  same value, theirs stays and the person is told.
- Pings every 30 seconds; 75 seconds of silence counts as a lost connection. Reconnecting waits 1,
  2, 4 … up to 30 seconds between tries, signs in again and catches up with `known` and
  `in_flight` (missed entries, or the whole plan when the server has started a new run of it).
  Edits made offline wait and go out on reconnection.
- The server says which refusals are final (`for_good` on `error` and `closing`): no access, the
  plan gone, signed out for good, another protocol. Only those, a wrong password (the password
  changed) and a different server key stop the retries; the plan stays open as it is
  (read-only), with *Save As* to keep a copy, and opening it again from the server signs in
  afresh beside it. Anything else (the server starting, stopping or busy) is retried.
- A change the server cannot take for now (it cannot save the plan: `rejected` with `retry`) is
  kept and sent again after 30 seconds. One refused because the person may now only view the
  plan (`rejected` with their `role`) is kept, unsent, for *Save As*; the plan turns read-only.
  Other refusals drop the work and say why.
- Missed entries with a change on its way reconnect (the server then says whether that change
  went in); without one, the whole plan is asked for, once per connection (a new connection asks
  again). When catching up after reconnecting fails part way, the whole plan is asked for and
  what reopening said about the change on its way is kept for the answer (a resync's answer does
  not know of it): a change that went in is never sent again. Messages still arriving from a
  dropped connection are ignored. A plan from the server that cannot be read is not asked for
  again until the next connection.
- A wait before signing in again that the server asks for is kept to at most an hour.
- The latest plan for the app is kept in one slot, not queued: a window that runs no frames for a
  while holds one copy of the plan, not one per change that came in.
- The link asks for a new block of ids when fewer than 250 are left, and again 30 seconds after the
  server rationed it. Opened without a lease (the server rations them: 20 per person and plan,
  then one every 30 seconds), new items wait for one; changes to existing items go through. An
  edit adding more new items than a block holds (1,000) is refused with a message, rather than
  using ids no lease handed out; so is a new item whose id neither the current block nor the one
  before it handed out (an edit made before a new block arrived counts on from the old one, and
  past its end are someone else's ids). An edit fitted onto a plan that moved on counts its ids
  from the plan as it is now, raised only past its own new items.
- If the link's thread ever hits an internal error, the plan shows as signed out (never a link
  that silently stopped).

### What the person sees

- The status bar: a coloured dot and the server with *Connected*, *Sending*, *Connecting*,
  *Offline* or *Signed out* (details on hover), *view only* for viewers, and who else is in the
  plan. Tabs of plans opened from a server show 👥.
- The task table's indicators column marks tasks others are on (👥, with their names on hover):
  a warning, not a lock.
- Work that was dropped (it clashed with someone's change, or no longer fitted the plan) shows in
  a bar above the plan, with *Open Your Version*: the person's plan just before, as a new unsaved
  plan. While the bar shows, the plan counts as unsaved: closing it (or PlanCraft) first opens
  that version as a plan of its own, so it is asked about and kept by recovery copies.
- Changes not yet confirmed count as unsaved: closing asks, and *Save* there writes a copy.
- Undo passes over steps that no longer change anything (an edit the server did not keep).
- Addresses are compared in lower case without a final dot, so a known server typed another way
  is still known. A "first visit" to a server whose name matches a known one with another key
  warns that it may be another computer pretending to be it. A mistyped password after
  confirming a key goes back to the form, with the key still confirmed.
- Opening a plan's file directly while a server's journal sits next to it (the server has it
  open, or stopped before saving) warns to open it from the server instead.
- MCP agents attached to that PlanCraft edit through the same document and link; their changes go
  out labelled as an AI agent's (the server records the signed-in person as the author).

### Tests

- `crates/plancraft-server/tests/link.rs`: links against a real server: two people see each
  other's changes and places; edits made while the server is down go out when it is back; a clash
  is reported and both end with one plan; a viewer cannot change the plan; a changed password
  stops the retries; ids are topped up and too many at once are refused; an editor opened without
  a lease adds nothing until one comes; an editor made a viewer keeps unsent changes; a plan
  changed outside the server is loaded again.
- `crates/plancraft-server/tests/link_scripted.rs`: a link against a scripted server, for states
  a real server rarely reaches: a gap after reconnecting asks for the plan again; a gap with a
  change on its way reconnects; a gap while catching up does not send an applied change again; an
  edit right after a refused one is kept; an absurd wait before signing in again is not taken as
  none.
- The app's `shared_tests` (in `app.rs`): the real app with a plan from a real server: edits both
  ways, status bar, people and their tasks, Save; Save As writes a copy (it once froze the app);
  undo keeps others' changes and is refused over theirs; a viewer; closing disconnects; work the
  server dropped is kept when the plan closes; a signed-out plan is not reused; the journal
  warning. `dialogs/server.rs` tests the window's steps (first visit, pinned key, second visit,
  wrong password, another key, a mistyped password on a first visit).
- `hostile.rs` › `shared_editing` › `shared_undo_keeps_the_plan_valid`: random own edits, other
  people's edits and undo/redo on a shared document never panic (not even one a guard catches)
  and always leave a valid plan.

### Left for later

- Agents shown as "Claude for Ana" to the others needs the author to travel with each change;
  for now the label says it was an AI agent's.
- Who changed what recently (a list of the server's entries with their authors).
- *Open Your Version* opens the plan from just before the latest dropped work; earlier dropped
  work listed in the same bar is not in it.
- A hard lock on a task someone is editing (the marker is a warning only).
- A sweep property that a successful shared undo leaves every value others changed since as it
  was (the direct tests in `sync/tests.rs` and `workspace.rs` cover the known cases).

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

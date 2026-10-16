# PlanCraft — notes for AI assistants (AGENTS.md and CLAUDE.md are identical; keep them in sync)

- Rust workspace: `plancraft-core` (logic), `plancraft-mcp` (MCP server), `plancraft-app` (egui desktop app, binary `plancraft`).
- Hard rule from the owner: everything in Rust; no calling into C/Python/other languages for features (scripts/adapters are fine when unavoidable). `cargo deny check` bans crates that compile C (`deny.toml`); `cargo tree -p plancraft-app -i cc -e build` should print nothing.
- Brand: deep blue accent (`theme::ACCENT` #1B3A8C), gold milestone diamond; the logo must never resemble a "P" letter.
- All project mutations go through `ops.rs` methods on `Project`, wrapped in `Document::edit`/`try_edit` (undo + recalculation). MCP edits use `ChangeSource::Agent`.
- Times are working minutes; finishes land on the end of the working day (17:00).
- egui 0.36: `App::ui(&mut self, ui, frame)`, `egui::Panel::top/bottom`, `Popup::menu`, `zoom_delta()` for Ctrl+wheel. Many emoji glyphs are missing from the default fonts; check new icon glyphs visually.
- Before every commit: `cargo xtask ci` (fmt, clippy -D warnings, tests, release speed gate, cargo deny). Test only: `cargo test --workspace`. Visual check: `PLANCRAFT_VIEW="Gantt Chart" PLANCRAFT_CMD=TaskInfo PLANCRAFT_SCREENSHOT=out.png cargo run -p plancraft-app -- --demo` writes a PNG and exits.
- Formatting: `cargo fmt --all` (rustfmt.toml: max_width 140).
- UI chrome: a two-row command bar (`menubar.rs`): plain tabs plus one compact toolbar row, with the view selector spanning both rows. Do NOT reintroduce a Microsoft-Ribbon-style UI (large icon buttons, stacked small buttons, group captions under button groups); the owner explicitly wants to avoid resembling the Ribbon.
- The Start Page (`start_page.rs`) is a centered card layout under the title band. Avoid Office-backstage patterns (full-height colored left nav, Back arrow, Info/Account/Options nav list).
- Packaging: `cargo xtask dist <mac|linux|windows|all>` (see docs/BUILDING.md). Build hosts are SSH aliases in the git-ignored `dist-hosts.toml`. Releases are built only on the owner's own machines, never in GitHub Actions; Actions runs checks only (`.github/workflows/ci.yml`: Linux on every push/PR, macOS and Windows on main/weekly/manual because private-repo minutes cost more). Packages are unsigned for now.
- Colors: always paint with `theme::pal().<COLOR>` (light and dark palettes in theme.rs); never hard-code `Color32` values in views, or dark mode breaks. The contrast test in theme.rs guards text readability.

## Never crash (a crash loses someone's plan)
- No `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!` or indexing/slicing (`v[i]`, `&s[a..b]`) in shipped code: workspace clippy lints deny them (`Cargo.toml`); tests may use them (`clippy.toml`, plus a file-level `#![allow(...)]` in `tests/*.rs`). Return `Result`/`Option`; use `.get()`; slice strings only with `.get(..)` (clippy does not flag `&str[a..b]`, so check by hand); check floats are finite; use checked/saturating integer maths; move dates with `calendar::shift` or `checked_*`, never `date + Duration`.
- Last-resort guards (safety nets, not substitutes for `Result`): `plancraft_core::guard::catch` turns a panic into an error. `Document::edit/try_edit` run each edit behind it, validate the result against `limits::check_edit`, and roll the project back on any error (`PlanError::Limit` / `Internal`). `schedule::recalculate` is guarded too (`try_recalculate` to roll back instead). The MCP server guards each tool call; the desktop app guards each frame and writes recovery copies of unsaved work.
- Treat files and MCP arguments as hostile: `plancraft_core::limits` caps counts, durations (5 years), dates (1900–2300), numbers and text. Opening a file clamps values into range (`io::load_with_notes` returns what changed) and refuses oversized files (64 MiB), too many items or duplicate ids. Lock the shared workspace with `lock().unwrap_or_else(PoisonError::into_inner)`.
- Every crash fix comes with a regression test. Extend the sweeps when adding parsers, ops or tools: `crates/plancraft-core/tests/hostile.rs` (property tests for parsers, random edit sequences) and `crates/plancraft-mcp/tests/garbage.rs` (every tool × every argument × junk values; a panic hook fails the test even when a guard caught the panic).
- Saves are atomic (`io::write_atomic`: temp file in the same directory, fsync, rename, fsync the directory). Per-user files (settings, recovery copies, the MCP session file) are owner-only: `Privacy::OwnerOnly`, `io::create_private_dir` (0600/0700 on Unix). New plan files are 0600; a replaced file keeps its mode.
- Speed gate: `crates/plancraft-core/tests/speed.rs` (200 tasks recalculate in < 10 ms in release). Keep it passing; don't loosen the bound to make a change fit.

## Behaviour parity (clean-room)
- Scheduling behaviour follows Microsoft's *published* documentation only (support.microsoft.com / learn.microsoft.com). `crates/plancraft-core/tests/ms_parity.rs` holds worked examples from those pages; every test cites its page and section (`// Source: <URL> — "<section>"`) and paraphrases (no copied text). A known difference is kept as a test marked `#[ignore = "parity gap: ..."]` until fixed. Never use Microsoft's code, files, binaries or product output.

## MCP security
- The HTTP server requires `Authorization: Bearer <token>` with a fresh random token per server start (`plancraft_mcp::session`), on top of the Origin/Host checks. The desktop app publishes URL + token in the owner-only `mcp-session.json`; `plancraft mcp --attach` relays stdio to the app using it (the recommended Claude Code setup). Never log the token or put it in a URL.


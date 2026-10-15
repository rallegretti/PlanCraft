# PlanCraft — notes for AI assistants

- Rust workspace: `plancraft-core` (logic), `plancraft-mcp` (MCP server), `plancraft-app` (egui desktop app, binary `plancraft`).
- Hard rule from the owner: everything in Rust; no calling into C/Python/other languages for features (scripts/adapters are fine when unavoidable). Check new deps with `cargo tree -p plancraft-app -i cc -e build` (should print nothing).
- Brand: deep blue accent (`theme::ACCENT` #1B3A8C), gold milestone diamond; the logo must never resemble a "P" letter.
- All project mutations go through `ops.rs` methods on `Project`, wrapped in `Document::edit`/`try_edit` (undo + recalculation). MCP edits use `ChangeSource::Agent`.
- Times are working minutes; finishes land on the end of the working day (17:00).
- egui 0.36: `App::ui(&mut self, ui, frame)`, `egui::Panel::top/bottom`, `Popup::menu`, `zoom_delta()` for Ctrl+wheel. Many emoji glyphs are missing from the default fonts; check new icon glyphs visually.
- Test: `cargo test --workspace`. Visual check: `PLANCRAFT_VIEW="Gantt Chart" PLANCRAFT_CMD=TaskInfo PLANCRAFT_SCREENSHOT=out.png cargo run -p plancraft-app -- --demo` writes a PNG and exits.
- Formatting: `cargo fmt --all` (rustfmt.toml: max_width 140).

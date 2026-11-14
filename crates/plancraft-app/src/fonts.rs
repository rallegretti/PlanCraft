//! Fonts for other scripts on screen: the system fonts the printer uses for characters the
//! bundled fonts lack (`plancraft_core::print::fallback`) are added to egui the first time some
//! text needs them: a plan's names and notes, typed or pasted text, recent file names.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{FontData, FontFamily, FontId};
use plancraft_core::prelude::*;
use plancraft_core::print::font;
use plancraft_core::workspace::Workspace;

/// How long to wait before looking again while the system fonts are still being found.
const NOT_READY_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Default)]
pub struct FontLoader {
    /// Characters already looked at: a bit per character of the Basic Multilingual Plane (cheap
    /// enough to run over a large plan's text after every edit), a set for the rest.
    checked_bmp: Vec<u64>,
    checked_other: HashSet<char>,
    /// Fallback faces added to egui.
    installed: HashSet<usize>,
    /// The revision of each plan when its text was last looked at.
    seen: HashMap<u64, u64>,
    /// Text that arrived before the system fonts were found (typed, pasted, file names).
    pending: Vec<String>,
}

impl FontLoader {
    /// Marks `ch` as looked at; `false` if it already was.
    fn first_look(&mut self, ch: char) -> bool {
        let c = u32::from(ch) as usize;
        if c < 0x1_0000 {
            if self.checked_bmp.is_empty() {
                self.checked_bmp = vec![0; 0x1_0000 / 64];
            }
            let Some(word) = self.checked_bmp.get_mut(c / 64) else { return false };
            let bit = 1u64 << (c % 64);
            let first = *word & bit == 0;
            *word |= bit;
            first
        } else {
            self.checked_other.insert(ch)
        }
    }

    /// Adds fonts for the characters of `text` that the fonts in use cannot draw. While the
    /// system fonts are still being found (a background thread at startup), the text waits, so
    /// the window never stalls on it.
    pub fn ensure(&mut self, ctx: &egui::Context, text: &str) {
        if text.is_ascii() {
            return;
        }
        if !font::ready() {
            if self.pending.iter().map(String::len).sum::<usize>() < 1 << 20 {
                self.pending.push(text.to_string());
            }
            ctx.request_repaint_after(NOT_READY_RETRY);
            return;
        }
        for ch in text.chars() {
            if ch.is_ascii() || !self.first_look(ch) {
                continue;
            }
            if font::invisible(ch) || ctx.fonts_mut(|f| f.has_glyph(&FontId::proportional(13.0), ch)) {
                continue;
            }
            let (i, gid) = font::pick(ch);
            if gid == 0 || i == 0 || !self.installed.insert(i) {
                continue;
            }
            let Some(face) = font::face(i) else { continue };
            let data = FontData { font: Cow::Borrowed(face.data), index: face.index, tweak: Default::default() };
            let families = [FontFamily::Proportional, FontFamily::Monospace]
                .into_iter()
                .map(|family| InsertFontFamily { family, priority: FontPriority::Lowest })
                .collect();
            ctx.add_font(FontInsert::new(&format!("system-fallback-{i}"), data, families));
        }
    }

    /// Looks at the text of plans that changed since the last frame, and at typed or pasted text.
    pub fn frame(&mut self, ctx: &egui::Context, ws: &Workspace) {
        if !font::ready() {
            // Plans are looked at once the fonts are found.
            ctx.request_repaint_after(NOT_READY_RETRY);
            return;
        }
        for t in std::mem::take(&mut self.pending) {
            self.ensure(ctx, &t);
        }
        for d in &ws.docs {
            if self.seen.insert(d.handle, d.revision) != Some(d.revision) {
                each_text(&d.project, &mut |s| self.ensure(ctx, s));
            }
        }
        let typed: Vec<String> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) | egui::Event::Paste(t) => Some(t.clone()),
                    egui::Event::Ime(egui::ImeEvent::Preedit { text: t, .. } | egui::ImeEvent::Commit(t)) => Some(t.clone()),
                    _ => None,
                })
                .collect()
        });
        for t in typed {
            self.ensure(ctx, &t);
        }
    }
}

/// Calls `f` with every piece of text in a plan that shows in a view or prints.
pub fn each_text(p: &Project, f: &mut impl FnMut(&str)) {
    for s in [&p.name, &p.author, &p.manager, &p.company, &p.notes, &p.settings.currency] {
        f(s);
    }
    for e in &p.time_entries {
        f(&e.note);
    }
    for x in &p.external_projects {
        f(&x.name);
        for t in &x.tasks {
            f(&t.name);
        }
    }
    for t in &p.tasks {
        f(&t.name);
        f(&t.notes);
        f(&t.deliverable);
        for v in t.custom.values() {
            if let plancraft_core::custom::CustomValue::Text(s) = v {
                f(s);
            }
        }
    }
    for r in &p.resources {
        for s in [&r.name, &r.initials, &r.group, &r.material_label, &r.notes, &r.email] {
            f(s);
        }
    }
    for c in &p.calendars {
        f(&c.name);
    }
    for c in &p.custom_fields {
        f(&c.name);
        for s in &c.choices {
            f(s);
        }
    }
    for c in &p.board_columns {
        f(&c.name);
    }
    for t in &p.tables {
        f(&t.name);
    }
    for s in p.page_setup.header.iter().chain(&p.page_setup.footer) {
        f(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What egui draws for `text`, left to right (only glyphs that take room).
    fn on_screen(ctx: &egui::Context, text: &str) -> String {
        let g = ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), FontId::proportional(13.0), egui::Color32::WHITE));
        let mut v: Vec<(f32, char)> =
            g.rows.iter().flat_map(|r| r.glyphs.iter().filter(|g| g.advance_width > 0.01).map(|g| (g.pos.x, g.chr))).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v.into_iter().map(|(_, c)| c).collect()
    }

    /// Hebrew on screen reads right to left with words in the right order (regression: the text
    /// was reversed before egui, which reverses right-to-left runs itself, so it came out
    /// backwards).
    #[test]
    fn hebrew_reads_right_to_left_on_screen() {
        let ctx = egui::Context::default();
        pass(&ctx);
        let mut loader = FontLoader::default();
        loader.ensure(&ctx, "שלום עולם");
        pass(&ctx);
        if !ctx.fonts_mut(|f| f.has_glyph(&FontId::proportional(13.0), 'ש')) {
            eprintln!("no Hebrew font on this system");
            return;
        }
        // Drawn left to right, the last letter of the last word comes first.
        let shown = |s: &str| on_screen(&ctx, &plancraft_core::bidi::display(s));
        assert_eq!(shown("שלום"), "םולש");
        assert_eq!(shown("שלום עולם"), "םלוע םולש");
        assert_eq!(shown("תכנון PlanCraft"), "PlanCraft ןונכת");
        assert_eq!(shown("Review שלום now"), "Review םולש now");
    }

    /// Runs a frame; finds the system fonts first, as the background warm-up does in the app.
    fn pass(ctx: &egui::Context) {
        let _ = plancraft_core::print::fallback::fallbacks();
        let mut out = ctx.run_ui(Default::default(), |_| {});
        out.textures_delta.clear();
    }

    #[test]
    fn text_in_other_scripts_loads_a_system_font() {
        let ctx = egui::Context::default();
        pass(&ctx);
        let mut loader = FontLoader::default();
        loader.ensure(&ctx, "Design review, Запуск");
        assert!(loader.installed.is_empty(), "Latin and Cyrillic are bundled");
        if font::pick('設').1 == 0 {
            eprintln!("no CJK font on this system");
            return;
        }
        assert!(!ctx.fonts_mut(|f| f.has_glyph(&FontId::proportional(13.0), '設')));
        loader.ensure(&ctx, "設計レビュー");
        assert_eq!(loader.installed.len(), 1, "one font for all of it");
        pass(&ctx);
        assert!(ctx.fonts_mut(|f| f.has_glyph(&FontId::proportional(13.0), '設')));
        // A plan's text is looked at once per revision.
        let mut p = Project::new("計画", chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        p.add_task("תכנון", None);
        let mut seen = Vec::new();
        each_text(&p, &mut |s| seen.push(s.to_string()));
        assert!(seen.iter().any(|s| s == "תכנון") && seen.iter().any(|s| s == "計画"));
    }
}

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

#[derive(Default)]
pub struct FontLoader {
    /// Characters already looked at.
    checked: HashSet<char>,
    /// Fallback faces added to egui.
    installed: HashSet<usize>,
    /// The revision of each plan when its text was last looked at.
    seen: HashMap<u64, u64>,
}

impl FontLoader {
    /// Adds fonts for the characters of `text` that the fonts in use cannot draw.
    pub fn ensure(&mut self, ctx: &egui::Context, text: &str) {
        for ch in text.chars() {
            if ch.is_ascii() || !self.checked.insert(ch) {
                continue;
            }
            if ch.is_control() || ch.is_whitespace() || ctx.fonts_mut(|f| f.has_glyph(&FontId::proportional(13.0), ch)) {
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
    for s in [&p.name, &p.author, &p.manager, &p.company, &p.notes] {
        f(s);
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
        for s in [&r.name, &r.initials, &r.group, &r.material_label, &r.notes] {
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

    fn pass(ctx: &egui::Context) {
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

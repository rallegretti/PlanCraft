//! The Print window: page setup on the left, a live preview of the printed pages on the right,
//! and Print / Save as PDF. The preview draws the very pages that print.

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use chrono::NaiveDate;
use egui::{Align2, Color32, Context, FontId, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};
use plancraft_core::print::{self, Item, LegendPlacement, Orientation, PageSetup, Paper, TimescaleFit, font};
use plancraft_core::workspace::Document;

use crate::edit::EditExt;
use crate::printing::{self, PrintView};
use crate::state::UiState;
use crate::theme;

/// A PDF waiting to be saved (the file dialog opens after the workspace lock is released).
pub struct PendingPdf {
    pub bytes: Vec<u8>,
    pub name: String,
}

pub struct PrintDialog {
    view: PrintView,
    setup: PageSetup,
    from: String,
    to: String,
    page: usize,
    printers: Vec<String>,
    printers_rx: Option<Receiver<Vec<String>>>,
    printer: String,
    copies: u8,
    sending: Option<Receiver<Result<String, String>>>,
    status: Option<(bool, String)>,
    cache: Option<(String, Arc<print::Document>)>,
    /// Set by "Save as PDF…"; taken by the app.
    pub pending_pdf: Option<PendingPdf>,
}

impl PrintDialog {
    pub fn new(doc: &Document, st: &UiState) -> Self {
        Self {
            view: PrintView::for_view(st.view),
            setup: doc.project.page_setup.clone(),
            from: String::new(),
            to: String::new(),
            page: 0,
            printers: Vec::new(),
            printers_rx: printing::HAS_PRINT_SERVICE.then(printing::list_printers),
            printer: String::new(),
            copies: 1,
            sending: None,
            status: None,
            cache: None,
            pending_pdf: None,
        }
    }

    /// The dates to print, or why they are wrong.
    fn range(&self) -> Result<(Option<NaiveDate>, Option<NaiveDate>), String> {
        let parse = |s: &str, what: &str| -> Result<Option<NaiveDate>, String> {
            let s = s.trim();
            if s.is_empty() {
                return Ok(None);
            }
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| format!("{what}: enter a date like 2026-11-30"))?;
            let year = chrono::Datelike::year(&d);
            if !(1900..=2300).contains(&year) {
                return Err(format!("{what}: pick a year between 1900 and 2300"));
            }
            Ok(Some(d))
        };
        let (a, b) = (parse(&self.from, "From")?, parse(&self.to, "To")?);
        if let (Some(a), Some(b)) = (a, b)
            && b < a
        {
            return Err("The To date is before the From date".into());
        }
        Ok((a, b))
    }

    /// The printed pages, laid out again only when something they depend on changed.
    fn pages(&mut self, doc: &Document, st: &UiState) -> Arc<print::Document> {
        let (from, to) = self.range().unwrap_or((None, None));
        let g = &st.gantt;
        let key = format!(
            "{}|{}|{:?}|{:?}|{from:?}|{to:?}|{:?}|{:?}|{:?}|{}|{:?}|{}|{}|{}|{:?}",
            doc.handle,
            doc.revision,
            self.view,
            self.setup,
            st.sort,
            st.group,
            st.filter,
            st.search,
            st.table,
            st.collapsed.len(),
            st.collapsed_groups.len(),
            g.px_per_day,
            (g.show_critical, g.show_baseline, g.show_progress, g.show_links, g.show_slack, g.show_resource_names),
        );
        if let Some((k, d)) = &self.cache
            && *k == key
        {
            return d.clone();
        }
        let d = Arc::new(printing::document(&doc.project, st, self.view, &self.setup, from, to));
        self.cache = Some((key, d.clone()));
        d
    }

    /// Saves changed page setup with the plan (one undoable edit).
    fn commit_setup(&mut self, doc: &mut Document) {
        let mut s = self.setup.clone();
        s.sanitize();
        if s != doc.project.page_setup {
            let _ = doc.edit_msg("Page Setup", move |p| {
                p.set_page_setup(s);
                Ok(())
            });
        }
    }

    pub fn show(&mut self, ctx: &Context, doc: &mut Document, st: &UiState) -> bool {
        // Background work: printer list and print jobs.
        if let Some(rx) = &self.printers_rx
            && let Ok(list) = rx.try_recv()
        {
            if self.printer.is_empty() {
                self.printer = list.first().cloned().unwrap_or_default();
            }
            self.printers = list;
            self.printers_rx = None;
        }
        if let Some(rx) = &self.sending {
            match rx.try_recv() {
                Ok(r) => {
                    self.status = Some(match r {
                        Ok(m) => (true, m),
                        Err(e) => (false, e),
                    });
                    self.sending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.sending = None,
            }
        }

        let pages = self.pages(doc, st);
        let mut open = true;
        let mut close = false;
        let mut print_now = false;
        let mut save_pdf = false;
        egui::Window::new("Print")
            .id(egui::Id::new("print_dialog"))
            .default_size([1080.0, 720.0])
            .min_size([760.0, 520.0])
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .resizable(true)
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let h = ui.available_height().max(480.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(290.0);
                        egui::ScrollArea::vertical().max_height(h).show(ui, |ui| {
                            self.settings(ui, &pages, &mut print_now, &mut save_pdf, &mut close);
                        });
                    });
                    ui.separator();
                    ui.vertical(|ui| self.preview(ui, &pages, h));
                });
            });

        let range_ok = self.range().is_ok();
        if (print_now || save_pdf) && range_ok {
            self.commit_setup(doc);
            let bytes = print::pdf::write(&pages);
            if save_pdf {
                self.pending_pdf = Some(PendingPdf { bytes, name: format!("{} - {}.pdf", doc.project.name, self.view.label()) });
            } else {
                self.status = None;
                let printer = Some(self.printer.clone()).filter(|p| !p.is_empty());
                self.sending = Some(printing::send(bytes, printer, self.copies, pages.title.clone()));
            }
        }
        if !open || close {
            self.commit_setup(doc);
            return false;
        }
        true
    }

    fn settings(&mut self, ui: &mut Ui, pages: &print::Document, print_now: &mut bool, save_pdf: &mut bool, close: &mut bool) {
        let weak = theme::pal().TEXT_WEAK;
        let heading = |ui: &mut Ui, t: &str| {
            ui.add_space(6.0);
            ui.label(RichText::new(t).strong());
        };
        egui::Grid::new("print_main").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("View");
            egui::ComboBox::from_id_salt("print_view").width(170.0).selected_text(self.view.label()).show_ui(ui, |ui| {
                for v in PrintView::ALL {
                    if ui.selectable_value(&mut self.view, v, v.label()).clicked() {
                        self.page = 0;
                    }
                }
            });
            ui.end_row();
            ui.label("From");
            ui.add(egui::TextEdit::singleline(&mut self.from).hint_text("project start").desired_width(170.0));
            ui.end_row();
            ui.label("To");
            ui.add(egui::TextEdit::singleline(&mut self.to).hint_text("project finish").desired_width(170.0));
            ui.end_row();
            if printing::HAS_PRINT_SERVICE {
                ui.label("Printer");
                let shown = if self.printer.is_empty() { "Default printer".to_string() } else { self.printer.clone() };
                egui::ComboBox::from_id_salt("print_printer").width(170.0).selected_text(shown).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.printer, String::new(), "Default printer");
                    for p in &self.printers {
                        ui.selectable_value(&mut self.printer, p.clone(), p);
                    }
                });
                ui.end_row();
                ui.label("Copies");
                ui.add(egui::DragValue::new(&mut self.copies).range(1..=99));
                ui.end_row();
            }
        });
        if let Err(e) = self.range() {
            ui.colored_label(theme::pal().ERROR, e);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let busy = self.sending.is_some();
            let label = if printing::HAS_PRINT_SERVICE { "Print" } else { "Print…" };
            let hint = if printing::HAS_PRINT_SERVICE {
                "Send these pages to the printer"
            } else {
                "Open these pages in your PDF viewer to print them"
            };
            if ui
                .add_enabled(!busy && !pages.pages.is_empty(), egui::Button::new(RichText::new(label).strong()))
                .on_hover_text(hint)
                .clicked()
            {
                *print_now = true;
            }
            if ui.add_enabled(!pages.pages.is_empty(), egui::Button::new("Save as PDF…")).clicked() {
                *save_pdf = true;
            }
            if ui.button("Close").clicked() {
                *close = true;
            }
        });
        if self.sending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Sending to the printer…");
            });
        }
        if let Some((ok, msg)) = &self.status {
            ui.colored_label(if *ok { theme::pal().OK } else { theme::pal().ERROR }, msg);
        }

        ui.separator();
        heading(ui, "Page Setup");
        let s = &mut self.setup;
        egui::Grid::new("print_setup").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Paper");
            egui::ComboBox::from_id_salt("print_paper").width(170.0).selected_text(s.paper.label()).show_ui(ui, |ui| {
                for p in Paper::ALL {
                    ui.selectable_value(&mut s.paper, p, p.label());
                }
            });
            ui.end_row();
            ui.label("Orientation");
            ui.horizontal(|ui| {
                ui.radio_value(&mut s.orientation, Orientation::Portrait, "Portrait");
                ui.radio_value(&mut s.orientation, Orientation::Landscape, "Landscape");
            });
            ui.end_row();
            ui.label("Margins");
            ui.add(egui::DragValue::new(&mut s.margin_mm).range(0.0..=print::MAX_MARGIN_MM).speed(0.5).suffix(" mm"));
            ui.end_row();
            ui.label("Scale");
            ui.add(egui::DragValue::new(&mut s.scale_percent).range(print::MIN_SCALE..=print::MAX_SCALE).suffix(" %"));
            ui.end_row();
            ui.label("Legend");
            egui::ComboBox::from_id_salt("print_legend").width(170.0).selected_text(s.legend.label()).show_ui(ui, |ui| {
                for l in LegendPlacement::ALL {
                    ui.selectable_value(&mut s.legend, l, l.label());
                }
            });
            ui.end_row();
        });
        heading(ui, "Timescale");
        let mut wide = match s.timescale {
            TimescaleFit::PagesWide(n) => n,
            TimescaleFit::AsShown => 0,
        };
        ui.horizontal(|ui| {
            if ui.radio(wide > 0, "Fit to").clicked() && wide == 0 {
                wide = 1;
            }
            let mut n = wide.max(1);
            if ui.add_enabled(wide > 0, egui::DragValue::new(&mut n).range(1..=print::MAX_PAGES_WIDE)).changed() {
                wide = n;
            }
            ui.label(if wide == 1 { "page wide" } else { "pages wide" });
        });
        if ui.radio(wide == 0, "As zoomed on screen").clicked() {
            wide = 0;
        }
        s.timescale = if wide == 0 { TimescaleFit::AsShown } else { TimescaleFit::PagesWide(wide) };
        heading(ui, "Columns and pages");
        ui.horizontal(|ui| {
            ui.label("Repeat the first");
            ui.add(egui::DragValue::new(&mut s.repeat_columns).range(0..=20));
            ui.label("columns on every page");
        });
        ui.checkbox(&mut s.print_all_columns, "Print all table columns")
            .on_hover_text("Otherwise only the columns left of the divider print, as on screen");
        ui.checkbox(&mut s.print_blank_pages, "Print pages with no bars on them");
        heading(ui, "Header and footer");
        for (label, texts) in [("Header", &mut s.header), ("Footer", &mut s.footer)] {
            ui.label(RichText::new(label).color(weak));
            for (t, hint) in texts.iter_mut().zip(["Left", "Centre", "Right"]) {
                ui.add(egui::TextEdit::singleline(t).hint_text(hint).desired_width(f32::INFINITY));
            }
        }
        ui.label(RichText::new("Codes: [Project] [Manager] [Company] [View] [Date] [Page] [Pages] [Start] [Finish]").small().color(weak));
        ui.add_space(4.0);
        if ui.button("Reset Page Setup").clicked() {
            *s = PageSetup::default();
        }
        s.sanitize();
    }

    fn preview(&mut self, ui: &mut Ui, pages: &print::Document, height: f32) {
        let n = pages.pages.len();
        self.page = self.page.min(n.saturating_sub(1));
        ui.horizontal(|ui| {
            if ui.add_enabled(self.page > 0, egui::Button::new("Previous")).clicked() {
                self.page = self.page.saturating_sub(1);
            }
            ui.label(format!("Page {} of {n}", (self.page + 1).min(n)));
            if ui.add_enabled(self.page + 1 < n, egui::Button::new("Next")).clicked() {
                self.page += 1;
            }
            if pages.truncated {
                ui.colored_label(theme::pal().WARN, format!("Only the first {} pages print", print::layout::MAX_PAGES));
            }
        });
        let size = vec2(ui.available_width().max(200.0), (height - 40.0).max(200.0));
        let (area, _) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, theme::pal().PANEL_BG);
        let Some(page) = pages.pages.get(self.page) else {
            painter.text(area.center(), Align2::CENTER_CENTER, "Nothing to print", FontId::proportional(14.0), theme::pal().TEXT_WEAK);
            return;
        };
        let k = ((area.width() - 24.0) / page.width).min((area.height() - 24.0) / page.height).max(0.05);
        let paper = Rect::from_center_size(area.center(), vec2(page.width * k, page.height * k));
        painter.rect_filled(paper.translate(vec2(3.0, 3.0)), 0.0, theme::pal().GRID_LINE_STRONG);
        paint_page(&painter, paper, page, k);
    }
}

fn color(c: print::Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// Draws a printed page at `paper` (k screen pixels per point). Paper is white in light and
/// dark mode, so the page uses the print palette, not the screen theme.
pub fn paint_page(painter: &Painter, paper: Rect, page: &print::Page, k: f32) {
    painter.rect_filled(paper, 0.0, color(print::layout::Palette::default().white));
    let at = |p: print::Pt| -> Pos2 { pos2(paper.left() + p.x * k, paper.top() + p.y * k) };
    let painter = painter.with_clip_rect(paper.intersect(painter.clip_rect()));
    paint_items(&painter, &page.items, &at, k);
}

fn paint_items(painter: &Painter, items: &[Item], at: &dyn Fn(print::Pt) -> Pos2, k: f32) {
    let rect = |r: &print::Rect| Rect::from_min_size(at(print::pt(r.x, r.y)), vec2(r.w * k, r.h * k));
    let ascent = font::metrics().ascent;
    for item in items {
        match item {
            Item::Rect { rect: r, fill, stroke } => {
                let r = rect(r);
                if let Some(f) = fill {
                    painter.rect_filled(r, 0.0, color(*f));
                }
                if let Some(s) = stroke {
                    painter.rect_stroke(r, 0.0, Stroke::new((s.width * k).max(0.5), color(s.color)), egui::StrokeKind::Middle);
                }
            }
            Item::Line { points, stroke } => {
                let pts: Vec<Pos2> = points.iter().map(|p| at(*p)).collect();
                let st = Stroke::new((stroke.width * k).max(0.5), color(stroke.color));
                match stroke.dash {
                    Some(d) => {
                        painter.extend(Shape::dashed_line(&pts, st, d * k, d * k));
                    }
                    None => {
                        painter.add(Shape::line(pts, st));
                    }
                }
            }
            Item::Polygon { points, fill } => {
                painter.add(Shape::convex_polygon(points.iter().map(|p| at(*p)).collect(), color(*fill), Stroke::NONE));
            }
            Item::Text { at: p, size, color: c, bold, text } => {
                let px = size * k;
                if px < 2.0 {
                    // Too small to read: a faint line stands in for the text.
                    let w = font::width(text, *size, *bold) * k;
                    let y = at(*p).y - px / 3.0;
                    painter.hline(at(*p).x..=at(*p).x + w, y, Stroke::new(px.max(0.4) / 2.0, color(*c).gamma_multiply(0.5)));
                    continue;
                }
                let top = at(print::pt(p.x, p.y - ascent * size));
                let font_id = FontId::proportional(px);
                painter.text(top, Align2::LEFT_TOP, text, font_id.clone(), color(*c));
                if *bold {
                    painter.text(top + vec2((px * 0.04).max(0.4), 0.0), Align2::LEFT_TOP, text, font_id, color(*c));
                }
            }
            Item::Clip { rect: r, items } => {
                let clip = rect(r).intersect(painter.clip_rect());
                paint_items(&painter.with_clip_rect(clip), items, at, k);
            }
        }
    }
}

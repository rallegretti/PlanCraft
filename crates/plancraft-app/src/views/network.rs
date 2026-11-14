//! Network Diagram: one box per task, laid out automatically (see `plancraft_core::network`),
//! with lines for the links. Click selects, double-click opens Task Information, dragging from a
//! box to another links them, dragging the background or scrolling moves the view, and
//! Ctrl+wheel zooms.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, TextFormat, Ui, pos2, vec2};
use plancraft_core::network::{self, BoxRect, Diagram, NetLinkKind};
use plancraft_core::prelude::*;
use plancraft_core::workspace::Document;

use crate::commands::Command;
use crate::edit::EditExt;
use crate::format;
use crate::state::{NetDrag, ToastKind, UiState};
use crate::theme;

pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 2.5;

/// What the layout depends on besides the plan: a change of any of these lays it out again.
fn cache_key(doc: &Document, st: &UiState) -> u64 {
    let net = &st.network;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (doc.handle, doc.revision, st.filter, &st.search, net.opts, net.show_summaries).hash(&mut h);
    // Order-independent hash of the collapsed summaries.
    let collapsed = st.collapsed.iter().fold(0u64, |acc, id| {
        let mut ih = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut ih);
        acc ^ ih.finish()
    });
    collapsed.hash(&mut h);
    h.finish()
}

/// Lays out the tasks of the outline the task views show (filter, search, collapsed summaries;
/// grouping does not apply here), or every subtask passing the filter when summaries are hidden.
fn diagram(doc: &Document, st: &UiState) -> Arc<Diagram> {
    Arc::new(layout(&doc.project, st))
}

/// The diagram the view shows for `p` (also what prints).
pub fn layout(p: &Project, st: &UiState) -> Diagram {
    let boxes = if st.network.show_summaries {
        let mut shown = vec![false; p.tasks.len()];
        for i in super::outline_rows(p, st).iter().filter_map(super::Row::task) {
            if let Some(s) = shown.get_mut(i) {
                *s = true;
            }
        }
        network::boxes_for(p, &shown, &st.collapsed)
    } else {
        p.tasks.iter().enumerate().map(|(i, t)| (!t.sched.is_summary && super::task_matches(p, st, t)).then_some(i)).collect()
    };
    network::layout(p, &boxes, st.network.opts)
}

/// One line of text cut to `width` with an ellipsis.
fn line(ui: &Ui, text: &str, size: f32, color: Color32, width: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.append(&plancraft_core::bidi::display(text), 0.0, TextFormat { font_id: FontId::proportional(size), color, ..Default::default() });
    job.wrap = TextWrapping::truncate_at_width(width.max(1.0));
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Arrowhead at the end of a line (lines always end going right, into a box's left side).
fn arrow(painter: &egui::Painter, pts: &[Pos2], z: f32, color: Color32) {
    if let Some(&tip) = pts.last() {
        let s = (5.0 * z).clamp(3.0, 7.0);
        painter.add(Shape::convex_polygon(
            vec![tip, pos2(tip.x - s * 1.4, tip.y - s * 0.8), pos2(tip.x - s * 1.4, tip.y + s * 0.8)],
            color,
            Stroke::NONE,
        ));
    }
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, out: &mut Vec<Command>) {
    let key = cache_key(doc, st);
    let d = match &st.network.cache {
        Some((k, d)) if *k == key => d.clone(),
        _ => diagram(doc, st),
    };
    st.network.cache = Some((key, d.clone()));
    if st.network.shown_for != Some(doc.handle) {
        st.network.shown_for = Some(doc.handle);
        st.network.offset = Pos2::ZERO;
        st.network.reveal_pending = true;
    }
    let p = &doc.project;
    let net = &mut st.network;
    net.view_rect = rect;

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::pal().BG);
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());

    // Fit the whole diagram, or bring the selected task into view.
    let (dw, dh) = d.size();
    if net.fit_pending {
        net.fit_pending = false;
        net.zoom = (rect.width() / dw).min(rect.height() / dh).clamp(MIN_ZOOM, 1.0);
        // Centred when it fits.
        let (vw, vh) = (rect.width() / net.zoom, rect.height() / net.zoom);
        net.offset = pos2(((dw - vw) / 2.0).min(0.0), ((dh - vh) / 2.0).min(0.0));
    }
    if net.fit_selected_pending {
        net.fit_selected_pending = false;
        let rects: Vec<BoxRect> = p
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| st.sel.tasks.contains(&t.id))
            .filter_map(|(i, _)| d.node_of(i))
            .filter_map(|n| d.rect(n))
            .collect();
        let (x0, y0) = rects.iter().fold((f32::MAX, f32::MAX), |(x, y), r| (x.min(r.x), y.min(r.y)));
        let (x1, y1) = rects.iter().fold((f32::MIN, f32::MIN), |(x, y), r| (x.max(r.right()), y.max(r.bottom())));
        if !rects.is_empty() {
            let m = d.geometry.margin;
            let (w, h) = (x1 - x0 + 2.0 * m, y1 - y0 + 2.0 * m);
            net.zoom = (rect.width() / w).min(rect.height() / h).clamp(MIN_ZOOM, 1.0);
            let (vw, vh) = (rect.width() / net.zoom, rect.height() / net.zoom);
            net.offset = pos2((x0 + x1) / 2.0 - vw / 2.0, (y0 + y1) / 2.0 - vh / 2.0);
        }
    }
    if net.reveal_pending {
        net.reveal_pending = false;
        if let Some(r) = st.sel.single_task().and_then(|id| p.task_index(id)).and_then(|i| d.node_of(i)).and_then(|n| d.rect(n)) {
            let (vw, vh) = (rect.width() / net.zoom, rect.height() / net.zoom);
            let inside = r.x >= net.offset.x && r.y >= net.offset.y && r.right() <= net.offset.x + vw && r.bottom() <= net.offset.y + vh;
            if !inside {
                net.offset = pos2(r.x + r.w / 2.0 - vw / 2.0, r.y + r.h / 2.0 - vh / 2.0);
            }
        }
    }

    // Scrolling and zooming.
    if ui.rect_contains_pointer(rect) {
        let (delta, cmd) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.command));
        let zoom = ui.input(|i| i.zoom_delta());
        if cmd && (zoom - 1.0).abs() > 1e-4 {
            if let Some(at) = ui.input(|i| i.pointer.hover_pos()) {
                let before = net.offset + (at - rect.min) / net.zoom;
                net.zoom = (net.zoom * zoom).clamp(MIN_ZOOM, MAX_ZOOM);
                net.offset = before - (at - rect.min) / net.zoom;
            }
        } else if !cmd {
            net.offset -= delta / net.zoom;
        }
    }
    // Keep some of the diagram in view.
    let (vw, vh) = (rect.width() / net.zoom, rect.height() / net.zoom);
    net.offset.x = net.offset.x.clamp(-vw * 0.5, (dw - vw * 0.5).max(0.0));
    net.offset.y = net.offset.y.clamp(-vh * 0.5, (dh - vh * 0.5).max(0.0));
    if !net.offset.x.is_finite() || !net.offset.y.is_finite() {
        net.offset = Pos2::ZERO;
    }

    let z = net.zoom;
    let origin = rect.min;
    let off = net.offset;
    let to_screen = move |x: f32, y: f32| pos2(origin.x + (x - off.x) * z, origin.y + (y - off.y) * z);
    let screen_rect = |r: BoxRect| Rect::from_min_max(to_screen(r.x, r.y), to_screen(r.right(), r.bottom()));
    let to_diagram = |pos: Pos2| (off.x + (pos.x - origin.x) / z, off.y + (pos.y - origin.y) / z);
    let hovered = resp.hover_pos().and_then(|pos| {
        let (x, y) = to_diagram(pos);
        d.node_at(x, y)
    });

    // Links.
    let g = d.geometry;
    for l in &d.links {
        let (Some(a), Some(b)) = (d.rect(l.from), d.rect(l.to)) else { continue };
        let span = Rect::from_min_max(
            to_screen(a.x.min(b.x), a.y.min(b.y) - g.row_gap),
            to_screen(a.right().max(b.right()), a.bottom().max(b.bottom())),
        );
        if !span.intersects(rect) {
            continue;
        }
        let pts: Vec<Pos2> = d.route(l).into_iter().map(|(x, y)| to_screen(x, y)).collect();
        match l.kind {
            NetLinkKind::Outline => {
                painter.extend(Shape::dashed_line(&pts, Stroke::new(1.0, theme::pal().GRID_LINE_STRONG), 4.0, 3.0));
            }
            NetLinkKind::Member => {
                let color = theme::pal().MEMBER_LINK;
                painter.extend(Shape::dashed_line(&pts, Stroke::new(1.2, color), 5.0, 3.0));
                arrow(&painter, &pts, z, color);
            }
            NetLinkKind::Dependency { kind, lag } => {
                let color = if l.critical { theme::pal().BAR_CRITICAL } else { theme::pal().LINK };
                let width = if l.critical { 1.6 } else { 1.0 };
                painter.add(Shape::line(pts.clone(), Stroke::new(width, color)));
                arrow(&painter, &pts, z, color);
                if net.link_labels && z >= 0.45 && !l.merged {
                    let label = format!("{}{}", kind.code(), lag.format(&p.settings.time));
                    if let [.., a, b] = pts.as_slice() {
                        let at = pos2((a.x + b.x) / 2.0, a.y.min(b.y) - 2.0);
                        painter.text(at, Align2::CENTER_BOTTOM, label, FontId::proportional(10.0 * z.min(1.2)), theme::pal().TEXT_WEAK);
                    }
                }
            }
        }
    }

    // Boxes.
    let fs = 11.0 * z;
    let ids_only = net.opts.ids_only || fs < 5.5;
    let pad = 5.0 * z;
    for (k, node) in d.nodes.iter().enumerate() {
        let Some(br) = d.rect(k) else { continue };
        let r = screen_rect(br);
        if !r.intersects(rect) {
            continue;
        }
        let Some(t) = p.tasks.get(node.task) else { continue };
        let selected = st.sel.tasks.contains(&t.id);
        let critical = t.sched.critical;
        let (fill, edge, edge_w) = if selected {
            (theme::pal().SELECTION, theme::pal().ACCENT, 2.0)
        } else if critical {
            (theme::pal().PANEL_BG, theme::pal().BAR_CRITICAL, 1.8)
        } else {
            (theme::pal().PANEL_BG, theme::pal().BAR_TASK_EDGE, 1.0)
        };
        let shape_pts = if t.is_milestone() {
            // Pointed ends mark milestones.
            let c = (r.height() / 2.0).min(10.0 * z);
            vec![
                pos2(r.left() + c, r.top()),
                pos2(r.right() - c, r.top()),
                pos2(r.right(), r.center().y),
                pos2(r.right() - c, r.bottom()),
                pos2(r.left() + c, r.bottom()),
                pos2(r.left(), r.center().y),
            ]
        } else {
            Vec::new()
        };
        if shape_pts.is_empty() {
            painter.rect_filled(r, 3.0 * z, fill);
            painter.rect_stroke(r, 3.0 * z, Stroke::new(edge_w, edge), StrokeKind::Inside);
        } else {
            painter.add(Shape::convex_polygon(shape_pts, fill, Stroke::new(edge_w, edge)));
        }
        if t.sched.is_summary {
            // A band along the top marks summaries.
            let band = Rect::from_min_size(r.min, vec2(r.width(), (4.0 * z).max(2.0)));
            painter.rect_filled(band, 0.0, theme::pal().BAR_SUMMARY);
        }
        let text_color = if t.active { theme::pal().TEXT } else { theme::pal().TEXT_WEAK };
        let weak = theme::pal().TEXT_WEAK;
        let id_text = (node.task + 1).to_string();
        if ids_only {
            if fs >= 4.0 || net.opts.ids_only {
                painter.text(r.center(), Align2::CENTER_CENTER, &id_text, FontId::proportional((11.0 * z).max(4.0)), text_color);
            }
        } else {
            let inner = r.width() - 2.0 * pad;
            let row_h = (r.height() - 2.0 * pad) / 4.0;
            let y0 = r.top() + pad;
            // Faux bold name: the bundled UI font has no bold face.
            let name = line(ui, &t.name, fs * 1.05, text_color, inner);
            painter.galley(pos2(r.left() + pad + 0.55, y0), name.clone(), text_color);
            painter.galley(pos2(r.left() + pad, y0), name, text_color);
            // Dates on the left, ID and duration on the right.
            let (left_w, right_x) = (inner * 0.6, inner * 0.62);
            let cells: [(&str, String, &str, String); 2] = [
                ("Start", format::date(t.sched.start), "ID", id_text.clone()),
                ("Finish", format::date(t.sched.finish), "Dur", p.format_duration(t.sched.duration, t.estimated)),
            ];
            for (row, (l1, v1, l2, v2)) in cells.iter().enumerate() {
                let y = y0 + row_h * (row as f32 + 1.0);
                painter.galley(pos2(r.left() + pad, y), line(ui, &format!("{l1}: {v1}"), fs, text_color, left_w), text_color);
                painter.galley(
                    pos2(r.left() + pad + right_x, y),
                    line(ui, &format!("{l2}: {v2}"), fs, text_color, inner - right_x),
                    text_color,
                );
            }
            let res = p.format_resource_names(t.id);
            if !res.is_empty() {
                painter.galley(pos2(r.left() + pad, y0 + row_h * 3.0), line(ui, &format!("Res: {res}"), fs, weak, inner), weak);
            }
        }
        // One diagonal through a task in progress, two through a complete one.
        if net.progress_marks && !t.sched.is_summary {
            let mark = Stroke::new(1.0, weak);
            if t.percent_complete >= 100 {
                painter.line_segment([r.left_top(), r.right_bottom()], mark);
                painter.line_segment([r.left_bottom(), r.right_top()], mark);
            } else if t.percent_complete > 0 {
                painter.line_segment([r.left_bottom(), r.right_top()], mark);
            }
        }
        if hovered == Some(k) && net.drag.is_none() {
            painter.rect_stroke(r.expand(1.0), 3.0 * z, Stroke::new(1.0, theme::pal().ACCENT_MID), StrokeKind::Outside);
        }
    }
    let net = &mut st.network;

    // Hover tip with the full name.
    if let (Some(k), Some(pos)) = (hovered, resp.hover_pos())
        && net.drag.is_none()
        && let Some(t) = d.nodes.get(k).and_then(|n| p.tasks.get(n.task))
    {
        let tip = format!("{}\n{} – {}", t.name, format::date(t.sched.start), format::date(t.sched.finish));
        egui::Area::new(ui.id().with("net_tip")).order(egui::Order::Tooltip).fixed_pos(pos + vec2(14.0, 14.0)).interactable(false).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new(tip).size(12.0));
                });
            },
        );
    }

    // Dragging: from a box draws a link, from the background moves the view.
    let task_of = |k: usize| d.nodes.get(k).and_then(|n| p.tasks.get(n.task)).map(|t| t.id);
    // Where the button went down: a drag only starts once the pointer has moved a little.
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        let (x, y) = to_diagram(pos);
        net.drag = Some(match d.node_at(x, y).and_then(task_of) {
            Some(from) => NetDrag::Link { from },
            None => NetDrag::Pan { grab: pos2(x, y) },
        });
    }
    let mut link: Option<(TaskId, TaskId)> = None;
    if let (Some(drag), Some(pos)) = (net.drag.clone(), ui.input(|i| i.pointer.interact_pos())) {
        match drag {
            NetDrag::Pan { grab } => {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                net.offset = grab - (pos - origin) / z;
            }
            NetDrag::Link { from } => {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                if let Some(a) = p.task_index(from).and_then(|i| d.node_of(i)).and_then(|n| d.rect(n)) {
                    let start = to_screen(a.right(), a.mid_y());
                    painter.line_segment([start, pos], Stroke::new(1.5, theme::pal().ACCENT));
                }
                let (x, y) = to_diagram(pos);
                if let Some(to) = d.node_at(x, y).and_then(task_of).filter(|to| *to != from)
                    && let Some(b) = p.task_index(to).and_then(|i| d.node_of(i)).and_then(|n| d.rect(n))
                {
                    painter.rect_stroke(screen_rect(b).expand(2.0), 3.0 * z, Stroke::new(2.0, theme::pal().ACCENT), StrokeKind::Outside);
                    if resp.drag_stopped() {
                        link = Some((from, to));
                    }
                }
            }
        }
        if resp.drag_stopped() {
            net.drag = None;
        }
    }
    if !ui.input(|i| i.pointer.any_down()) && !resp.drag_stopped() {
        net.drag = None;
    }

    // Clicks select; a double-click opens Task Information; a right-click opens the task menu.
    let clicked = hovered.and_then(task_of);
    let mods = ui.input(|i| i.modifiers);
    if resp.clicked() || resp.secondary_clicked() || resp.double_clicked() {
        match clicked {
            Some(id) if mods.command && resp.clicked() => {
                if !st.sel.tasks.remove(&id) {
                    st.sel.tasks.insert(id);
                    st.sel.anchor = Some(id);
                }
            }
            Some(id) if mods.shift && resp.clicked() => {
                st.sel.tasks.insert(id);
                st.sel.anchor = Some(id);
            }
            Some(id) if resp.secondary_clicked() && st.sel.tasks.contains(&id) => {}
            Some(id) => st.sel.select_task(id),
            None if !mods.command && !mods.shift => {
                st.sel.tasks.clear();
                st.sel.anchor = None;
            }
            None => {}
        }
    }
    if resp.double_clicked() && clicked.is_some() {
        out.push(Command::TaskInfo);
    }
    if resp.secondary_clicked()
        && clicked.is_some()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        ui.ctx().data_mut(|dm| dm.insert_temp(egui::Id::new("task_ctx_menu"), pos));
    }
    if d.nodes.is_empty() {
        painter.text(rect.center(), Align2::CENTER_CENTER, "No tasks to show", FontId::proportional(14.0), theme::pal().TEXT_WEAK);
    }

    // Keys, once the diagram has been clicked: Delete, Insert, and Enter for Task Information.
    if resp.clicked() || resp.secondary_clicked() || resp.drag_started() {
        resp.request_focus();
    }
    if resp.has_focus() && !ui.ctx().egui_wants_keyboard_input() {
        let (del, ins, enter) =
            ui.input(|i| (i.key_pressed(egui::Key::Delete), i.key_pressed(egui::Key::Insert), i.key_pressed(egui::Key::Enter)));
        if del && !st.sel.tasks.is_empty() {
            out.push(Command::DeleteSelection);
        }
        if ins {
            out.push(Command::InsertTask);
        }
        if enter && st.sel.single_task().is_some() {
            out.push(Command::TaskInfo);
        }
    }

    if let Some((from, to)) = link {
        let linked = p.task(to).is_some_and(|t| t.predecessors.iter().any(|d| d.predecessor == TaskRef::Local(from)));
        if linked {
            // Dropping onto a task already linked would reset that link's type and lag.
            st.toast(ToastKind::Info, "These tasks are already linked; change the link in Task Information.");
        } else {
            let r = doc.edit_msg("Link Tasks", |p| p.link(TaskRef::Local(from), to, LinkType::FS, Lag::None).map_err(|e| e.to_string()));
            if let Err(e) = r {
                st.toast(ToastKind::Warning, e);
            }
        }
    }
}

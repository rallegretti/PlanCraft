//! Resource Sheet: an editable table of all resources.

use std::collections::{HashMap, HashSet};

use egui::{Rect, Ui};
use plancraft_core::prelude::*;
use plancraft_core::workspace::{ChangeSource, Document};

use crate::commands::Command;
use crate::format;
use crate::state::{ToastKind, UiState};
use crate::theme;
use crate::widgets::grid::{self, Cell, ColumnDef, GridConfig, GridEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Indicators,
    Name,
    Type,
    Category,
    Initials,
    Group,
    MaxUnits,
    StdRate,
    OvtRate,
    CostPerUse,
    Calendar,
    Email,
    Work,
    Cost,
}

const COLS: [Col; 14] = [
    Col::Indicators,
    Col::Name,
    Col::Type,
    Col::Category,
    Col::Initials,
    Col::Group,
    Col::MaxUnits,
    Col::StdRate,
    Col::OvtRate,
    Col::CostPerUse,
    Col::Calendar,
    Col::Email,
    Col::Work,
    Col::Cost,
];

fn type_label(t: ResourceType) -> &'static str {
    match t {
        ResourceType::Work => "Work",
        ResourceType::Material => "Material",
        ResourceType::Cost => "Cost",
    }
}

fn defs(p: &Project) -> Vec<ColumnDef> {
    let cal_names: Vec<String> = p.calendars.iter().map(|c| c.name.clone()).collect();
    let cats: Vec<&str> = ResourceCategory::ALL.iter().map(|c| c.label()).collect();
    COLS.iter()
        .map(|c| match c {
            Col::Indicators => ColumnDef::new("ℹ", 30.0).read_only().center(),
            Col::Name => ColumnDef::new("Resource Name", 200.0),
            Col::Type => ColumnDef::new("Type", 74.0).choice(&["Work", "Material", "Cost"]),
            Col::Category => ColumnDef::new("Category", 120.0).choice(&cats),
            Col::Initials => ColumnDef::new("Initials", 60.0),
            Col::Group => ColumnDef::new("Group", 110.0),
            Col::MaxUnits => ColumnDef::new("Max.", 62.0).right(),
            Col::StdRate => ColumnDef::new("Std. Rate", 86.0).right(),
            Col::OvtRate => ColumnDef::new("Ovt. Rate", 86.0).right(),
            Col::CostPerUse => ColumnDef::new("Cost/Use", 84.0).right(),
            Col::Calendar => {
                let mut d = ColumnDef::new("Base Calendar", 110.0);
                d.editor = grid::Editor::Choice(std::iter::once("(Project)".to_string()).chain(cal_names.iter().cloned()).collect());
                d
            }
            Col::Email => ColumnDef::new("Email", 170.0),
            Col::Work => ColumnDef::new("Work", 76.0).right().read_only(),
            Col::Cost => ColumnDef::new("Cost", 96.0).right().read_only(),
        })
        .collect()
}

fn cell(p: &Project, r: &Resource, col: Col, over: &HashSet<ResourceId>, work: &HashMap<ResourceId, (Minutes, f64)>) -> Cell {
    let red = |mut c: Cell| {
        if over.contains(&r.id) {
            c.color = Some(theme::pal().ERROR);
        }
        c
    };
    match col {
        Col::Indicators => {
            let mut icons = Vec::new();
            if over.contains(&r.id) {
                icons.push(("⚠".to_string(), theme::pal().ERROR, "This resource is overallocated and should be leveled.".to_string()));
            }
            if !r.exceptions.is_empty() {
                icons.push((
                    "📅".to_string(),
                    theme::pal().ACCENT_MID,
                    format!("{} calendar exception(s): time off or altered hours", r.exceptions.len()),
                ));
            }
            if !r.notes.is_empty() {
                icons.push(("📝".to_string(), theme::pal().TEXT_WEAK, r.notes.clone()));
            }
            icons.truncate(2);
            Cell { icons, ..Default::default() }
        }
        Col::Name => red(Cell::text(r.name.clone())),
        Col::Type => Cell::text(type_label(r.kind)),
        Col::Category => Cell::text(r.category.label()),
        Col::Initials => Cell::text(r.initials.clone()),
        Col::Group => Cell::text(r.group.clone()),
        Col::MaxUnits => {
            if r.kind == ResourceType::Work {
                Cell::text(format::units(r.max_units))
            } else {
                Cell::default()
            }
        }
        Col::StdRate => {
            let unit = match r.kind {
                ResourceType::Work => "/hr".to_string(),
                ResourceType::Material => {
                    if r.material_label.is_empty() {
                        String::new()
                    } else {
                        format!("/{}", r.material_label)
                    }
                }
                ResourceType::Cost => return Cell::default(),
            };
            Cell {
                text: format!("{}{unit}", format::money(p, r.standard_rate)),
                edit_text: Some(r.standard_rate.to_string()),
                ..Default::default()
            }
        }
        Col::OvtRate => {
            if r.kind == ResourceType::Work {
                Cell {
                    text: format!("{}/hr", format::money(p, r.overtime_rate)),
                    edit_text: Some(r.overtime_rate.to_string()),
                    ..Default::default()
                }
            } else {
                Cell::default()
            }
        }
        Col::CostPerUse => {
            Cell { text: format::money(p, r.cost_per_use), edit_text: Some(r.cost_per_use.to_string()), ..Default::default() }
        }
        Col::Calendar => Cell::text(r.calendar.and_then(|c| p.calendar(c)).map(|c| c.name.clone()).unwrap_or_else(|| "(Project)".into())),
        Col::Email => Cell::text(r.email.clone()),
        Col::Work => Cell::text(work.get(&r.id).map(|w| format_work(w.0)).unwrap_or_else(|| format_work(0))),
        Col::Cost => Cell::text(format::money(p, work.get(&r.id).map(|w| w.1).unwrap_or(0.0))),
    }
}

fn commit(p: &mut Project, id: ResourceId, col: Col, text: &str) -> Result<(), String> {
    let cal_id = p.calendars.iter().find(|c| c.name == text).map(|c| c.id);
    let r = p.resource_mut(id).ok_or("resource not found")?;
    match col {
        Col::Name => {
            if text.is_empty() {
                return Err("resource name cannot be empty".into());
            }
            r.name = text.into();
        }
        Col::Type => {
            r.kind = match text {
                "Material" => ResourceType::Material,
                "Cost" => ResourceType::Cost,
                _ => ResourceType::Work,
            }
        }
        Col::Category => r.category = ResourceCategory::ALL.into_iter().find(|c| c.label() == text).unwrap_or_default(),
        Col::Initials => r.initials = text.into(),
        Col::Group => r.group = text.into(),
        Col::MaxUnits => r.max_units = format::parse_units(text).ok_or("enter units like 100% or 50%")?.max(0.0),
        Col::StdRate => r.standard_rate = format::parse_money(text).ok_or("invalid rate")?,
        Col::OvtRate => r.overtime_rate = format::parse_money(text).ok_or("invalid rate")?,
        Col::CostPerUse => r.cost_per_use = format::parse_money(text).ok_or("invalid amount")?,
        Col::Calendar => r.calendar = cal_id,
        Col::Email => r.email = text.into(),
        _ => return Err("this field is calculated".into()),
    }
    Ok(())
}

pub fn show(ui: &mut Ui, rect: Rect, doc: &mut Document, st: &mut UiState, out: &mut Vec<Command>) {
    let defs = defs(&doc.project);
    let over: HashSet<ResourceId> = plancraft_core::workload::overallocations(&doc.project).into_iter().map(|(r, _)| r).collect();
    let mut work: HashMap<ResourceId, (Minutes, f64)> = HashMap::new();
    for a in &doc.project.assignments {
        let Some(r) = doc.project.resource(a.resource) else {
            continue;
        };
        let e = work.entry(a.resource).or_default();
        e.0 += a.work;
        e.1 += match r.kind {
            ResourceType::Work => a.work as f64 / 60.0 * r.standard_rate + r.cost_per_use,
            ResourceType::Material => a.units * r.standard_rate + r.cost_per_use,
            ResourceType::Cost => a.cost,
        };
    }
    let n = doc.project.resources.len();
    let cfg = GridConfig {
        id: egui::Id::new("resource_sheet"),
        columns: &defs,
        row_count: n,
        blank_rows: 20,
        row_numbers: true,
        show_vscroll: true,
        header_height: 30.0,
        row_height: theme::ROW_HEIGHT,
    };
    let output = {
        let p = &doc.project;
        let sel = &st.sel;
        let is_sel = |r: usize| sel.resources.contains(&p.resources[r].id);
        let mut cellf = |r: usize, c: usize| cell(p, &p.resources[r], COLS[c], &over, &work);
        grid::show(ui, rect, &mut st.resource_grid, &cfg, &is_sel, &mut cellf)
    };
    for ev in output.events {
        match ev {
            GridEvent::Select { row, ctrl, .. } => {
                let id = doc.project.resources[row].id;
                if ctrl {
                    if !st.sel.resources.remove(&id) {
                        st.sel.resources.insert(id);
                    }
                } else {
                    st.sel.select_resource(id);
                }
            }
            GridEvent::CursorRow { row, .. } => st.sel.select_resource(doc.project.resources[row].id),
            GridEvent::Activate { row, .. } => {
                st.sel.select_resource(doc.project.resources[row].id);
                out.push(Command::ResourceInfo);
            }
            GridEvent::Commit { row, col, text } => {
                if row < n {
                    let id = doc.project.resources[row].id;
                    if let Err(e) =
                        doc.try_edit(&format!("Edit {}", defs[col].title), ChangeSource::User, |p| commit(p, id, COLS[col], &text))
                    {
                        st.toast(ToastKind::Error, e);
                    }
                } else if !text.is_empty() {
                    let r = doc.try_edit("New Resource", ChangeSource::User, |p| {
                        let name = if COLS[col] == Col::Name { text.clone() } else { format!("Resource {}", p.resources.len() + 1) };
                        let id = p.add_resource(name);
                        if COLS[col] != Col::Name {
                            commit(p, id, COLS[col], &text)?;
                        }
                        Ok::<_, String>(id)
                    });
                    match r {
                        Ok(id) => st.sel.select_resource(id),
                        Err(e) => st.toast(ToastKind::Error, e),
                    }
                }
            }
            GridEvent::DeleteKey => out.push(Command::DeleteResources),
            GridEvent::InsertKey => out.push(Command::InsertResource),
            GridEvent::ContextMenu { row, .. } => {
                st.sel.select_resource(doc.project.resources[row].id);
            }
            GridEvent::ClickEmpty { .. } => st.sel.resources.clear(),
            GridEvent::ToggleExpand(_) => {}
        }
    }
}

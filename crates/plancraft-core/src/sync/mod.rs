//! Shared editing: several people working on one plan through a host (see docs/COLLABORATION.md).
//!
//! This module is plain logic with no networking, and nothing here runs unless someone shares a
//! plan. One [`Host`] holds the plan everyone agrees on and puts changes in one order. Each person's
//! PlanCraft keeps a [`Replica`]: the host's plan plus that person's changes the host has not
//! confirmed yet.
//!
//! - A change is a [`Patch`]: the plan's values that differ between two versions (top-level
//!   values, and the items of each list matched by id, field by field). The schedule (`sched`) is
//!   left out, because every copy works it out again by recalculating.
//! - A change made from an older version is *rebased* over the changes made since ([`rebase`]).
//!   Changes to different fields of the same task combine; the same field changed to different
//!   values, an item changed by one person and deleted by the other, or a list reordered by both
//!   people is a [`Conflict`].
//! - Ids of new items never collide: the host leases each replica its own block of ids
//!   ([`Host::lease`]). If a block runs out, the host refuses a clashing id; it never merges two
//!   items into one.

mod host;
mod patch;
mod replica;

pub use host::{Change, Entry, Host, Outcome};
pub use patch::{Rebased, apply, diff, fit_counters, rebase};
pub use replica::{Merged, Replica};

use crate::model::Project;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// The host's version of a shared plan, counted up by one for every change it accepts.
pub type Version = u64;

/// How many ids of each kind (tasks, resources, ...) a lease hands out.
pub const LEASE_SIZE: u32 = 1_000;

/// Changes the host keeps for replicas that fall behind (older ones resync from a snapshot).
pub const LOG_LIMIT: usize = 1_000;

/// Identifies one change, so a change sent twice (after a timeout) is applied once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChangeId(pub Uuid);

impl ChangeId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ChangeId {
    fn default() -> Self {
        Self::new()
    }
}

/// The lists of a plan whose items are matched by key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collection {
    Calendars,
    Tasks,
    Resources,
    Assignments,
    TimeEntries,
    ExternalProjects,
    CustomFields,
    Tables,
    BoardColumns,
}

impl Collection {
    pub const ALL: [Collection; 9] = [
        Collection::Calendars,
        Collection::Tasks,
        Collection::Resources,
        Collection::Assignments,
        Collection::TimeEntries,
        Collection::ExternalProjects,
        Collection::CustomFields,
        Collection::Tables,
        Collection::BoardColumns,
    ];

    /// The list's field in a saved plan.
    pub fn field(self) -> &'static str {
        match self {
            Collection::Calendars => "calendars",
            Collection::Tasks => "tasks",
            Collection::Resources => "resources",
            Collection::Assignments => "assignments",
            Collection::TimeEntries => "time_entries",
            Collection::ExternalProjects => "external_projects",
            Collection::CustomFields => "custom_fields",
            Collection::Tables => "tables",
            Collection::BoardColumns => "board_columns",
        }
    }

    pub fn from_field(field: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.field() == field)
    }

    /// The field that identifies an item of the list.
    pub fn key_field(self) -> &'static str {
        match self {
            Collection::Tables => "name",
            _ => "id",
        }
    }

    /// What one item is called in messages.
    fn noun(self) -> &'static str {
        match self {
            Collection::Calendars => "calendar",
            Collection::Tasks => "task",
            Collection::Resources => "resource",
            Collection::Assignments => "assignment",
            Collection::TimeEntries => "time entry",
            Collection::ExternalProjects => "linked project",
            Collection::CustomFields => "custom field",
            Collection::Tables => "table",
            Collection::BoardColumns => "board column",
        }
    }
}

/// An item's key: its id (or a table's name) written as JSON, e.g. `42` or `"Entry"`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Key(pub String);

/// A field's new value, or that the field is gone (a value saved files leave out when empty).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Set(serde_json::Value),
    Unset,
}

/// What changed between two versions of a plan (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    /// Top-level values that changed. A list whose items cannot be matched by key (two items
    /// with one key) is replaced as a whole here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Slot>,
    /// Lists whose items changed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lists: BTreeMap<Collection, ListPatch>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.lists.values().all(ListPatch::is_empty)
    }
}

/// What changed in one list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListPatch {
    /// New items, whole (without their schedule).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub added: BTreeMap<Key, serde_json::Value>,
    /// Fields that changed in existing items.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub changed: BTreeMap<Key, BTreeMap<String, Slot>>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub removed: BTreeSet<Key>,
    /// The order of all items afterwards, when it changed (items added, removed or moved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<Key>>,
    /// Items that were there before changed places (a move, not only additions and removals).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moved: bool,
}

impl ListPatch {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty() && self.order.is_none()
    }
}

/// Two people changed the same thing at the same time; the later change was not applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub kind: ConflictKind,
    /// The list of the item concerned (`None` for a top-level value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<Collection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Who made the change that came first, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// Both changed the same value to different values.
    BothChanged,
    /// You changed an item they deleted.
    ChangedDeleted,
    /// You deleted an item they changed.
    DeletedChanged,
    /// Both reordered the same list.
    BothMoved,
    /// Both added an item with the same key.
    SameNewKey,
    /// Your change no longer fits the plan as the others' changes left it (e.g. an assignment to
    /// a task someone deleted).
    NoLongerFits,
}

impl Conflict {
    /// A sentence for the person whose change was not applied, e.g. "Ana changed the duration of
    /// task 'Pour slab' at the same time".
    pub fn describe(&self, plan: &Project) -> String {
        let by = self.by.as_deref().unwrap_or("Someone else");
        let item = self.item_name(plan);
        let field = self.field.as_deref().map(|f| f.replace('_', " "));
        match self.kind {
            ConflictKind::BothChanged => match (field, item) {
                (Some(f), Some(i)) => format!("{by} changed the {f} of {i} at the same time"),
                (Some(f), None) => format!("{by} changed the plan's {f} at the same time"),
                (None, Some(i)) => format!("{by} changed {i} at the same time"),
                (None, None) => format!("{by} changed the plan at the same time"),
            },
            ConflictKind::ChangedDeleted => format!("{by} deleted {}, which you changed", item.unwrap_or_else(|| "an item".into())),
            ConflictKind::DeletedChanged => format!("{by} changed {}, which you deleted", item.unwrap_or_else(|| "an item".into())),
            ConflictKind::BothMoved => {
                let list = self.list.map_or("items", Collection::field).replace('_', " ");
                format!("{by} reordered the {list} at the same time")
            }
            ConflictKind::SameNewKey => format!("{by} added {} at the same time", item.unwrap_or_else(|| "an item".into())),
            ConflictKind::NoLongerFits => match (item, field) {
                (Some(i), _) => format!("Your change to {i} no longer fits the plan after the others' changes"),
                (None, Some(f)) => format!("Your change to the plan's {f} no longer fits the plan after the others' changes"),
                (None, None) => "Your change no longer fits the plan after the others' changes".into(),
            },
        }
    }

    fn item_name(&self, plan: &Project) -> Option<String> {
        let list = self.list?;
        let key = self.key.as_ref()?;
        let id: Option<u32> = key.0.parse().ok();
        let name = match (list, id) {
            (Collection::Tasks, Some(id)) => plan.tasks.iter().find(|t| t.id.0 == id).map(|t| t.name.clone()),
            (Collection::Resources, Some(id)) => plan.resources.iter().find(|r| r.id.0 == id).map(|r| r.name.clone()),
            (Collection::Calendars, Some(id)) => plan.calendars.iter().find(|c| c.id.0 == id).map(|c| c.name.clone()),
            (Collection::Tables, _) => serde_json::from_str::<String>(&key.0).ok(),
            _ => None,
        };
        Some(match name {
            Some(n) => format!("{} '{n}'", list.noun()),
            None => format!("{} {}", list.noun(), key.0),
        })
    }
}

/// Why a patch could not be worked out or applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    /// The plan could not be turned into values or back (a damaged patch).
    #[error("the change does not fit the plan: {0}")]
    Shape(String),
    /// A patch tries to change something sharing never changes (the plan's id, its schedule,
    /// an item's key).
    #[error("the change tries to set '{0}', which cannot be changed")]
    Fixed(String),
    #[error("{0} was not found")]
    Missing(String),
    #[error("{0} already exists")]
    Exists(String),
    /// A replica missed changes and must load the host's plan again.
    #[error("changes were missed; the plan must be loaded again")]
    Gap,
}

#[cfg(test)]
mod tests;

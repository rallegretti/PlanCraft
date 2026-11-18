//! Working out, applying and rebasing [`Patch`]es.

use super::{Collection, Conflict, ConflictKind, Key, ListPatch, Patch, Slot, SyncError};
use crate::model::Project;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Top-level values a patch never carries: fixed for the plan's lifetime (`format`, `version`,
/// `id`), handed out by the host (`ids`, see [`super::Host::lease`]) or worked out by
/// recalculation (`sched`).
const NOT_SHARED: [&str; 5] = ["format", "version", "id", "ids", "sched"];

/// An item's schedule, worked out by recalculation.
const ITEM_SCHEDULE: &str = "sched";

fn state(p: &Project) -> Result<Map<String, Value>, SyncError> {
    match serde_json::to_value(p) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(SyncError::Shape("a plan is not an object".into())),
        Err(e) => Err(SyncError::Shape(e.to_string())),
    }
}

fn slot(v: Option<&Value>) -> Slot {
    v.map_or(Slot::Unset, |v| Slot::Set(v.clone()))
}

/// The items of a list with their keys, or `None` when they cannot be matched by key (not a
/// list of objects, an item without its key, or two items with one key).
fn keyed(list: Collection, v: Option<&Value>) -> Option<Vec<(Key, &Map<String, Value>)>> {
    let items = match v {
        None => return Some(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return None,
    };
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let obj = item.as_object()?;
        let key = Key(serde_json::to_string(obj.get(list.key_field())?).ok()?);
        if !seen.insert(key.clone()) {
            return None;
        }
        out.push((key, obj));
    }
    Some(out)
}

/// Field-by-field changes from `a` to `b`, leaving out the schedule and `skip`.
fn diff_fields(a: &Map<String, Value>, b: &Map<String, Value>, skip: &[&str]) -> BTreeMap<String, Slot> {
    let mut out = BTreeMap::new();
    for k in a.keys().chain(b.keys()) {
        if skip.contains(&k.as_str()) || out.contains_key(k) {
            continue;
        }
        let (old, new) = (a.get(k), b.get(k));
        if old != new {
            out.insert(k.clone(), slot(new));
        }
    }
    out
}

fn without_schedule(item: &Map<String, Value>) -> Value {
    let mut item = item.clone();
    item.remove(ITEM_SCHEDULE);
    Value::Object(item)
}

fn diff_list(list: Collection, a: &[(Key, &Map<String, Value>)], b: &[(Key, &Map<String, Value>)]) -> ListPatch {
    let before: HashMap<&Key, &Map<String, Value>> = a.iter().map(|(k, v)| (k, *v)).collect();
    let after: HashSet<&Key> = b.iter().map(|(k, _)| k).collect();
    let mut out = ListPatch::default();
    for (k, item) in b {
        match before.get(k) {
            None => {
                out.added.insert(k.clone(), without_schedule(item));
            }
            Some(old) => {
                let fields = diff_fields(old, item, &[ITEM_SCHEDULE, list.key_field()]);
                if !fields.is_empty() {
                    out.changed.insert(k.clone(), fields);
                }
            }
        }
    }
    out.removed = a.iter().map(|(k, _)| k).filter(|k| !after.contains(k)).cloned().collect();
    let old_order: Vec<&Key> = a.iter().map(|(k, _)| k).collect();
    let new_order: Vec<&Key> = b.iter().map(|(k, _)| k).collect();
    if old_order != new_order {
        let kept_before: Vec<&Key> = old_order.iter().copied().filter(|k| after.contains(k)).collect();
        let kept_after: Vec<&Key> = new_order.iter().copied().filter(|k| before.contains_key(k)).collect();
        out.moved = kept_before != kept_after;
        out.order = Some(new_order.into_iter().cloned().collect());
    }
    out
}

/// What changed from `before` to `after` (leaving out schedules and id counters).
pub fn diff(before: &Project, after: &Project) -> Result<Patch, SyncError> {
    let (a, b) = (state(before)?, state(after)?);
    let mut patch = Patch::default();
    let mut done = HashSet::new();
    for k in a.keys().chain(b.keys()) {
        if NOT_SHARED.contains(&k.as_str()) || !done.insert(k.as_str()) {
            continue;
        }
        let (old, new) = (a.get(k), b.get(k));
        if let Some(list) = Collection::from_field(k)
            && let (Some(ka), Some(kb)) = (keyed(list, old), keyed(list, new))
        {
            let lp = diff_list(list, &ka, &kb);
            if !lp.is_empty() {
                patch.lists.insert(list, lp);
            }
        } else if old != new {
            patch.fields.insert(k.clone(), slot(new));
        }
    }
    Ok(patch)
}

fn set(obj: &mut Map<String, Value>, field: &str, s: &Slot) {
    match s {
        Slot::Set(v) => {
            obj.insert(field.to_string(), v.clone());
        }
        Slot::Unset => {
            obj.remove(field);
        }
    }
}

fn apply_list(list: Collection, current: Option<Value>, lp: &ListPatch) -> Result<Value, SyncError> {
    let what = |k: &Key| format!("{} {}", list.noun(), k.0);
    let mut items: Vec<(Key, Map<String, Value>)> = match keyed(list, current.as_ref()) {
        Some(items) => items.into_iter().map(|(k, v)| (k, v.clone())).collect(),
        None => return Err(SyncError::Shape(format!("the {} cannot be matched by key", list.field()))),
    };
    items.retain(|(k, _)| !lp.removed.contains(k));
    let index: HashMap<Key, usize> = items.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect();
    for (k, fields) in &lp.changed {
        let Some((_, item)) = index.get(k).and_then(|i| items.get_mut(*i)) else {
            return Err(SyncError::Missing(what(k)));
        };
        for (field, s) in fields {
            if field == ITEM_SCHEDULE || field == list.key_field() {
                return Err(SyncError::Fixed(field.clone()));
            }
            set(item, field, s);
        }
    }
    for (k, v) in &lp.added {
        if index.contains_key(k) {
            return Err(SyncError::Exists(what(k)));
        }
        let Value::Object(item) = v else {
            return Err(SyncError::Shape(format!("{} is not an object", what(k))));
        };
        let own = item.get(list.key_field()).and_then(|v| serde_json::to_string(v).ok());
        if own.as_deref() != Some(k.0.as_str()) {
            return Err(SyncError::Shape(format!("{} carries another key", what(k))));
        }
        items.push((k.clone(), item.clone()));
    }
    if let Some(order) = &lp.order {
        // Items take the order's places (the first mention counts; unknown keys are skipped). An
        // item the order leaves out (one whose deletion clashed) stays after the item it followed.
        let present: HashSet<&Key> = items.iter().map(|(k, _)| k).collect();
        let mut seen = HashSet::new();
        let named: Vec<Key> = order.iter().filter(|k| present.contains(k) && seen.insert(*k)).cloned().collect();
        let current: Vec<Key> = items.iter().map(|(k, _)| k.clone()).collect();
        let left_out: BTreeSet<&Key> = current.iter().filter(|k| !seen.contains(k)).collect();
        let sequence = merge_orders(&named, &current, &left_out, &|_| false);
        let mut by_key: HashMap<Key, Map<String, Value>> = items.into_iter().collect();
        items = sequence.into_iter().filter_map(|k| by_key.remove(&k).map(|v| (k, v))).collect();
    }
    Ok(Value::Array(items.into_iter().map(|(_, v)| Value::Object(v)).collect()))
}

/// The plan with `patch` applied. The caller validates and recalculates the result.
pub fn apply(p: &Project, patch: &Patch) -> Result<Project, SyncError> {
    let mut s = state(p)?;
    for (k, v) in &patch.fields {
        if NOT_SHARED.contains(&k.as_str()) {
            return Err(SyncError::Fixed(k.clone()));
        }
        set(&mut s, k, v);
    }
    for (list, lp) in &patch.lists {
        if lp.is_empty() {
            continue;
        }
        let field = list.field();
        let updated = apply_list(*list, s.remove(field), lp)?;
        s.insert(field.to_string(), updated);
    }
    serde_json::from_value(Value::Object(s)).map_err(|e| SyncError::Shape(e.to_string()))
}

/// Raises the plan's id counters above every id it uses, so new items never take a used id.
pub fn fit_counters(p: &mut Project) {
    fn raise(counter: &mut u32, ids: impl Iterator<Item = u32>) {
        if let Some(max) = ids.max() {
            *counter = (*counter).max(max);
        }
    }
    raise(&mut p.ids.task, p.tasks.iter().map(|t| t.id.0));
    raise(&mut p.ids.resource, p.resources.iter().map(|r| r.id.0));
    raise(&mut p.ids.calendar, p.calendars.iter().map(|c| c.id.0));
    raise(&mut p.ids.assignment, p.assignments.iter().map(|a| a.id.0));
    raise(&mut p.ids.time_entry, p.time_entries.iter().map(|e| e.id.0));
    raise(&mut p.ids.custom_field, p.custom_fields.iter().map(|f| f.id.0));
    raise(&mut p.ids.board_column, p.board_columns.iter().map(|c| c.id.0));
}

/// A patch rebased over a change made before it, with the parts that clashed left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rebased {
    pub patch: Patch,
    pub conflicts: Vec<Conflict>,
}

/// Rebases `mine` (made from an older version) over `theirs` (made since, by `by`).
///
/// Values both changed to the same thing are dropped from `mine` (already done); values changed
/// to different things, items one changed and the other deleted, a list both reordered, and items
/// both added with one key are conflicts, left out of the result. The host refuses a change with
/// any conflict; a replica keeps the rest of its unconfirmed work.
pub fn rebase(mine: &Patch, theirs: &Patch, by: Option<&str>) -> Rebased {
    let mut out = Rebased::default();
    let conflict = |kind, list: Option<Collection>, key: Option<&Key>, field: Option<&str>| Conflict {
        kind,
        list,
        key: key.cloned(),
        field: field.map(str::to_string),
        by: by.map(str::to_string),
    };

    for (k, v) in &mine.fields {
        // A whole list replaced here clashes with any change to that list there.
        let list_touched = Collection::from_field(k).is_some_and(|c| theirs.lists.get(&c).is_some_and(|lp| !lp.is_empty()));
        match theirs.fields.get(k) {
            Some(t) if t == v => {}
            Some(_) => out.conflicts.push(conflict(ConflictKind::BothChanged, None, None, Some(k))),
            None if list_touched => out.conflicts.push(conflict(ConflictKind::BothChanged, None, None, Some(k))),
            None => {
                out.patch.fields.insert(k.clone(), v.clone());
            }
        }
    }

    let empty = ListPatch::default();
    for (list, mlp) in &mine.lists {
        if theirs.fields.contains_key(list.field()) {
            out.conflicts.push(conflict(ConflictKind::BothChanged, None, None, Some(list.field())));
            continue;
        }
        let tlp = theirs.lists.get(list).unwrap_or(&empty);
        let list = *list;
        let mut lp = ListPatch::default();

        for (k, v) in &mlp.added {
            match tlp.added.get(k) {
                Some(t) if t == v => {}
                Some(_) => out.conflicts.push(conflict(ConflictKind::SameNewKey, Some(list), Some(k), None)),
                None => {
                    lp.added.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, fields) in &mlp.changed {
            if tlp.removed.contains(k) {
                out.conflicts.push(conflict(ConflictKind::ChangedDeleted, Some(list), Some(k), None));
                continue;
            }
            let theirs_fields = tlp.changed.get(k);
            let mut kept = BTreeMap::new();
            for (f, s) in fields {
                match theirs_fields.and_then(|t| t.get(f)) {
                    Some(t) if t == s => {}
                    Some(_) => out.conflicts.push(conflict(ConflictKind::BothChanged, Some(list), Some(k), Some(f))),
                    None => {
                        kept.insert(f.clone(), s.clone());
                    }
                }
            }
            if !kept.is_empty() {
                lp.changed.insert(k.clone(), kept);
            }
        }
        for k in &mlp.removed {
            if tlp.removed.contains(k) {
                continue;
            }
            if tlp.changed.contains_key(k) {
                out.conflicts.push(conflict(ConflictKind::DeletedChanged, Some(list), Some(k), None));
                continue;
            }
            lp.removed.insert(k.clone());
        }

        if let Some(mo) = &mlp.order {
            let both_moved = mlp.moved && tlp.moved && tlp.order.as_ref() != Some(mo);
            if both_moved {
                out.conflicts.push(conflict(ConflictKind::BothMoved, Some(list), None, None));
            }
            let my_moves = mlp.moved && !tlp.moved;
            // Gone after both changes: what either deleted, and my additions that clashed.
            let gone =
                |k: &Key| lp.removed.contains(k) || tlp.removed.contains(k) || (mlp.added.contains_key(k) && !lp.added.contains_key(k));
            let order = match (&tlp.order, my_moves) {
                (None, _) => mo.iter().filter(|k| !gone(k)).cloned().collect(),
                // Their reordering stands; my new items go after the items they followed.
                (Some(to), false) => merge_orders(to, mo, &lp.added.keys().collect(), &gone),
                // My reordering stands; their new items go after the items they followed.
                (Some(to), true) => merge_orders(mo, to, &tlp.added.keys().collect(), &gone),
            };
            lp.order = Some(order);
            lp.moved = my_moves;
        }

        if !lp.is_empty() {
            out.patch.lists.insert(list, lp);
        }
    }
    out
}

/// `primary`'s order with the keys of `insert` placed as `secondary` places them: each right
/// after the nearest key before it in `secondary` (or first). Keys of `insert` that `secondary`
/// does not name go last, so no key is ever lost. Linear in the lengths.
fn merge_orders(primary: &[Key], secondary: &[Key], insert: &BTreeSet<&Key>, gone: &dyn Fn(&Key) -> bool) -> Vec<Key> {
    let base: Vec<&Key> = primary.iter().filter(|k| !gone(k) && !insert.contains(k)).collect();
    let mut placed: HashSet<&Key> = base.iter().copied().collect();
    // The inserted keys that follow each key (`None`: the start), in `secondary`'s order.
    let mut after: HashMap<Option<&Key>, Vec<&Key>> = HashMap::new();
    let mut anchor: Option<&Key> = None;
    for k in secondary {
        if insert.contains(k) && !placed.contains(k) && !gone(k) {
            after.entry(anchor).or_default().push(k);
            placed.insert(k);
            anchor = Some(k);
        } else if placed.contains(k) {
            anchor = Some(k);
        }
    }
    let mut out = Vec::with_capacity(placed.len());
    // Each key, then what follows it (and what follows that), without recursion.
    let emit = |from: Option<&Key>, out: &mut Vec<Key>| {
        let mut stack: Vec<&Key> = after.get(&from).map(|c| c.iter().rev().copied().collect()).unwrap_or_default();
        while let Some(k) = stack.pop() {
            out.push(k.clone());
            if let Some(c) = after.get(&Some(k)) {
                stack.extend(c.iter().rev().copied());
            }
        }
    };
    emit(None, &mut out);
    for k in base {
        out.push(k.clone());
        emit(Some(k), &mut out);
    }
    for k in insert {
        if !placed.contains(*k) && !gone(k) {
            out.push((*k).clone());
        }
    }
    out
}

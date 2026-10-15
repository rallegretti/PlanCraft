//! Time-phased workload, capacity and over-allocation analysis.
//!
//! Every assignment's work is spread across the working time of its task
//! (shaped by the assignment's [`WorkContour`]) and compared against each
//! resource's capacity: working minutes on the resource's calendar multiplied
//! by its available units on that date.

use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::calendar::{CalLayer, WorkCal};
use crate::duration::Minutes;
use crate::ids::*;
use crate::model::*;
use crate::schedule::task_calendar;

/// Size of the buckets used for time-phased data.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    #[default]
    Day,
    Week,
    Month,
}

impl Granularity {
    /// First day of the bucket containing `d` (weeks start on Monday).
    pub fn bucket_start(self, d: NaiveDate) -> NaiveDate {
        match self {
            Granularity::Day => d,
            Granularity::Week => d - Duration::days(d.weekday().num_days_from_monday() as i64),
            Granularity::Month => d.with_day(1).unwrap_or(d),
        }
    }

    /// First day of the bucket after the one starting at `start`.
    pub fn next(self, start: NaiveDate) -> NaiveDate {
        match self {
            Granularity::Day => start + Duration::days(1),
            Granularity::Week => start + Duration::days(7),
            Granularity::Month => {
                let (y, m) = if start.month() == 12 { (start.year() + 1, 1) } else { (start.year(), start.month() + 1) };
                NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(start + Duration::days(31))
            }
        }
    }

    /// Bucket start dates covering `[from, to]`.
    pub fn buckets(self, from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        let mut out = Vec::new();
        let mut d = self.bucket_start(from);
        while d <= to && out.len() < 5000 {
            out.push(d);
            d = self.next(d);
        }
        out
    }
}

/// Work of one assignment per day, in minutes (fractional to avoid rounding drift).
pub fn assignment_daily_work(p: &Project, a: &Assignment) -> BTreeMap<NaiveDate, f64> {
    let Some(ti) = p.task_index(a.task) else {
        return BTreeMap::new();
    };
    let t = &p.tasks[ti];
    let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else {
        return BTreeMap::new();
    };
    let cal = task_calendar(p, ti);
    distribute(&cal, s, f, a.work as f64, a.contour)
}

/// Spreads `work` over the working time of `[start, finish)` following `contour`.
pub fn distribute(cal: &WorkCal, start: NaiveDateTime, finish: NaiveDateTime, work: f64, contour: WorkContour) -> BTreeMap<NaiveDate, f64> {
    let mut out = BTreeMap::new();
    if work <= 0.0 || finish <= start {
        return out;
    }
    let mut days: Vec<(NaiveDate, f64)> = Vec::new();
    let mut d = start.date();
    while d <= finish.date() {
        let wm = cal.working_minutes_within(d, start, finish) as f64;
        if wm > 0.0 {
            days.push((d, wm));
        }
        d += Duration::days(1);
    }
    let total: f64 = days.iter().map(|(_, w)| w).sum();
    if total <= 0.0 {
        // No working time in span (e.g. elapsed-time task): spread evenly over calendar days.
        let n = (finish.date() - start.date()).num_days().max(0) + 1;
        for i in 0..n {
            out.insert(start.date() + Duration::days(i), work / n as f64);
        }
        return out;
    }
    let mut cum = 0.0;
    let weights: Vec<f64> = days
        .iter()
        .map(|(_, wm)| {
            let x = (cum + wm / 2.0) / total;
            cum += wm;
            contour.weight(x) * wm
        })
        .collect();
    let wsum: f64 = weights.iter().sum();
    for ((day, _), w) in days.iter().zip(weights) {
        out.insert(*day, work * w / wsum);
    }
    out
}

/// Working calendar of a resource (base calendar plus personal exceptions).
pub fn resource_calendar<'a>(p: &'a Project, r: &'a Resource) -> WorkCal<'a> {
    let base = r.calendar.and_then(|c| p.calendar(c)).unwrap_or_else(|| p.base_calendar());
    WorkCal::from_layers(vec![CalLayer::new(base, &r.exceptions)])
}

/// Capacity in minutes of a resource on a day: working minutes × available units.
pub fn capacity_on(cal: &WorkCal, r: &Resource, d: NaiveDate) -> f64 {
    cal.working_minutes_on(d) as f64 * r.units_on(d)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PeriodLoad {
    pub start: NaiveDate,
    /// Minutes of available working time (capacity).
    pub capacity: f64,
    /// Minutes of planned work.
    pub load: f64,
    pub overallocated: bool,
}

impl PeriodLoad {
    /// Load as a fraction of capacity (`f64::INFINITY` when there is load but no capacity).
    pub fn utilization(&self) -> f64 {
        if self.capacity > 0.0 {
            self.load / self.capacity
        } else if self.load > 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AssignmentLoad {
    pub assignment: AssignmentId,
    pub task: TaskId,
    pub task_name: String,
    /// Load per period (aligned with `WorkloadReport::periods`).
    pub load: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ResourceLoad {
    pub resource: ResourceId,
    pub name: String,
    pub periods: Vec<PeriodLoad>,
    pub assignments: Vec<AssignmentLoad>,
    /// Total planned work of the resource in the whole project (minutes).
    pub total_work: Minutes,
    pub actual_work: Minutes,
    /// Complexity points assigned (split between co-assigned resources by units).
    pub complexity_points: f64,
    /// Load within the reported window.
    pub window_load: f64,
    pub window_capacity: f64,
    /// Overallocated on at least one day in the window.
    pub overallocated: bool,
    /// Days within the window on which the resource is overallocated.
    pub overallocated_days: Vec<NaiveDate>,
}

impl ResourceLoad {
    pub fn utilization(&self) -> f64 {
        if self.window_capacity > 0.0 { self.window_load / self.window_capacity } else { 0.0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WorkloadReport {
    pub granularity: Granularity,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub periods: Vec<NaiveDate>,
    pub resources: Vec<ResourceLoad>,
    /// Estimated work of tasks without work resources, per period.
    pub unassigned: Vec<f64>,
}

/// Daily load per resource for the whole project.
pub fn daily_loads(p: &Project) -> HashMap<ResourceId, BTreeMap<NaiveDate, f64>> {
    let mut map: HashMap<ResourceId, BTreeMap<NaiveDate, f64>> = HashMap::new();
    for a in &p.assignments {
        let Some(r) = p.resource(a.resource) else {
            continue;
        };
        if r.kind != ResourceType::Work || p.task(a.task).is_none_or(|t| !t.active) {
            continue;
        }
        let entry = map.entry(a.resource).or_default();
        for (d, w) in assignment_daily_work(p, a) {
            *entry.entry(d).or_default() += w;
        }
    }
    map
}

/// Days on which each resource is overallocated (over the whole project).
pub fn overallocations(p: &Project) -> Vec<(ResourceId, Vec<NaiveDate>)> {
    let loads = daily_loads(p);
    let mut out = Vec::new();
    for r in &p.resources {
        let Some(days) = loads.get(&r.id) else {
            continue;
        };
        let cal = resource_calendar(p, r);
        let over: Vec<NaiveDate> = days.iter().filter(|(d, load)| **load > capacity_on(&cal, r, **d) + 0.5).map(|(d, _)| *d).collect();
        if !over.is_empty() {
            out.push((r.id, over));
        }
    }
    out
}

/// Builds a time-phased workload report for `[from, to]`.
pub fn workload(p: &Project, from: NaiveDate, to: NaiveDate, granularity: Granularity) -> WorkloadReport {
    let periods = granularity.buckets(from, to);
    let bucket_of = |d: NaiveDate| -> Option<usize> {
        if d < from || d > to {
            return None;
        }
        let b = granularity.bucket_start(d);
        periods.binary_search(&b).ok()
    };

    let mut resources = Vec::new();
    for r in &p.resources {
        let cal = resource_calendar(p, r);
        let mut periods_load: Vec<PeriodLoad> =
            periods.iter().map(|&start| PeriodLoad { start, capacity: 0.0, load: 0.0, overallocated: false }).collect();
        let mut daily: BTreeMap<NaiveDate, f64> = BTreeMap::new();
        let mut assignment_loads = Vec::new();
        let mut total_work = 0;
        let mut actual_work = 0;
        let mut points = 0.0;
        for a in p.assignments_for_resource(r.id) {
            let Some(t) = p.task(a.task) else { continue };
            total_work += a.work;
            actual_work += a.actual_work
                + p.time_entries.iter().filter(|e| e.task == a.task && e.resource == r.id).map(|e| e.minutes).sum::<Minutes>();
            if let Some(Estimate::Complexity { points: pts }) = t.estimate {
                let total_units: f64 = p.assignments_for_task(t.id).map(|x| x.units).sum();
                if total_units > 0.0 {
                    points += pts * a.units / total_units;
                }
            }
            if r.kind != ResourceType::Work || !t.active {
                continue;
            }
            let mut per = vec![0.0; periods.len()];
            for (d, w) in assignment_daily_work(p, a) {
                *daily.entry(d).or_default() += w;
                if let Some(b) = bucket_of(d) {
                    per[b] += w;
                }
            }
            assignment_loads.push(AssignmentLoad { assignment: a.id, task: t.id, task_name: t.name.clone(), load: per });
        }
        let mut over_days = Vec::new();
        let mut d = from;
        while d <= to {
            let cap = if r.kind == ResourceType::Work { capacity_on(&cal, r, d) } else { 0.0 };
            let load = daily.get(&d).copied().unwrap_or(0.0);
            if let Some(b) = bucket_of(d) {
                periods_load[b].capacity += cap;
                periods_load[b].load += load;
                if load > cap + 0.5 {
                    periods_load[b].overallocated = true;
                }
            }
            if load > cap + 0.5 {
                over_days.push(d);
            }
            d += Duration::days(1);
        }
        let window_load = periods_load.iter().map(|p| p.load).sum();
        let window_capacity = periods_load.iter().map(|p| p.capacity).sum();
        resources.push(ResourceLoad {
            resource: r.id,
            name: r.name.clone(),
            periods: periods_load,
            assignments: assignment_loads,
            total_work,
            actual_work,
            complexity_points: points,
            window_load,
            window_capacity,
            overallocated: !over_days.is_empty(),
            overallocated_days: over_days,
        });
    }

    // Unassigned estimated work.
    let mut unassigned = vec![0.0; periods.len()];
    for (i, t) in p.tasks.iter().enumerate() {
        if t.sched.is_summary || !t.active || t.sched.work <= 0 {
            continue;
        }
        let has_work_res = p.assignments_for_task(t.id).any(|a| p.resource(a.resource).is_some_and(|r| r.kind == ResourceType::Work));
        if has_work_res {
            continue;
        }
        let (Some(s), Some(f)) = (t.sched.start, t.sched.finish) else {
            continue;
        };
        let cal = task_calendar(p, i);
        for (d, w) in distribute(&cal, s, f, t.sched.work as f64, WorkContour::Flat) {
            if let Some(b) = bucket_of(d) {
                unassigned[b] += w;
            }
        }
    }

    WorkloadReport { granularity, from, to, periods, resources, unassigned }
}

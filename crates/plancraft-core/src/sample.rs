//! A ready-made demonstration project.

use chrono::NaiveDate;

use crate::calendar::CalendarException;
use crate::duration::Lag;
use crate::ids::TaskId;
use crate::model::*;
use crate::ops::{PlanError, PlanResult};
use crate::schedule::recalculate;

/// A small product-launch project with summaries, milestones, resources of
/// every category, estimates, progress and tracked time.
pub fn demo_project(start: NaiveDate) -> Project {
    build(start).unwrap_or_else(|_| Project::new("Product Launch", start))
}

fn build(start: NaiveDate) -> PlanResult<Project> {
    let mut p = Project::new("Product Launch", start);
    p.manager = "Project Manager".into();
    p.company = "PlanCraft Demo Co.".into();
    p.notes = "Demonstration project created by PlanCraft.".into();
    let day = p.settings.time.minutes_per_day;

    // Resources.
    let ana = p.add_resource("Ana Lima");
    let ben = p.add_resource("Ben Okafor");
    let chloe = p.add_resource("Chloe Park");
    let studio = p.add_resource("Northwind Design Studio");
    let vendor = p.add_resource("CloudHost Inc.");
    let qa = p.add_resource("QA Team");
    let travel = p.add_resource("Launch Event Budget");
    {
        let r = p.resource_mut(ana).ok_or(PlanError::ResourceNotFound(ana))?;
        r.group = "Product".into();
        r.standard_rate = 85.0;
        r.email = "ana@example.com".into();
        r.exceptions.push(CalendarException::non_working(
            "Vacation",
            start + chrono::Duration::days(23),
            start + chrono::Duration::days(25),
        ));
    }
    {
        let r = p.resource_mut(ben).ok_or(PlanError::ResourceNotFound(ben))?;
        r.group = "Engineering".into();
        r.standard_rate = 95.0;
    }
    {
        let r = p.resource_mut(chloe).ok_or(PlanError::ResourceNotFound(chloe))?;
        r.group = "Engineering".into();
        r.standard_rate = 90.0;
        r.max_units = 0.8;
    }
    {
        let r = p.resource_mut(studio).ok_or(PlanError::ResourceNotFound(studio))?;
        r.category = ResourceCategory::PartnerCompany;
        r.group = "Partners".into();
        r.standard_rate = 120.0;
        r.max_units = 2.0;
    }
    {
        let r = p.resource_mut(vendor).ok_or(PlanError::ResourceNotFound(vendor))?;
        r.category = ResourceCategory::Vendor;
        r.group = "Vendors".into();
        r.cost_per_use = 1500.0;
    }
    {
        let r = p.resource_mut(qa).ok_or(PlanError::ResourceNotFound(qa))?;
        r.category = ResourceCategory::Team;
        r.group = "Engineering".into();
        r.max_units = 3.0;
        r.standard_rate = 70.0;
    }
    {
        let r = p.resource_mut(travel).ok_or(PlanError::ResourceNotFound(travel))?;
        r.kind = ResourceType::Cost;
        r.category = ResourceCategory::Other;
    }

    let add = |p: &mut Project, name: &str, level: u16, days: f64| -> TaskId {
        let id = p.add_task(name, None);
        if let Some(t) = p.task_mut(id) {
            t.outline_level = level;
            t.duration = (days * day as f64) as i64;
            t.estimated = false;
        }
        id
    };

    let planning = add(&mut p, "Planning", 1, 0.0);
    let scope = add(&mut p, "Define scope and goals", 2, 3.0);
    let research = add(&mut p, "Market research", 2, 5.0);
    let plan_ok = p.add_milestone("Plan approved", None);
    p.task_mut(plan_ok).ok_or(PlanError::TaskNotFound(plan_ok))?.outline_level = 2;

    let design = add(&mut p, "Design", 1, 0.0);
    let ux = add(&mut p, "UX flows and wireframes", 2, 6.0);
    let visual = add(&mut p, "Visual design", 2, 5.0);
    let design_ok = p.add_milestone("Design sign-off", None);
    p.task_mut(design_ok).ok_or(PlanError::TaskNotFound(design_ok))?.outline_level = 2;

    let build = add(&mut p, "Build", 1, 0.0);
    let backend = add(&mut p, "Backend services", 2, 10.0);
    let frontend = add(&mut p, "Frontend application", 2, 12.0);
    let infra = add(&mut p, "Hosting and infrastructure", 2, 4.0);
    let testing = add(&mut p, "Integration testing", 2, 6.0);
    let beta = p.add_milestone("Beta ready", None);
    p.task_mut(beta).ok_or(PlanError::TaskNotFound(beta))?.outline_level = 2;

    let launch = add(&mut p, "Launch", 1, 0.0);
    let marketing = add(&mut p, "Launch campaign", 2, 8.0);
    let event = add(&mut p, "Launch event", 2, 1.0);
    let golive = p.add_milestone("Product live", None);
    p.task_mut(golive).ok_or(PlanError::TaskNotFound(golive))?.outline_level = 2;

    // Links.
    p.link_chain(&[scope, research])?;
    p.add_milestone_members(plan_ok, &[scope.into(), research.into()])?;
    p.link(plan_ok.into(), ux, LinkType::FS, Lag::None)?;
    p.link(ux.into(), visual, LinkType::SS, Lag::Working(3 * day))?;
    p.add_milestone_members(design_ok, &[ux.into(), visual.into()])?;
    p.link(design_ok.into(), backend, LinkType::FS, Lag::None)?;
    p.link(design_ok.into(), frontend, LinkType::FS, Lag::None)?;
    p.link(plan_ok.into(), infra, LinkType::FS, Lag::Working(5 * day))?;
    p.link(backend.into(), testing, LinkType::FS, Lag::None)?;
    p.link(frontend.into(), testing, LinkType::FS, Lag::None)?;
    p.link(infra.into(), testing, LinkType::FS, Lag::None)?;
    p.add_milestone_members(beta, &[testing.into()])?;
    p.link(beta.into(), marketing, LinkType::SS, Lag::Percent(0))?;
    p.link(marketing.into(), event, LinkType::FF, Lag::None)?;
    p.add_milestone_members(golive, &[marketing.into(), event.into()])?;

    p.task_mut(plan_ok).ok_or(PlanError::TaskNotFound(plan_ok))?.deliverable = "Signed project charter".into();
    p.task_mut(design_ok).ok_or(PlanError::TaskNotFound(design_ok))?.deliverable = "Approved UI kit and prototypes".into();
    p.task_mut(beta).ok_or(PlanError::TaskNotFound(beta))?.deliverable = "Feature-complete beta build".into();
    p.task_mut(golive).ok_or(PlanError::TaskNotFound(golive))?.deliverable = "Public release".into();
    p.task_mut(golive).ok_or(PlanError::TaskNotFound(golive))?.deadline =
        p.parse_datetime(&(start + chrono::Duration::days(70)).to_string(), true);
    for id in [plan_ok, design_ok, beta, golive] {
        p.task_mut(id).ok_or(PlanError::TaskNotFound(id))?.show_on_timeline = true;
    }
    p.task_mut(research).ok_or(PlanError::TaskNotFound(research))?.estimate =
        Some(Estimate::ThreePoint { optimistic: 24 * 60, most_likely: 40 * 60, pessimistic: 64 * 60 });
    p.task_mut(backend).ok_or(PlanError::TaskNotFound(backend))?.estimate = Some(Estimate::Complexity { points: 21.0 });
    p.task_mut(frontend).ok_or(PlanError::TaskNotFound(frontend))?.estimate = Some(Estimate::Complexity { points: 26.0 });

    // Assignments.
    p.assign(scope, ana, None, None)?;
    p.assign(research, ana, Some(0.5), None)?;
    p.assign(ux, studio, None, None)?;
    p.assign(ux, ana, Some(0.25), None)?;
    p.assign(visual, studio, None, None)?;
    p.assign(backend, ben, None, None)?;
    p.assign(frontend, chloe, Some(0.8), None)?;
    p.assign(frontend, ben, Some(0.25), None)?;
    p.assign(infra, vendor, None, None)?;
    p.assign(infra, ben, Some(0.5), None)?;
    p.assign(testing, qa, Some(2.0), None)?;
    p.assign(marketing, ana, None, None)?;
    let ev = p.assign(event, travel, None, None)?;
    if let Some(a) = p.assignments.iter_mut().find(|a| a.id == ev) {
        a.cost = 12_000.0;
    }

    recalculate(&mut p);

    // Some progress and tracked time.
    p.set_percent_complete(scope, 100)?;
    let s = p.task(scope).and_then(|t| t.sched.start).map_or(start, |s| s.date());
    for i in 0..3 {
        let _ = p.log_time(scope, ana, s + chrono::Duration::days(i), 8 * 60, "Scoping workshop");
    }
    p.set_percent_complete(research, 40)?;
    let _ = planning;
    let _ = design;
    let _ = build;
    let _ = launch;
    recalculate(&mut p);
    p.set_baseline(&[]);
    recalculate(&mut p);
    Ok(p)
}

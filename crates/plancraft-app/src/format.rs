//! Display formatting helpers.

use chrono::{NaiveDate, NaiveDateTime};
use plancraft_core::prelude::*;

pub fn date(dt: Option<NaiveDateTime>) -> String {
    dt.map(|d| d.format("%a %m/%d/%y").to_string())
        .unwrap_or_else(|| "NA".into())
}

pub fn date_long(dt: Option<NaiveDateTime>) -> String {
    dt.map(|d| d.format("%a %b %-d, %Y %H:%M").to_string())
        .unwrap_or_else(|| "NA".into())
}

pub fn day(d: NaiveDate) -> String {
    d.format("%a %m/%d/%y").to_string()
}

pub fn iso(dt: Option<NaiveDateTime>) -> String {
    dt.map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

pub fn iso_date(dt: Option<NaiveDateTime>) -> String {
    dt.map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

pub fn money(p: &Project, v: f64) -> String {
    let neg = v < 0.0;
    let cents = (v.abs() * 100.0).round() as i64;
    let whole = cents / 100;
    let frac = cents % 100;
    let mut s = whole.to_string();
    let mut i = s.len() as isize - 3;
    while i > 0 {
        s.insert(i as usize, ',');
        i -= 3;
    }
    format!(
        "{}{}{s}.{frac:02}",
        if neg { "-" } else { "" },
        p.currency()
    )
}

pub fn hours(mins: f64) -> String {
    let h = mins / 60.0;
    if (h - h.round()).abs() < 0.05 {
        format!("{}h", h.round() as i64)
    } else {
        format!("{h:.1}h")
    }
}

pub fn percent(v: f32) -> String {
    format!("{}%", v.round() as i64)
}

pub fn units(u: f64) -> String {
    format!("{}%", (u * 100.0).round() as i64)
}

/// Parses "50%", "50", "0.5" into units (fraction).
pub fn parse_units(text: &str) -> Option<f64> {
    let t = text.trim();
    if let Some(p) = t.strip_suffix('%') {
        return p.trim().parse::<f64>().ok().map(|v| v / 100.0);
    }
    let v: f64 = t.parse().ok()?;
    // Bare numbers are percentages ("50" = 50%) unless written as a small fraction ("0.5", "1.5").
    Some(if t.contains('.') && v <= 10.0 {
        v
    } else {
        v / 100.0
    })
}

/// Parses money like "$1,200.50".
pub fn parse_money(text: &str) -> Option<f64> {
    let cleaned: String = text
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    cleaned.parse().ok()
}

pub fn slack(p: &Project, m: Minutes) -> String {
    p.format_duration(m, false)
}

pub fn estimate_label(e: &Option<Estimate>) -> String {
    match e {
        None => String::new(),
        Some(Estimate::Complexity { points }) => format!("{points} pts"),
        Some(Estimate::ThreePoint { .. }) => "3-point".into(),
        Some(Estimate::Work { minutes }) => format_work(*minutes),
    }
}

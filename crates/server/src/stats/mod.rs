//! `/api/stats` — daily-note frontmatter metrics: aggregation, logging API, a
//! per-user display registry, and self-serving docs.
//!
//! Metric data lives in daily-note frontmatter (parsed by
//! [`rust_note_core::stats`]); the registry (units/labels/chart/agg) and
//! timezone live in the user's settings note as flat `stat.<metric>.<field>`
//! keys.

pub mod routes;

use rust_note_core::frontmatter::Frontmatter;
use rust_note_core::stats::AggKind;
use time_tz::OffsetDateTimeExt;

/// Default timezone used to resolve "today" for API writes when the user
/// hasn't set one.
pub const DEFAULT_TZ: &str = "Europe/Warsaw";

/// Allowed chart kinds (display-only hint for the /stats view).
pub const CHART_KINDS: &[&str] = &["line", "bar", "boolean", "heatmap"];

/// A per-user metric display definition, stored as `stat.<metric>.<field>`
/// keys in the settings note.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetricDef {
    pub metric: String,
    pub unit: String,
    pub label: String,
    pub chart: String,
    pub agg: String,
}

impl MetricDef {
    fn with_defaults(metric: &str) -> Self {
        Self {
            metric: metric.to_string(),
            unit: String::new(),
            label: metric.to_string(),
            chart: "line".to_string(),
            agg: "sum".to_string(),
        }
    }
}

/// A metric name is a lowercase dotted identifier: `caffeine`,
/// `exercise.cardio`. Rejects empties, `stat.*` (registry namespace), leading/
/// trailing/doubled dots, and anything outside `[a-z0-9._-]`.
pub fn is_valid_metric(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with("stat.")
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Field suffixes we recognize under `stat.<metric>.`.
const REGISTRY_FIELDS: &[&str] = &["unit", "label", "chart", "agg"];

/// Collect the registry definitions out of parsed settings frontmatter, keyed
/// and returned sorted by metric name.
pub fn parse_registry(fm: &Frontmatter) -> Vec<MetricDef> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, MetricDef> = BTreeMap::new();
    for (key, value) in &fm.fields {
        let Some(rest) = key.strip_prefix("stat.") else {
            continue;
        };
        let Some((metric, field)) = rest.rsplit_once('.') else {
            continue;
        };
        if !REGISTRY_FIELDS.contains(&field) || metric.is_empty() {
            continue;
        }
        let def = map
            .entry(metric.to_string())
            .or_insert_with(|| MetricDef::with_defaults(metric));
        match field {
            "unit" => def.unit = value.clone(),
            "label" => def.label = value.clone(),
            "chart" => def.chart = value.clone(),
            "agg" => def.agg = value.clone(),
            _ => {}
        }
    }
    map.into_values().collect()
}

/// The user's configured timezone from settings frontmatter, or [`DEFAULT_TZ`].
pub fn timezone_of(fm: &Frontmatter) -> String {
    fm.get("timezone")
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_TZ)
        .to_string()
}

/// Resolve `AggKind` from a registry string, defaulting to `Sum`.
pub fn agg_of(def: &MetricDef) -> AggKind {
    AggKind::parse(&def.agg).unwrap_or_default()
}

/// Today's civil date in `tz_name` (falling back to UTC for an unknown zone).
pub fn today_in_tz(tz_name: &str) -> time::Date {
    let tz = time_tz::timezones::get_by_name(tz_name).unwrap_or(time_tz::timezones::db::UTC);
    time::OffsetDateTime::now_utc().to_timezone(tz).date()
}

/// Parse a `YYYY-MM-DD` string into a `time::Date`.
pub fn parse_date(s: &str) -> Option<time::Date> {
    if s.len() != 10 {
        return None;
    }
    let b = s.as_bytes();
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u8 = s.get(5..7)?.parse().ok()?;
    let day: u8 = s.get(8..10)?.parse().ok()?;
    time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()
}

/// Format a `time::Date` as `YYYY-MM-DD`.
pub fn fmt_date(d: time::Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_metric_names() {
        assert!(is_valid_metric("caffeine"));
        assert!(is_valid_metric("exercise.cardio"));
        assert!(is_valid_metric("blood_pressure-1"));
        assert!(!is_valid_metric(""));
        assert!(!is_valid_metric("stat.caffeine")); // registry namespace
        assert!(!is_valid_metric(".x"));
        assert!(!is_valid_metric("x."));
        assert!(!is_valid_metric("a..b"));
        assert!(!is_valid_metric("Caffeine")); // uppercase
        assert!(!is_valid_metric("a b")); // space
    }

    #[test]
    fn registry_round_trip_from_frontmatter() {
        let content = "---\ntheme: ration\nstat.caffeine.unit: mg\nstat.caffeine.label: Caffeine\nstat.caffeine.chart: line\nstat.caffeine.agg: sum\nstat.exercise.cardio.unit: min\ntimezone: Europe/Warsaw\n---\n";
        let fm = Frontmatter::parse(content);
        let defs = parse_registry(&fm);
        assert_eq!(defs.len(), 2);
        // sorted by metric; exercise.cardio before caffeine? 'c' < 'e' → caffeine first
        assert_eq!(defs[0].metric, "caffeine");
        assert_eq!(defs[0].unit, "mg");
        assert_eq!(defs[0].label, "Caffeine");
        assert_eq!(defs[0].agg, "sum");
        // dotted metric parsed via last-segment field split
        assert_eq!(defs[1].metric, "exercise.cardio");
        assert_eq!(defs[1].unit, "min");
        assert_eq!(defs[1].label, "exercise.cardio"); // default when unset
        assert_eq!(timezone_of(&fm), "Europe/Warsaw");
    }

    #[test]
    fn date_helpers() {
        let d = parse_date("2026-09-01").unwrap();
        assert_eq!(fmt_date(d), "2026-09-01");
        assert!(parse_date("2026-9-1").is_none());
        assert!(parse_date("nonsense").is_none());
    }
}

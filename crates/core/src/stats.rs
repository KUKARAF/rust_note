//! Parsing daily-note frontmatter "stats" — arbitrary tracked metrics such as
//! `protein: 60`, timed events `caffeine: 40@0720`, durations
//! `exercise.cardio: 30@0930`, and flags `exercise: true`.
//!
//! Hand-rolled and dependency-light (like [`crate::tasks`]) so `core` stays
//! usable from server and mobile. It never errors: anything it doesn't
//! understand is skipped.
//!
//! # Canonical on-disk format
//! Flat, dotted, literal frontmatter keys with one of these value shapes:
//! - integer — `protein: 60`
//! - boolean flag — `exercise: true`
//! - timed integer — `caffeine: 40@0720` (value `40` at 07:20; `@HHMM`, 24h)
//! - inline list — `caffeine: [40@0720, 30@1500]`
//!
//! No floats. [`append_stat_entry`] always writes this inline form.
//!
//! # Legacy read compatibility
//! [`read_stats`] also understands the older block-list written by the web
//! "track value" dialog, so existing vault data still shows up:
//! ```text
//! caffeine:
//!   - value: 40
//!     time: "07:20"
//! ```
//! Such a metric is migrated to the inline form on its next [`append_stat_entry`].

use serde::{Deserialize, Serialize};

/// One numeric sample of a metric, optionally stamped with a time of day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatPoint {
    pub value: i64,
    /// Normalized `"HH:MM"` (24h), or `None` for an untimed sample.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub at: Option<String>,
}

/// A metric's parsed value: either a boolean flag or one-or-more numeric points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatValue {
    Bool(bool),
    Nums(Vec<StatPoint>),
}

/// How to reduce a day's multiple points into one number (from the registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AggKind {
    #[default]
    Sum,
    Last,
    Max,
    Min,
    Count,
}

impl AggKind {
    /// Parse a registry `agg` string; unknown values fall back to `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sum" => Some(Self::Sum),
            "last" => Some(Self::Last),
            "max" => Some(Self::Max),
            "min" => Some(Self::Min),
            "count" => Some(Self::Count),
            _ => None,
        }
    }
}

/// Reduce points to a single value per [`AggKind`]. Empty input yields 0.
pub fn aggregate(points: &[StatPoint], kind: AggKind) -> i64 {
    match kind {
        AggKind::Sum => points.iter().map(|p| p.value).sum(),
        AggKind::Last => points.last().map_or(0, |p| p.value),
        AggKind::Max => points.iter().map(|p| p.value).max().unwrap_or(0),
        AggKind::Min => points.iter().map(|p| p.value).min().unwrap_or(0),
        AggKind::Count => points.len() as i64,
    }
}

/// Normalize a time token to `"HH:MM"`. Accepts `"HHMM"` (canonical) and
/// `"HH:MM"` (legacy). Returns `None` if out of range or malformed.
fn parse_hhmm(raw: &str) -> Option<String> {
    let s = raw.trim().trim_matches('"');
    let (h, m) = if let Some((h, m)) = s.split_once(':') {
        (h, m)
    } else if s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()) {
        (&s[..2], &s[2..])
    } else {
        return None;
    };
    if m.len() != 2 || h.is_empty() || h.len() > 2 {
        return None;
    }
    let hh: u32 = h.parse().ok()?;
    let mm: u32 = m.parse().ok()?;
    if hh < 24 && mm < 60 {
        Some(format!("{hh:02}:{mm:02}"))
    } else {
        None
    }
}

/// Parse one numeric token: `N` or `N@HHMM`. Rejects floats (non-integer).
fn parse_point(tok: &str) -> Option<StatPoint> {
    let tok = tok.trim();
    if let Some((num, time)) = tok.split_once('@') {
        Some(StatPoint {
            value: num.trim().parse().ok()?,
            at: Some(parse_hhmm(time)?),
        })
    } else {
        Some(StatPoint {
            value: tok.parse().ok()?,
            at: None,
        })
    }
}

/// Parse the inline value of a frontmatter stat line. `None` for anything not
/// a bool / integer / `int@HHMM` / inline list of those (e.g. floats, strings).
pub fn parse_stat_value(raw: &str) -> Option<StatValue> {
    let raw = raw.trim();
    match raw {
        "" => return None,
        "true" => return Some(StatValue::Bool(true)),
        "false" => return Some(StatValue::Bool(false)),
        _ => {}
    }
    if let Some(inner) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let mut pts = Vec::new();
        for elem in inner.split(',') {
            if elem.trim().is_empty() {
                continue;
            }
            pts.push(parse_point(elem)?);
        }
        return if pts.is_empty() {
            None
        } else {
            Some(StatValue::Nums(pts))
        };
    }
    parse_point(raw).map(|p| StatValue::Nums(vec![p]))
}

/// Return the lines between the leading `---` fences, or `None` when the doc
/// has no well-formed frontmatter block.
fn frontmatter_lines(doc: &str) -> Option<Vec<&str>> {
    let rest = doc.strip_prefix("---\n")?;
    let mut lines = Vec::new();
    for line in rest.split('\n') {
        if line == "---" {
            return Some(lines);
        }
        lines.push(line);
    }
    None
}

/// Read every stat we understand from the doc's frontmatter, in order.
/// Handles the canonical inline form and the legacy block-list form.
pub fn read_stats(doc: &str) -> Vec<(String, StatValue)> {
    let Some(lines) = frontmatter_lines(doc) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(&line) = lines.get(i) else { break };
        // Only column-0 lines are keys; indented lines belong to a block list
        // and are consumed by the inner loop below.
        if line.starts_with(char::is_whitespace) {
            i += 1;
            continue;
        }
        let Some((key, rest)) = line.split_once(':') else {
            i += 1;
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            i += 1;
            continue;
        }
        let inline = rest.trim();
        if !inline.is_empty() {
            if let Some(v) = parse_stat_value(inline) {
                out.push((key.to_string(), v));
            }
            i += 1;
            continue;
        }
        // Empty inline value: gather legacy `- value:` / `time:` block entries.
        let (points, next) = read_block_entries(&lines, i + 1);
        if !points.is_empty() {
            out.push((key.to_string(), StatValue::Nums(points)));
        }
        i = next.max(i + 1);
    }
    out
}

/// From `start`, consume indented legacy block entries and return the points
/// plus the index of the first line that isn't part of the block.
fn read_block_entries(lines: &[&str], start: usize) -> (Vec<StatPoint>, usize) {
    let mut points = Vec::new();
    let mut j = start;
    while j < lines.len() {
        let Some(&el) = lines.get(j) else { break };
        if !el.starts_with(char::is_whitespace) {
            break;
        }
        let Some(after) = el.trim_start().strip_prefix("- value:") else {
            break;
        };
        let Ok(value) = after.trim().parse::<i64>() else {
            break;
        };
        let mut at = None;
        if let Some(next) = lines.get(j + 1) {
            if next.starts_with(char::is_whitespace) {
                if let Some(tv) = next.trim_start().strip_prefix("time:") {
                    at = parse_hhmm(tv);
                    if at.is_some() {
                        j += 1;
                    }
                }
            }
        }
        points.push(StatPoint { value, at });
        j += 1;
    }
    (points, j)
}

/// Render one point back to inline form: `N` or `N@HHMM`.
fn render_point(p: &StatPoint) -> String {
    match &p.at {
        Some(t) => format!("{}@{}", p.value, t.replace(':', "")),
        None => p.value.to_string(),
    }
}

/// Render a `key: value` line from a point list (scalar when single).
fn render_line(key: &str, points: &[StatPoint]) -> String {
    match points {
        [single] => format!("{key}: {}", render_point(single)),
        _ => {
            let inner = points
                .iter()
                .map(render_point)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{key}: [{inner}]")
        }
    }
}

/// The frontmatter block's byte extent and its lines.
struct Block {
    /// Byte offset of the first content line (just after the opening fence).
    content_start: usize,
    /// Byte offset of the closing `---` line.
    fence_start: usize,
    lines: Vec<String>,
}

fn locate_block(doc: &str) -> Option<Block> {
    if !doc.starts_with("---\n") {
        return None;
    }
    let content_start = 4;
    let mut offset = content_start;
    loop {
        let line_end = doc[offset..].find('\n').map(|p| offset + p);
        let line = match line_end {
            Some(e) => &doc[offset..e],
            None => &doc[offset..],
        };
        if line == "---" {
            let lines = if offset == content_start {
                Vec::new()
            } else {
                doc[content_start..offset - 1]
                    .split('\n')
                    .map(str::to_string)
                    .collect()
            };
            return Some(Block {
                content_start,
                fence_start: offset,
                lines,
            });
        }
        match line_end {
            Some(e) => offset = e + 1,
            None => return None, // no closing fence
        }
    }
}

/// Append one numeric sample to `key` in the doc's frontmatter, returning the
/// full new doc text (the caller applies it to the collab doc as one edit).
/// Always writes the canonical inline form:
/// - no frontmatter → a new block is created at the top;
/// - key absent → appended before the closing fence;
/// - key present as inline scalar/list or legacy block → migrated to an inline
///   list with the new point appended.
///
/// Returns `None` if `at` is a malformed time, the frontmatter is unclosed, or
/// the key currently holds a non-numeric value (e.g. `exercise: true`).
pub fn append_stat_entry(doc: &str, key: &str, value: i64, at: Option<&str>) -> Option<String> {
    let at = match at {
        Some(a) => Some(parse_hhmm(a)?),
        None => None,
    };
    let new_point = StatPoint { value, at };

    if !doc.starts_with("---") {
        let block = format!("---\n{}\n---\n", render_line(key, &[new_point]));
        return Some(format!("{block}{doc}"));
    }

    let block = locate_block(doc)?;
    let mut lines = block.lines;

    // Find the key's column-0 line.
    let key_idx = lines.iter().position(|l| {
        !l.starts_with(char::is_whitespace)
            && l.split_once(':').is_some_and(|(k, _)| k.trim() == key)
    });

    match key_idx {
        None => {
            lines.push(render_line(key, &[new_point]));
        }
        Some(idx) => {
            let (_, rest) = lines.get(idx)?.split_once(':')?;
            let inline = rest.trim().to_string();
            let mut points;
            let extent_end;
            if inline.is_empty() {
                // Legacy block form: read + replace its entry lines.
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                let (pts, next) = read_block_entries(&refs, idx + 1);
                points = pts;
                extent_end = next;
            } else {
                match parse_stat_value(&inline) {
                    Some(StatValue::Nums(pts)) => points = pts,
                    // Bool or unparseable: refuse rather than corrupt.
                    _ => return None,
                }
                extent_end = idx + 1;
            }
            points.push(new_point);
            let new_line = render_line(key, &points);
            lines.splice(idx..extent_end, std::iter::once(new_line));
        }
    }

    let before = &doc[..block.content_start];
    let after = &doc[block.fence_start..];
    let middle = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    Some(format!("{before}{middle}{after}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nums(v: &[(i64, Option<&str>)]) -> StatValue {
        StatValue::Nums(
            v.iter()
                .map(|(val, at)| StatPoint {
                    value: *val,
                    at: at.map(|s| s.to_string()),
                })
                .collect(),
        )
    }

    #[test]
    fn parse_scalar_bool_timed_and_list() {
        assert_eq!(parse_stat_value("60"), Some(nums(&[(60, None)])));
        assert_eq!(parse_stat_value("true"), Some(StatValue::Bool(true)));
        assert_eq!(parse_stat_value("false"), Some(StatValue::Bool(false)));
        assert_eq!(
            parse_stat_value("40@0720"),
            Some(nums(&[(40, Some("07:20"))]))
        );
        assert_eq!(
            parse_stat_value("[40@0720, 30@1500]"),
            Some(nums(&[(40, Some("07:20")), (30, Some("15:00"))]))
        );
        // Floats and junk are ignored (not stats).
        assert_eq!(parse_stat_value("72.5"), None);
        assert_eq!(parse_stat_value("great"), None);
        // Out-of-range time rejects the whole token.
        assert_eq!(parse_stat_value("10@2599"), None);
    }

    #[test]
    fn read_inline_and_dotted_keys() {
        let doc = "---\nprotein: 60\ncaffeine: [40@0720, 30@1500]\nexercise.cardio: 30@0930\nexercise: true\n---\nbody\n";
        let stats = read_stats(doc);
        assert_eq!(stats[0], ("protein".to_string(), nums(&[(60, None)])));
        assert_eq!(
            stats[1],
            (
                "caffeine".to_string(),
                nums(&[(40, Some("07:20")), (30, Some("15:00"))])
            )
        );
        assert_eq!(
            stats[2],
            ("exercise.cardio".to_string(), nums(&[(30, Some("09:30"))]))
        );
        assert_eq!(stats[3], ("exercise".to_string(), StatValue::Bool(true)));
    }

    #[test]
    fn read_legacy_block_list() {
        let doc = "---\ncalories:\n  - value: 300\n    time: \"09:30\"\n  - value: 500\n---\n";
        let stats = read_stats(doc);
        assert_eq!(
            stats,
            vec![(
                "calories".to_string(),
                nums(&[(300, Some("09:30")), (500, None)])
            )]
        );
    }

    #[test]
    fn aggregate_kinds() {
        let pts = [
            StatPoint {
                value: 40,
                at: None,
            },
            StatPoint {
                value: 30,
                at: None,
            },
            StatPoint {
                value: 10,
                at: None,
            },
        ];
        assert_eq!(aggregate(&pts, AggKind::Sum), 80);
        assert_eq!(aggregate(&pts, AggKind::Last), 10);
        assert_eq!(aggregate(&pts, AggKind::Max), 40);
        assert_eq!(aggregate(&pts, AggKind::Min), 10);
        assert_eq!(aggregate(&pts, AggKind::Count), 3);
        assert_eq!(aggregate(&[], AggKind::Sum), 0);
    }

    #[test]
    fn append_creates_block_when_absent() {
        let out = append_stat_entry("body text\n", "caffeine", 40, Some("0720")).unwrap();
        assert_eq!(out, "---\ncaffeine: 40@0720\n---\nbody text\n");
    }

    #[test]
    fn append_new_key_before_fence() {
        let doc = "---\nprotein: 60\n---\nbody\n";
        let out = append_stat_entry(doc, "caffeine", 40, None).unwrap();
        assert_eq!(out, "---\nprotein: 60\ncaffeine: 40\n---\nbody\n");
    }

    #[test]
    fn append_scalar_migrates_to_inline_list() {
        let doc = "---\ncaffeine: 40@0720\n---\nbody\n";
        let out = append_stat_entry(doc, "caffeine", 30, Some("1500")).unwrap();
        assert_eq!(out, "---\ncaffeine: [40@0720, 30@1500]\n---\nbody\n");
    }

    #[test]
    fn append_migrates_legacy_block_to_inline() {
        let doc = "---\ncaffeine:\n  - value: 40\n    time: \"07:20\"\n---\nbody\n";
        let out = append_stat_entry(doc, "caffeine", 30, None).unwrap();
        assert_eq!(out, "---\ncaffeine: [40@0720, 30]\n---\nbody\n");
    }

    #[test]
    fn append_refuses_bool_key() {
        let doc = "---\nexercise: true\n---\n";
        assert_eq!(append_stat_entry(doc, "exercise", 1, None), None);
    }

    #[test]
    fn append_rejects_bad_time() {
        assert_eq!(append_stat_entry("x", "caffeine", 40, Some("2599")), None);
    }
}

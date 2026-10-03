//! A typed view over a "pipeline tracker" note's flat frontmatter.
//!
//! A pipeline item is one git-backed markdown note under `pipeline/` whose
//! frontmatter captures the "whose-court-is-the-ball-in" state of an
//! opportunity — either a **lead** (a gig Rafał could do) or an
//! **application** (Rafał applied somewhere as a candidate). The body holds a
//! freeform reverse-chronological timeline; git history is the event log.
//!
//! Like [`crate::tasks`] and [`crate::stats`], this is a never-panic typed
//! parser layered over [`crate::frontmatter`]: every field except `kind` and
//! `company` is best-effort, and absence or malformed input degrades to
//! `None`/empty rather than erroring. [`PipelineItem::from_frontmatter`]
//! returns `None` only when the note isn't a pipeline item at all (no valid
//! `kind`, or an empty `company`).
//!
//! The on-disk conventions (documented in `docs/pipeline.md`): lists are
//! comma-delimited strings, timestamps are RFC3339 text, dates are
//! `YYYY-MM-DD`, and `priority` is an integer 1–5.

use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::frontmatter::Frontmatter;

/// Which tracker a note belongs to. Serialized lowercase (`"lead"` /
/// `"application"`), matching the on-disk `kind:` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Lead,
    Application,
}

impl Kind {
    /// Parse the `kind` frontmatter value (case-insensitive). Unknown values
    /// yield `None`, which makes the whole item skippable.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lead" => Some(Self::Lead),
            "application" => Some(Self::Application),
            _ => None,
        }
    }
}

/// Whose court the next move is in. Serialized lowercase; `none` means the
/// item is closed or dormant and is also the default for a missing/unknown
/// `ball:` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Ball {
    Ours,
    Theirs,
    #[default]
    None,
}

impl Ball {
    /// Parse the `ball` frontmatter value (case-insensitive). Anything that
    /// isn't `ours`/`theirs` (including `none` and malformed input) is
    /// [`Ball::None`].
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "ours" => Self::Ours,
            "theirs" => Self::Theirs,
            _ => Self::None,
        }
    }
}

/// One entry in a pipeline item's append-only `stage_history`: the stage the
/// item moved *to* and the instant the server observed that move. Stored
/// on-disk as `"<stage>@<rfc3339>"` inside the comma-list `stage_history` key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageEntry {
    pub stage: String,
    /// Parsed RFC3339; serialized back as RFC3339.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// A typed pipeline item parsed from a note's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PipelineItem {
    /// Note id, e.g. `pipeline/affirm-senior-swe-backend`.
    pub id: String,
    pub kind: Kind,
    pub company: String,
    pub role: Option<String>,
    pub stage: Option<String>,
    pub ball: Ball,
    /// Parsed RFC3339; unparseable input degrades to `None`.
    #[serde(with = "time::serde::rfc3339::option")]
    pub expected_at: Option<OffsetDateTime>,
    pub next_action: Option<String>,
    /// `contact` comma-list, trimmed, empties dropped.
    pub contacts: Vec<String>,
    /// Parsed `1..=5`; out-of-range or non-integer degrades to `None`.
    pub priority: Option<u8>,
    pub closed_reason: Option<String>,
    /// Append-only, chronological log of stage transitions parsed from the
    /// `stage_history` comma-list. Malformed entries are dropped; file order
    /// is preserved.
    pub stage_history: Vec<StageEntry>,
}

/// Terminal stages: reaching one means the item is closed and the ball should
/// be `none`. Matched case-insensitively.
const TERMINAL_STAGES: &[&str] = &["closed", "won", "lost", "rejected", "withdrawn", "accepted"];

impl PipelineItem {
    /// Build a typed item from a note `id` and its parsed [`Frontmatter`].
    ///
    /// Returns `None` only when the note isn't a pipeline item: `kind` is
    /// missing/unknown, or `company` is missing/empty. Every other field is
    /// best-effort and never errors.
    pub fn from_frontmatter(id: &str, fm: &Frontmatter) -> Option<Self> {
        let kind = Kind::parse(fm.get("kind")?)?;
        let company = fm
            .get("company")
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();

        Some(Self {
            id: id.to_string(),
            kind,
            company,
            role: opt_string(fm, "role"),
            stage: opt_string(fm, "stage"),
            ball: fm.get("ball").map(Ball::parse).unwrap_or_default(),
            expected_at: fm.get("expected_at").and_then(parse_rfc3339),
            next_action: opt_string(fm, "next_action"),
            contacts: fm.get("contact").map(split_list).unwrap_or_default(),
            priority: fm
                .get("priority")
                .and_then(|s| s.trim().parse::<u8>().ok())
                .filter(|n| (1..=5).contains(n)),
            closed_reason: opt_string(fm, "closed_reason"),
            stage_history: fm
                .get("stage_history")
                .map(parse_stage_history)
                .unwrap_or_default(),
        })
    }

    /// Whether this item is closed: its `stage` is a terminal stage
    /// (case-insensitive), or the ball is in nobody's court ([`Ball::None`]).
    pub fn is_closed(&self) -> bool {
        if self.ball == Ball::None {
            return true;
        }
        self.stage
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .is_some_and(|s| TERMINAL_STAGES.contains(&s.as_str()))
    }
}

/// A trimmed, non-empty frontmatter value, or `None`.
fn opt_string(fm: &Frontmatter, key: &str) -> Option<String> {
    fm.get(key)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Split a comma-list value: trimmed, empties dropped.
fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse an RFC3339 timestamp; malformed input yields `None`.
fn parse_rfc3339(raw: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(raw.trim(), &Rfc3339).ok()
}

/// Parse the `stage_history` comma-list into ordered [`StageEntry`]s.
///
/// Each element is `"<stage>@<rfc3339>"`; it's split on the LAST `@` so a
/// stage name may itself contain `@`. Entries with an empty stage, no `@`, or
/// an unparseable timestamp are dropped (best-effort, consistent with the rest
/// of this parser). File order is preserved.
fn parse_stage_history(raw: &str) -> Vec<StageEntry> {
    split_list(raw)
        .into_iter()
        .filter_map(|entry| {
            let (stage, ts) = entry.rsplit_once('@')?;
            let stage = stage.trim();
            if stage.is_empty() {
                return None;
            }
            let at = parse_rfc3339(ts)?;
            Some(StageEntry {
                stage: stage.to_string(),
                at,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "---\n\
kind: application\n\
company: Affirm\n\
role: Senior SWE, Back-end\n\
stage: interview\n\
ball: theirs\n\
expected_at: 2026-10-08T12:00:00Z\n\
next_action: await reschedule\n\
contact: Paulina Syberska <p@affirm.com> (coordinator), Aakash <a@affirm.com>\n\
priority: 4\n\
closed_reason: ghosted\n\
stage_history: \"lead@2026-09-14T09:00:00Z, interview@2026-09-20T14:00:00Z\"\n\
---\n\
# Affirm\n\n## Timeline\n- 2026-09-16 — Applied.\n";

    fn parse(content: &str) -> Option<PipelineItem> {
        let fm = Frontmatter::parse(content);
        PipelineItem::from_frontmatter("pipeline/affirm-senior-swe-backend", &fm)
    }

    #[test]
    fn full_frontmatter_parses_every_field() {
        let item = parse(FULL).expect("full frontmatter is a valid item");
        assert_eq!(item.id, "pipeline/affirm-senior-swe-backend");
        assert_eq!(item.kind, Kind::Application);
        assert_eq!(item.company, "Affirm");
        assert_eq!(item.role.as_deref(), Some("Senior SWE, Back-end"));
        assert_eq!(item.stage.as_deref(), Some("interview"));
        assert_eq!(item.ball, Ball::Theirs);
        assert!(item.expected_at.is_some());
        assert_eq!(item.next_action.as_deref(), Some("await reschedule"));
        assert_eq!(
            item.contacts,
            vec![
                "Paulina Syberska <p@affirm.com> (coordinator)".to_string(),
                "Aakash <a@affirm.com>".to_string(),
            ]
        );
        assert_eq!(item.priority, Some(4));
        assert_eq!(item.closed_reason.as_deref(), Some("ghosted"));
        assert_eq!(item.stage_history.len(), 2);
        assert_eq!(item.stage_history[0].stage, "lead");
        assert_eq!(item.stage_history[1].stage, "interview");
        // ball=theirs, not a terminal stage → open.
        assert!(!item.is_closed());
    }

    #[test]
    fn missing_kind_is_none() {
        assert!(parse("---\ncompany: Acme\n---\nbody\n").is_none());
    }

    #[test]
    fn unknown_kind_is_none() {
        assert!(parse("---\nkind: wishlist\ncompany: Acme\n---\n").is_none());
    }

    #[test]
    fn empty_or_missing_company_is_none() {
        assert!(parse("---\nkind: lead\n---\n").is_none());
        // Present but blank/whitespace company is still "missing".
        assert!(parse("---\nkind: lead\ncompany: \"   \"\n---\n").is_none());
    }

    #[test]
    fn minimal_item_fills_defaults() {
        let item = parse("---\nkind: lead\ncompany: Acme\n---\n").expect("minimal is valid");
        assert_eq!(item.kind, Kind::Lead);
        assert_eq!(item.company, "Acme");
        assert_eq!(item.ball, Ball::None);
        assert!(item.role.is_none());
        assert!(item.expected_at.is_none());
        assert!(item.contacts.is_empty());
        assert!(item.priority.is_none());
        assert!(item.stage_history.is_empty());
        // ball defaults to None → closed.
        assert!(item.is_closed());
    }

    #[test]
    fn comma_lists_are_split_trimmed_and_empties_dropped() {
        let item = parse("---\nkind: lead\ncompany: Acme\ncontact: A <a@x>, , B <b@x>\n---\n")
            .expect("valid");
        assert_eq!(
            item.contacts,
            vec!["A <a@x>".to_string(), "B <b@x>".to_string()]
        );
    }

    #[test]
    fn bad_rfc3339_timestamp_degrades_to_none() {
        let item = parse("---\nkind: lead\ncompany: Acme\nexpected_at: not-a-date\n---\n")
            .expect("item still valid even with bad timestamps");
        assert!(item.expected_at.is_none());
    }

    #[test]
    fn priority_out_of_range_or_non_integer_is_none() {
        let hi = parse("---\nkind: lead\ncompany: Acme\npriority: 9\n---\n").expect("valid");
        assert!(hi.priority.is_none());
        let zero = parse("---\nkind: lead\ncompany: Acme\npriority: 0\n---\n").expect("valid");
        assert!(zero.priority.is_none());
        let nan = parse("---\nkind: lead\ncompany: Acme\npriority: high\n---\n").expect("valid");
        assert!(nan.priority.is_none());
        let ok = parse("---\nkind: lead\ncompany: Acme\npriority: 1\n---\n").expect("valid");
        assert_eq!(ok.priority, Some(1));
        let ok5 = parse("---\nkind: lead\ncompany: Acme\npriority: 5\n---\n").expect("valid");
        assert_eq!(ok5.priority, Some(5));
    }

    #[test]
    fn is_closed_on_terminal_stage_even_when_ball_in_play() {
        // Case-insensitive terminal stage closes the item regardless of ball.
        let item = parse("---\nkind: lead\ncompany: Acme\nstage: WON\nball: theirs\n---\n")
            .expect("valid");
        assert!(item.is_closed());
        // A non-terminal stage with the ball in play stays open.
        let open = parse("---\nkind: lead\ncompany: Acme\nstage: proposal\nball: ours\n---\n")
            .expect("valid");
        assert!(!open.is_closed());
    }

    #[test]
    fn stage_history_parses_multiple_entries_in_file_order() {
        let item = parse(
            "---\nkind: lead\ncompany: Acme\nstage_history: \"lead@2026-09-14T09:00:00Z, interview@2026-09-20T14:00:00Z, offer@2026-09-28T10:00:00Z\"\n---\n",
        )
        .expect("valid");
        let stages: Vec<&str> = item
            .stage_history
            .iter()
            .map(|e| e.stage.as_str())
            .collect();
        assert_eq!(stages, vec!["lead", "interview", "offer"]);
        // Timestamps parsed and kept in file order (chronological here).
        assert!(item.stage_history[0].at < item.stage_history[1].at);
        assert!(item.stage_history[1].at < item.stage_history[2].at);
    }

    #[test]
    fn stage_history_drops_malformed_entries_but_keeps_good_ones() {
        let item = parse(
            "---\nkind: lead\ncompany: Acme\nstage_history: \"lead@2026-09-14T09:00:00Z, broken@not-a-date, @2026-09-20T14:00:00Z, offer@2026-09-28T10:00:00Z\"\n---\n",
        )
        .expect("valid");
        // `broken@not-a-date` (bad timestamp) and `@...` (empty stage) dropped.
        let stages: Vec<&str> = item
            .stage_history
            .iter()
            .map(|e| e.stage.as_str())
            .collect();
        assert_eq!(stages, vec!["lead", "offer"]);
    }

    #[test]
    fn stage_history_entry_without_at_is_skipped() {
        let item = parse(
            "---\nkind: lead\ncompany: Acme\nstage_history: \"nostamp, lead@2026-09-14T09:00:00Z\"\n---\n",
        )
        .expect("valid");
        let stages: Vec<&str> = item
            .stage_history
            .iter()
            .map(|e| e.stage.as_str())
            .collect();
        assert_eq!(stages, vec!["lead"]);
    }

    #[test]
    fn stage_history_empty_or_missing_is_empty_vec() {
        let missing = parse("---\nkind: lead\ncompany: Acme\n---\n").expect("valid");
        assert!(missing.stage_history.is_empty());
        let empty =
            parse("---\nkind: lead\ncompany: Acme\nstage_history: \"\"\n---\n").expect("valid");
        assert!(empty.stage_history.is_empty());
    }

    #[test]
    fn stage_history_serializes_entries_with_rfc3339_timestamps() {
        let item = parse(
            "---\nkind: lead\ncompany: Acme\nstage_history: \"lead@2026-09-14T09:00:00Z, offer@2026-09-28T10:00:00Z\"\n---\n",
        )
        .expect("valid");
        let json = serde_json::to_value(&item).expect("serializes");
        let hist = &json["stage_history"];
        assert_eq!(hist[0]["stage"], "lead");
        assert_eq!(hist[0]["at"], "2026-09-14T09:00:00Z");
        assert_eq!(hist[1]["stage"], "offer");
        assert_eq!(hist[1]["at"], "2026-09-28T10:00:00Z");
    }

    #[test]
    fn serializes_enums_lowercase_and_timestamps_as_rfc3339() {
        let item = parse(FULL).expect("valid");
        let json = serde_json::to_value(&item).expect("serializes");
        assert_eq!(json["kind"], "application");
        assert_eq!(json["ball"], "theirs");
        assert_eq!(json["expected_at"], "2026-10-08T12:00:00Z");
        // A None timestamp serializes as JSON null.
        let minimal = parse("---\nkind: lead\ncompany: Acme\n---\n").expect("valid");
        let json = serde_json::to_value(&minimal).expect("serializes");
        assert_eq!(json["kind"], "lead");
        assert_eq!(json["ball"], "none");
        assert!(json["expected_at"].is_null());
    }
}

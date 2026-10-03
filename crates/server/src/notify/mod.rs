//! Pipeline overdue → priority-notify digest (see `docs/notifications.md`).
//!
//! A background scheduler (spawned from `main.rs`) ticks every 60s and, for
//! each user who has the digest enabled, decides via the user's RRULE whether
//! it is time to fire. When it is, it collects the user's overdue pipeline
//! items (filtered by scope), builds ONE digest, and POSTs it to the user's
//! self-hosted priority-notify server. `notify_last_sent` is stamped after
//! every fire so a scheduled occurrence fires at most once.
//!
//! The same digest machinery backs the `POST /api/settings/notify-test`
//! handler ([`routes`]), which runs it on demand ignoring the schedule.
//!
//! SECRET SAFETY: the priority-notify token is never logged and never
//! serialized into any response.

pub mod routes;

use std::time::Duration;

use anyhow::anyhow;
use rrule::{RRule, Tz, Unvalidated};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use rust_note_core::leads::Ball;

use crate::error::AppResult;
use crate::pipeline::routes::{collect_pipeline, PipelineEntry};
use crate::settings::store::{self, UserSettings};
use crate::state::AppState;
use crate::stats::DEFAULT_TZ;

/// Deep-link appended to every digest message.
const PIPELINE_LINK: &str = "→ https://notes.osmosis.page/pipeline";
/// Title used when a run finds nothing overdue (test endpoint; no POST sent).
const NOTHING_OVERDUE_TITLE: &str = "Nothing overdue";
/// If `notify_last_sent` is empty, look back this far so a brand-new config
/// doesn't instantly replay old occurrences but still fires the next one soon.
const FRESH_LOOKBACK_HOURS: u64 = 25;
/// Scheduler tick period.
const TICK_SECS: u64 = 60;
/// Upper bound on occurrences examined per fire decision (plenty for the
/// ~25h windows this uses; bounds the RRULE iterator).
const OCCURRENCE_LIMIT: u16 = 366;

// ---- scope -----------------------------------------------------------------

/// Which overdue items a user wants digested, by whose court the ball is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Ours,
    Theirs,
    Both,
}

impl Scope {
    /// Parse a `notify_scope` string; unknown values fall back to [`Scope::Both`].
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim() {
            "ours" => Self::Ours,
            "theirs" => Self::Theirs,
            _ => Self::Both,
        }
    }

    /// Whether an item whose ball is `ball` is in scope. `Ball::None` never
    /// matches (a closed/dormant item is never digested).
    pub(crate) fn matches(self, ball: Ball) -> bool {
        match self {
            Self::Ours => ball == Ball::Ours,
            Self::Theirs => ball == Ball::Theirs,
            Self::Both => matches!(ball, Ball::Ours | Ball::Theirs),
        }
    }
}

// ---- validation (used by `PUT /api/settings`) ------------------------------

/// Whether `s` is a valid RRULE accepted by the `rrule` crate (grammar AND
/// semantics — it must be buildable against a reference start date, so e.g. a
/// daily rule missing a time component is rejected).
pub fn is_valid_rrule(s: &str) -> bool {
    let Some(dt_start) = chrono::DateTime::from_timestamp(1_577_836_800, 0) else {
        return false;
    };
    let dt_start = dt_start.with_timezone(&Tz::UTC);
    let Ok(rrule) = s.trim().parse::<RRule<Unvalidated>>() else {
        return false;
    };
    rrule.build(dt_start).is_ok()
}

/// Whether `s` parses as an http(s) URL with a host. Uses reqwest's re-exported
/// `url::Url` (no new dependency).
pub fn is_valid_endpoint(s: &str) -> bool {
    match reqwest::Url::parse(s.trim()) {
        Ok(u) => matches!(u.scheme(), "http" | "https") && u.has_host(),
        Err(_) => false,
    }
}

// ---- RRULE fire decision ---------------------------------------------------

/// Convert a UTC [`OffsetDateTime`] into a chrono datetime in `tz`.
fn to_tz(ts: OffsetDateTime, tz: Tz) -> Option<chrono::DateTime<Tz>> {
    let utc = chrono::DateTime::from_timestamp(ts.unix_timestamp(), ts.nanosecond())?;
    Some(utc.with_timezone(&tz))
}

/// The effective lower bound of the fire window: the parsed `notify_last_sent`,
/// or `now - 25h` when it is empty/unparseable.
pub(crate) fn effective_last(last_sent: &str, now: OffsetDateTime) -> OffsetDateTime {
    OffsetDateTime::parse(last_sent.trim(), &Rfc3339)
        .unwrap_or_else(|_| now - Duration::from_secs(FRESH_LOOKBACK_HOURS * 3600))
}

/// Whether `schedule` (an RRULE) has an occurrence in the half-open window
/// `(last, now]`, evaluated in [`DEFAULT_TZ`]. An unparseable schedule or an
/// empty/inverted window is "no occurrence" (never fire).
pub(crate) fn rrule_fires_in_window(
    schedule: &str,
    last: OffsetDateTime,
    now: OffsetDateTime,
) -> bool {
    if last >= now {
        return false;
    }
    let Ok(chrono_tz) = DEFAULT_TZ.parse::<chrono_tz::Tz>() else {
        return false;
    };
    let tz: Tz = chrono_tz.into();
    let (Some(last_dt), Some(now_dt)) = (to_tz(last, tz), to_tz(now, tz)) else {
        return false;
    };

    let Ok(rrule) = schedule.trim().parse::<RRule<Unvalidated>>() else {
        return false;
    };
    // Anchor the recurrence at `last` so occurrences are generated from there
    // forward; the BY* parts pin the actual times of day/week.
    let Ok(set) = rrule.build(last_dt) else {
        return false;
    };

    // `all` bounds are inclusive on both ends; keep the upper bound (`<= now`)
    // but re-narrow the lower bound to strict (`> last`) for the half-open
    // window, so an occurrence landing exactly at `last` can't re-fire.
    set.after(last_dt)
        .before(now_dt)
        .all(OCCURRENCE_LIMIT)
        .dates
        .iter()
        .any(|occ| *occ > last_dt)
}

// ---- digest ----------------------------------------------------------------

/// The outcome of running a digest, mirroring the `notify-test` response shape.
#[derive(Debug, Clone)]
pub(crate) struct DigestOutcome {
    pub(crate) sent: bool,
    pub(crate) count: usize,
    pub(crate) title: String,
    pub(crate) message: String,
}

/// Whole days `expected` is overdue relative to `now` (floored, never negative
/// for an actually-overdue item).
fn days_overdue(expected: OffsetDateTime, now: OffsetDateTime) -> i64 {
    (now - expected).whole_days()
}

/// The descriptive clause for an item with no explicit `next_action`, keyed by
/// whose court the (overdue) ball is in.
fn ball_phrase(ball: Ball) -> &'static str {
    match ball {
        Ball::Ours => "your move is late",
        Ball::Theirs => "follow up — silent since due",
        Ball::None => "",
    }
}

/// Build the digest `(title, message, count)` from overdue entries, most-overdue
/// first. Returns `None` when there is nothing to send.
fn build_digest(entries: &[PipelineEntry], now: OffsetDateTime) -> Option<(String, String, usize)> {
    if entries.is_empty() {
        return None;
    }

    // Most-overdue first = earliest `expected_at` first.
    let mut items: Vec<&PipelineEntry> = entries.iter().collect();
    items.sort_by_key(|e| e.item.expected_at);
    let count = items.len();

    let title = match (count, items.first()) {
        (1, Some(first)) => match &first.item.next_action {
            Some(action) => format!("⏰ {} — {action}", first.item.company),
            None => format!("⏰ {}", first.item.company),
        },
        _ => format!("⏰ {count} items need your move"),
    };

    let mut lines = Vec::with_capacity(count + 1);
    for entry in &items {
        let item = &entry.item;
        let role = item
            .role
            .as_deref()
            .map(|r| format!(" — {r}"))
            .unwrap_or_default();
        let overdue = item
            .expected_at
            .map(|e| format!(" — {}d overdue", days_overdue(e, now)))
            .unwrap_or_default();
        let tail = item
            .next_action
            .clone()
            .unwrap_or_else(|| ball_phrase(item.ball).to_string());
        lines.push(format!("• {}{role}{overdue} · {tail}", item.company));
    }
    lines.push(PIPELINE_LINK.to_string());

    Some((title, lines.join("\n"), count))
}

// ---- delivery --------------------------------------------------------------

/// POST a digest to the user's priority-notify server. The token is sent as a
/// Bearer credential and is never logged. Errors on a non-2xx status.
async fn post_digest(settings: &UserSettings, title: &str, message: &str) -> anyhow::Result<()> {
    let url = format!(
        "{}/api/notifications/",
        settings.notify_endpoint.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "title": title,
        "message": message,
        "priority": settings.notify_priority,
        "source": "pipeline",
    });
    let client = crate::recurring::http_client()?;
    let resp = client
        .post(&url)
        .bearer_auth(&settings.notify_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("request to {url} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("priority-notify {url} returned {status}"));
    }
    Ok(())
}

/// Collect the user's overdue-in-scope pipeline items, build one digest, and
/// POST it when there is something to send. Does NOT touch `notify_last_sent`.
///
/// On nothing-overdue, returns `sent = false, count = 0` with a
/// "Nothing overdue" title and no POST. On a POST failure, logs it (without the
/// token) and returns `sent = false` with the built title/message/count intact.
pub(crate) async fn deliver_digest(
    state: &AppState,
    user_id: &str,
    settings: &UserSettings,
    now: OffsetDateTime,
) -> AppResult<DigestOutcome> {
    let scope = Scope::parse(&settings.notify_scope);
    let entries = collect_pipeline(state, user_id, None, false).await?;
    let overdue: Vec<PipelineEntry> = entries
        .into_iter()
        .filter(|e| e.overdue && scope.matches(e.item.ball))
        .collect();

    let Some((title, message, count)) = build_digest(&overdue, now) else {
        return Ok(DigestOutcome {
            sent: false,
            count: 0,
            title: NOTHING_OVERDUE_TITLE.to_string(),
            message: String::new(),
        });
    };

    let sent = match post_digest(settings, &title, &message).await {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(user_id, error = %err, "notify: digest POST failed");
            false
        }
    };

    Ok(DigestOutcome {
        sent,
        count,
        title,
        message,
    })
}

// ---- scheduler -------------------------------------------------------------

/// Spawn the 60s-interval digest scheduler. Detached; runs for the process
/// lifetime. Per-user and per-tick errors are logged, never fatal.
pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(TICK_SECS));
        loop {
            ticker.tick().await;
            run_tick(&state).await;
        }
    });
}

/// One scheduler tick: process every user independently.
async fn run_tick(state: &AppState) {
    let user_ids = match fetch_user_ids(&state.db).await {
        Ok(ids) => ids,
        Err(err) => {
            tracing::error!(error = %err, "notify: listing users failed; skipping tick");
            return;
        }
    };
    let now = OffsetDateTime::now_utc();
    for user_id in user_ids {
        if let Err(err) = process_user(state, &user_id, now).await {
            // A single user's failure must never stop the others.
            tracing::warn!(user_id, error = %err, "notify: per-user tick failed");
        }
    }
}

/// Every user id, for the per-tick sweep.
async fn fetch_user_ids(db: &sqlx::SqlitePool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM users").fetch_all(db).await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Decide-and-fire for one user. Skips silently when notifications are off or
/// no token is set, or when the RRULE has no occurrence in `(last, now]`.
/// Otherwise delivers the digest and ALWAYS stamps `notify_last_sent = now`
/// afterwards (success, nothing-overdue, or POST failure) so an occurrence
/// fires at most once.
async fn process_user(state: &AppState, user_id: &str, now: OffsetDateTime) -> AppResult<()> {
    let settings = {
        let note_id = store::settings_note_id(user_id);
        let _guard = state.note_locks.lock(&note_id).await;
        store::load_or_bootstrap(state, user_id).await?
    };

    if !settings.notify_enabled || settings.notify_token.is_empty() {
        return Ok(());
    }

    let last = effective_last(&settings.notify_last_sent, now);
    if !rrule_fires_in_window(&settings.notify_schedule, last, now) {
        return Ok(());
    }

    // The occurrence fired: deliver (best-effort) then stamp unconditionally.
    if let Err(err) = deliver_digest(state, user_id, &settings, now).await {
        tracing::warn!(user_id, error = %err, "notify: building digest failed");
    }

    let stamp = now.format(&Rfc3339).unwrap_or_default();
    store::update_settings_fields(state, user_id, &[("notify_last_sent", &stamp)]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_note_core::leads::{Kind, PipelineItem};

    fn ts(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &Rfc3339).unwrap()
    }

    fn entry(
        company: &str,
        role: Option<&str>,
        next_action: Option<&str>,
        ball: Ball,
        expected_at: Option<OffsetDateTime>,
        overdue: bool,
    ) -> PipelineEntry {
        PipelineEntry {
            item: PipelineItem {
                id: format!("pipeline/{}", company.to_ascii_lowercase()),
                kind: Kind::Lead,
                company: company.to_string(),
                role: role.map(str::to_string),
                stage: None,
                ball,
                expected_at,
                next_action: next_action.map(str::to_string),
                contacts: Vec::new(),
                priority: None,
                closed_reason: None,
                stage_history: Vec::new(),
            },
            overdue,
        }
    }

    // ---- scope → ball filtering ----

    #[test]
    fn scope_parse_and_matches() {
        assert_eq!(Scope::parse("ours"), Scope::Ours);
        assert_eq!(Scope::parse("theirs"), Scope::Theirs);
        assert_eq!(Scope::parse("both"), Scope::Both);
        // Unknown → Both.
        assert_eq!(Scope::parse("whatever"), Scope::Both);

        assert!(Scope::Ours.matches(Ball::Ours));
        assert!(!Scope::Ours.matches(Ball::Theirs));
        assert!(!Scope::Ours.matches(Ball::None));

        assert!(Scope::Theirs.matches(Ball::Theirs));
        assert!(!Scope::Theirs.matches(Ball::Ours));

        assert!(Scope::Both.matches(Ball::Ours));
        assert!(Scope::Both.matches(Ball::Theirs));
        // Both never includes a closed/dormant item.
        assert!(!Scope::Both.matches(Ball::None));
    }

    #[test]
    fn scope_filters_entries_by_ball() {
        let now = ts("2026-10-02T10:00:00Z");
        let past = Some(ts("2026-10-01T00:00:00Z"));
        let entries = vec![
            entry("Ours", None, None, Ball::Ours, past, true),
            entry("Theirs", None, None, Ball::Theirs, past, true),
            // Overdue flag false → excluded regardless of ball.
            entry("NotOverdue", None, None, Ball::Ours, past, false),
        ];

        let filter = |scope: Scope| {
            entries
                .iter()
                .filter(|e| e.overdue && scope.matches(e.item.ball))
                .map(|e| e.item.company.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(filter(Scope::Ours), vec!["Ours".to_string()]);
        assert_eq!(filter(Scope::Theirs), vec!["Theirs".to_string()]);
        assert_eq!(
            filter(Scope::Both),
            vec!["Ours".to_string(), "Theirs".to_string()]
        );
        let _ = now;
    }

    // ---- RRULE (last, now] fire decision ----

    #[test]
    fn rrule_fires_when_occurrence_in_window() {
        // Daily at 08:00 Europe/Warsaw (06:00Z in early-October CEST). Window
        // (Oct 1 09:00Z, Oct 2 10:00Z] contains Oct 2 06:00Z (= 08:00 Warsaw).
        let last = ts("2026-10-01T09:00:00Z");
        let now = ts("2026-10-02T10:00:00Z");
        assert!(rrule_fires_in_window(
            "FREQ=DAILY;BYHOUR=8;BYMINUTE=0",
            last,
            now
        ));
    }

    #[test]
    fn rrule_does_not_fire_without_occurrence_in_window() {
        // Window (Oct 2 07:00Z, Oct 2 09:00Z] — Warsaw 08:00 = 06:00Z is before
        // it, next is Oct 3 → no occurrence.
        let last = ts("2026-10-02T07:00:00Z");
        let now = ts("2026-10-02T09:00:00Z");
        assert!(!rrule_fires_in_window(
            "FREQ=DAILY;BYHOUR=8;BYMINUTE=0",
            last,
            now
        ));
    }

    #[test]
    fn rrule_half_open_excludes_the_lower_bound() {
        // `last` sits exactly on an 08:00 Warsaw occurrence (06:00Z); it must
        // NOT re-fire. The next is 24h later, past `now`.
        let last = ts("2026-10-02T06:00:00Z");
        let now = ts("2026-10-02T12:00:00Z");
        assert!(!rrule_fires_in_window(
            "FREQ=DAILY;BYHOUR=8;BYMINUTE=0",
            last,
            now
        ));
    }

    #[test]
    fn rrule_empty_or_inverted_window_never_fires() {
        let t = ts("2026-10-02T10:00:00Z");
        assert!(!rrule_fires_in_window(
            "FREQ=DAILY;BYHOUR=8;BYMINUTE=0",
            t,
            t
        ));
        let later = ts("2026-10-02T11:00:00Z");
        assert!(!rrule_fires_in_window(
            "FREQ=DAILY;BYHOUR=8;BYMINUTE=0",
            later,
            t
        ));
    }

    #[test]
    fn effective_last_falls_back_when_empty() {
        let now = ts("2026-10-02T10:00:00Z");
        let back = effective_last("", now);
        assert_eq!(back, now - Duration::from_secs(25 * 3600));
        // A valid stamp is parsed as-is.
        assert_eq!(
            effective_last("2026-10-01T06:00:00Z", now),
            ts("2026-10-01T06:00:00Z")
        );
    }

    // ---- digest string formatting ----

    #[test]
    fn digest_single_item_title_and_bullet() {
        let now = ts("2026-10-04T10:00:00Z");
        let entries = vec![entry(
            "Affirm",
            Some("Senior SWE"),
            Some("await reschedule"),
            Ball::Theirs,
            Some(ts("2026-10-01T10:00:00Z")),
            true,
        )];
        let (title, message, count) = build_digest(&entries, now).unwrap();
        assert_eq!(count, 1);
        assert_eq!(title, "⏰ Affirm — await reschedule");
        assert_eq!(
            message,
            "• Affirm — Senior SWE — 3d overdue · await reschedule\n\
             → https://notes.osmosis.page/pipeline"
        );
    }

    #[test]
    fn digest_multi_item_title_sorted_most_overdue_first() {
        let now = ts("2026-10-10T10:00:00Z");
        let entries = vec![
            // 2 days overdue, ball ours, no next_action → ours phrase.
            entry(
                "Beta",
                None,
                None,
                Ball::Ours,
                Some(ts("2026-10-08T10:00:00Z")),
                true,
            ),
            // 9 days overdue, ball theirs, no next_action → theirs phrase.
            entry(
                "Alpha",
                None,
                None,
                Ball::Theirs,
                Some(ts("2026-10-01T10:00:00Z")),
                true,
            ),
        ];
        let (title, message, count) = build_digest(&entries, now).unwrap();
        assert_eq!(count, 2);
        assert_eq!(title, "⏰ 2 items need your move");
        assert_eq!(
            message,
            "• Alpha — 9d overdue · follow up — silent since due\n\
             • Beta — 2d overdue · your move is late\n\
             → https://notes.osmosis.page/pipeline"
        );
    }

    #[test]
    fn digest_empty_is_none() {
        let now = ts("2026-10-02T10:00:00Z");
        assert!(build_digest(&[], now).is_none());
    }

    // ---- validators ----

    #[test]
    fn rrule_validator_accepts_defaults_and_rejects_junk() {
        assert!(is_valid_rrule("FREQ=DAILY;BYHOUR=8;BYMINUTE=0"));
        assert!(is_valid_rrule(
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;BYHOUR=8;BYMINUTE=0"
        ));
        assert!(!is_valid_rrule("not-an-rrule"));
        assert!(!is_valid_rrule(""));
    }

    #[test]
    fn endpoint_validator_requires_http_scheme_and_host() {
        assert!(is_valid_endpoint("https://notifications.osmosis.page"));
        assert!(is_valid_endpoint("http://localhost:8080"));
        assert!(!is_valid_endpoint("ftp://example.com"));
        assert!(!is_valid_endpoint("notaurl"));
        assert!(!is_valid_endpoint("https://"));
    }
}

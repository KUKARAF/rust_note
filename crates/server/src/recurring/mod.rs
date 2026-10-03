//! Recurring todos — daily-repeating items of two kinds:
//!
//! * **local**: done-state is a YAML frontmatter bool in *today's* daily note
//!   (`diary/YYYY-MM-DD`), keyed by the todo's `key` (e.g. `stretches: true`;
//!   a missing key means "not done").
//! * **foreign**: done-state is polled live over HTTP — a GET to `url` whose
//!   JSON body carries a `has_unread` bool. The row is *pending* while
//!   `has_unread == true` and *satisfied* once it flips to `false`.
//! * **calendar**: done-state is derived from an iCalendar (ICS) feed at `url`.
//!   The row is *pending* (active) while the calendar has an event occurring
//!   *today* (in [`crate::stats::DEFAULT_TZ`]) whose `SUMMARY` matches the row's
//!   `regex`, and *satisfied* otherwise (`done = !active`). See [`calendar`].
//!
//! The DEFINITIONS list (every todo, of either kind) is stored as a JSON array
//! in a single git-backed note ([`DEFS_NOTE_ID`]); per-day local done-state
//! stays in the daily-note frontmatter, separate from the definitions.

pub mod calendar;
pub mod routes;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use rust_note_core::frontmatter::Frontmatter;

use crate::state::AppState;
use crate::stats::{fmt_date, today_in_tz};

/// The git-backed note whose whole body is the JSON array of recurring-todo
/// definitions. Owned/parsed by the backend (not hand-edited frontmatter).
pub const DEFS_NOTE_ID: &str = "settings/recurring-todos";

/// Which of the two done-state mechanisms a recurring todo uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecurringKind {
    /// Done-state is a frontmatter bool in today's daily note.
    Local,
    /// Done-state is polled from `url` (`has_unread == false` means satisfied).
    Foreign,
    /// Done-state is derived from an ICS feed at `url`: *pending* while an event
    /// occurring today has a `SUMMARY` matching `regex`, else *satisfied*.
    Calendar,
}

/// A single recurring-todo definition, as stored in [`DEFS_NOTE_ID`] and sent
/// over the API. `url` is non-null for `Foreign` (a JSON status endpoint) and
/// `Calendar` (an ICS feed) rows; `regex` is only meaningful for `Calendar` rows
/// (the `SUMMARY` pattern to match against today's events).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurringTodo {
    pub key: String,
    pub label: String,
    pub emoji: String,
    pub order: i64,
    pub kind: RecurringKind,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub regex: Option<String>,
}

impl RecurringTodo {
    /// Keep only the fields each kind uses so the stored definitions and API
    /// responses stay consistent: `Local` carries neither `url` nor `regex`;
    /// `Foreign` keeps `url` but never `regex`; `Calendar` keeps both.
    fn normalized(mut self) -> Self {
        match self.kind {
            RecurringKind::Local => {
                self.url = None;
                self.regex = None;
            }
            RecurringKind::Foreign => {
                self.regex = None;
            }
            RecurringKind::Calendar => {}
        }
        self
    }
}

/// A definition plus its currently-computed done-state, returned by the GET.
#[derive(Debug, Clone, Serialize)]
pub struct RecurringTodoStatus {
    pub key: String,
    pub label: String,
    pub emoji: String,
    pub order: i64,
    pub kind: RecurringKind,
    pub url: Option<String>,
    pub regex: Option<String>,
    pub done: bool,
}

/// Read a note's current text, preferring the live collab room over disk so an
/// open editor's unflushed edits aren't missed. Missing note → empty string.
pub(crate) fn read_note_text(state: &AppState, note_id: &str) -> String {
    match state.rooms.get(note_id) {
        Some(room) => room.snapshot_text(),
        None => {
            let rel = crate::notes::fs_store::note_id_to_path(note_id);
            state
                .notes_repo
                .read_file(&rel)
                .ok()
                .flatten()
                .unwrap_or_default()
        }
    }
}

/// Parse the definitions JSON out of the note body. A missing/empty/unparsable
/// body degrades to an empty list rather than erroring.
pub(crate) fn parse_defs(text: &str) -> Vec<RecurringTodo> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    serde_json::from_str(trimmed).unwrap_or_default()
}

/// Load and parse the recurring-todo definitions from [`DEFS_NOTE_ID`].
pub(crate) fn load_defs(state: &AppState) -> Vec<RecurringTodo> {
    parse_defs(&read_note_text(state, DEFS_NOTE_ID))
}

/// The daily-note id for today in `tz` (`diary/YYYY-MM-DD`).
pub(crate) fn today_daily_note_id(tz: &str) -> String {
    format!("diary/{}", fmt_date(today_in_tz(tz)))
}

/// Whether a local todo is satisfied today: its frontmatter key parses as the
/// literal `true`. Any other value (or a missing key) counts as not done.
pub(crate) fn local_done(fm: &Frontmatter, key: &str) -> bool {
    fm.get(key) == Some("true")
}

/// Whether `s` is safe to store as a `Foreign`/`Calendar` recurring-todo
/// `url`: a well-formed http(s) URL whose host isn't `localhost` and, when
/// the host is a literal IP, isn't loopback/private/link-local/unique-local
/// (this covers the cloud-metadata range `169.254.0.0/16` too). Blocks the
/// obvious SSRF vectors; a plain hostname that merely *resolves* to an
/// internal address later (DNS rebinding) isn't caught here, but the
/// outbound client in [`http_client`] disables redirects, which closes the
/// other common SSRF escalation path.
pub fn is_safe_recurring_url(s: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(s.trim()) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    // `Url::host_str` keeps the IPv6 literal's brackets (e.g. `[::1]`); strip
    // them before parsing so literal-IPv6 urls are checked, not waved through
    // as an "ordinary hostname".
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => is_public_ipv4(ip),
        Ok(IpAddr::V6(ip)) => is_public_ipv6(ip),
        // Not an IP literal: an ordinary hostname, allowed.
        Err(_) => true,
    }
}

/// Whether an IPv4 address is routable/public (i.e. not loopback, private,
/// link-local/metadata, unspecified, broadcast, or documentation-reserved).
fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation())
}

/// Whether an IPv6 address is routable/public (i.e. not loopback, unique-local
/// (`fc00::/7`), unicast link-local (`fe80::/10`), or unspecified; IPv4-mapped
/// addresses are checked against their embedded IPv4 payload).
fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    !(ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() || ip.is_unicast_link_local())
}

/// Build a hardened outbound HTTP client for polling foreign rows: no
/// redirects (SSRF guard; foreign urls must be direct `https://`) and a bounded
/// timeout. Mirrors the client in `todos::query`.
pub(crate) fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| anyhow!("failed to build HTTP client: {e}"))
}

/// GET `url` and read its JSON `has_unread` bool. Errors on a non-2xx status,
/// a non-JSON body, or a missing/`non-bool` `has_unread` field.
pub(crate) async fn fetch_has_unread(client: &reqwest::Client, url: &str) -> anyhow::Result<bool> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow!("request to {url} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("{url} returned {status}"));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("parsing {url} response as JSON failed: {e}"))?;
    body.get("has_unread")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| anyhow!("{url} response has no boolean `has_unread` field"))
}

#[cfg(test)]
mod tests {
    use super::is_safe_recurring_url;

    #[test]
    fn rejects_non_http_schemes() {
        assert!(!is_safe_recurring_url("ftp://example.com"));
        assert!(!is_safe_recurring_url("file:///etc/passwd"));
        assert!(!is_safe_recurring_url("not a url"));
    }

    #[test]
    fn rejects_localhost_and_loopback() {
        assert!(!is_safe_recurring_url("http://localhost/x"));
        assert!(!is_safe_recurring_url("http://LOCALHOST/x"));
        assert!(!is_safe_recurring_url("http://127.0.0.1/x"));
        assert!(!is_safe_recurring_url("http://127.5.6.7/x"));
        assert!(!is_safe_recurring_url("http://[::1]/x"));
    }

    #[test]
    fn rejects_cloud_metadata_and_link_local() {
        assert!(!is_safe_recurring_url(
            "http://169.254.169.254/latest/meta-data"
        ));
        assert!(!is_safe_recurring_url("http://[fe80::1]/x"));
    }

    #[test]
    fn rejects_private_ranges() {
        assert!(!is_safe_recurring_url("http://10.0.0.5/x"));
        assert!(!is_safe_recurring_url("http://172.16.0.1/x"));
        assert!(!is_safe_recurring_url("http://172.31.255.255/x"));
        assert!(!is_safe_recurring_url("http://192.168.1.1/x"));
        assert!(!is_safe_recurring_url("http://[fd00::1]/x"));
        assert!(!is_safe_recurring_url("http://[fc00::1]/x"));
    }

    #[test]
    fn rejects_ipv4_mapped_private() {
        assert!(!is_safe_recurring_url("http://[::ffff:127.0.0.1]/x"));
        assert!(!is_safe_recurring_url("http://[::ffff:192.168.1.1]/x"));
    }

    #[test]
    fn allows_ordinary_public_hosts() {
        assert!(is_safe_recurring_url("https://example.com/feed.ics"));
        assert!(is_safe_recurring_url(
            "http://status.example.org:8080/status"
        ));
        assert!(is_safe_recurring_url("https://8.8.8.8/x"));
    }
}

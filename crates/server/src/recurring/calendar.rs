//! iCalendar (ICS) fetching + "occurs today" evaluation for `Calendar`
//! recurring todos.
//!
//! A `Calendar` row is *active* (pending) when its ICS source has an event
//! occurring **today** (civil date in [`DEFAULT_TZ`]) whose `SUMMARY` matches the
//! row's regex. To avoid re-fetching a shared link once per row, a whole request
//! parses each distinct url **once** into a [`ParsedCalendar`] (the list of
//! today's event summaries) and every row's regex is then matched against that
//! list in memory.
//!
//! Recurrence: fully delegated to the `rrule` crate via
//! [`icalendar`]'s `recurrence` feature (`CalendarEvent::get_recurrence`), which
//! expands `RRULE`/`RDATE`/`EXDATE` and yields a single occurrence even for a
//! plain (non-recurring) `DTSTART`. So weekly *and* every other RRULE frequency,
//! plus RDATE/EXDATE, are handled.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Days, NaiveDate, TimeZone};
use icalendar::{Calendar, CalendarEvent, Component, DatePerhapsTime};
use regex::RegexBuilder;

use crate::stats::DEFAULT_TZ;

/// The `SUMMARY` of every event occurring today for one ICS url. Regex matching
/// is applied per-row against this list, so a url is fetched+parsed once per
/// request even when several calendar rows share it.
pub(crate) type ParsedCalendar = Vec<String>;

/// GET an ICS document body (plain text, not JSON). Errors on a non-2xx status.
async fn fetch_ics(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("request to {url} failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!("{url} returned {status}"));
    }
    resp.text()
        .await
        .map_err(|e| anyhow::anyhow!("reading {url} body failed: {e}"))
}

/// Fetch + parse each **distinct** url once, mapping url → today's event
/// summaries. A fetch or parse failure logs a warning and yields an empty list
/// (fail-open to not-active) so a broken calendar never blocks the modal.
pub(crate) async fn fetch_calendars<I>(
    client: &reqwest::Client,
    urls: I,
    today: time::Date,
) -> HashMap<String, ParsedCalendar>
where
    I: IntoIterator<Item = String>,
{
    let distinct: HashSet<String> = urls.into_iter().collect();
    let mut map = HashMap::with_capacity(distinct.len());
    for url in distinct {
        let summaries = match fetch_ics(client, &url).await {
            Ok(body) => todays_event_summaries(&body, today),
            Err(err) => {
                tracing::warn!(error = %err, url, "recurring calendar: ICS fetch failed");
                Vec::new()
            }
        };
        map.insert(url, summaries);
    }
    map
}

/// Whether any of `summaries` matches `regex` (compiled **case-insensitively**).
/// An empty/`None` regex, or one that fails to compile, is treated as not
/// active (a warning is logged for a compile failure).
pub(crate) fn calendar_active(summaries: &[String], regex: Option<&str>) -> bool {
    let pattern = match regex {
        Some(p) if !p.trim().is_empty() => p,
        _ => return false,
    };
    let re = match RegexBuilder::new(pattern).case_insensitive(true).build() {
        Ok(re) => re,
        Err(err) => {
            tracing::warn!(error = %err, pattern, "recurring calendar: invalid regex");
            return false;
        }
    };
    summaries.iter().any(|s| re.is_match(s))
}

/// Whether a single calendar row is active (pending): its ICS `url` had an event
/// today whose `SUMMARY` matches the row's `regex`. A missing `url` (or a url not
/// in `calendars`) is not active. `done` for the row is the negation.
pub(crate) fn calendar_row_active(
    calendars: &HashMap<String, ParsedCalendar>,
    url: Option<&str>,
    regex: Option<&str>,
) -> bool {
    let Some(url) = url else {
        return false;
    };
    let summaries = calendars.get(url).map(Vec::as_slice).unwrap_or(&[]);
    calendar_active(summaries, regex)
}

/// Parse an ICS body and return the `SUMMARY` of every event occurring on
/// `today` (civil date in [`DEFAULT_TZ`]). Network-free and unit-tested. A
/// malformed calendar or event is skipped, never fatal.
pub(crate) fn todays_event_summaries(ics: &str, today: time::Date) -> ParsedCalendar {
    let cal: Calendar = match ics.parse() {
        Ok(cal) => cal,
        Err(err) => {
            tracing::warn!(error = %err, "recurring calendar: ICS parse failed");
            return Vec::new();
        }
    };
    let Some(ctx) = TodayContext::build(today) else {
        tracing::warn!("recurring calendar: could not resolve today-context");
        return Vec::new();
    };

    let mut out = Vec::new();
    for event in cal.calendar_events() {
        if event_occurs_today(&event, &ctx) {
            out.push(event.get_summary().unwrap_or_default().to_string());
        }
    }
    out
}

/// Precomputed "today" state: the civil date plus the timezone and the query
/// window handed to the rrule expander.
struct TodayContext {
    today: NaiveDate,
    tz: rrule::Tz,
    /// Occurrences are queried in `[after, before]`, widened well past today so a
    /// multi-day span or a midnight tz-offset can't drop today's instance; the
    /// per-occurrence civil-date check re-narrows to today.
    after: DateTime<rrule::Tz>,
    before: DateTime<rrule::Tz>,
}

impl TodayContext {
    fn build(today: time::Date) -> Option<Self> {
        let today_naive = NaiveDate::from_ymd_opt(
            today.year(),
            u32::from(u8::from(today.month())),
            u32::from(today.day()),
        )?;
        let tz: rrule::Tz = DEFAULT_TZ.parse::<chrono_tz::Tz>().ok()?.into();
        // 7 days back catches a week-long all-day span still covering today; 2
        // forward gives headroom around midnight/DST.
        let lo = today_naive
            .checked_sub_days(Days::new(7))?
            .and_hms_opt(0, 0, 0)?;
        let hi = today_naive
            .checked_add_days(Days::new(2))?
            .and_hms_opt(0, 0, 0)?;
        let after = tz.from_local_datetime(&lo).single()?;
        let before = tz.from_local_datetime(&hi).single()?;
        Some(Self {
            today: today_naive,
            tz,
            after,
            before,
        })
    }
}

/// Whether any occurrence of `event` covers today's civil date.
fn event_occurs_today(event: &CalendarEvent, ctx: &TodayContext) -> bool {
    let Some(start) = event.get_start() else {
        return false;
    };
    let all_day = matches!(start, DatePerhapsTime::Date(_));
    let end_offset = end_offset_days(&start, event.get_end().as_ref());

    match event.get_recurrence() {
        Ok(set) => set
            .after(ctx.after)
            .before(ctx.before)
            .all(366)
            .dates
            .iter()
            .any(|occ| {
                covers_today(
                    occurrence_civil_date(occ, all_day, ctx),
                    end_offset,
                    ctx.today,
                )
            }),
        Err(err) => {
            // Invalid RRULE/DTSTART: fall back to the raw DTSTART civil date so a
            // bad recurrence rule never hides an otherwise-valid event.
            tracing::warn!(error = %err, "recurring calendar: recurrence expansion failed; using DTSTART only");
            covers_today(start.date_naive(), end_offset, ctx.today)
        }
    }
}

/// The civil date an occurrence represents. All-day occurrences are compared by
/// their own (tz-agnostic) date; timed occurrences are converted into
/// [`DEFAULT_TZ`] first.
fn occurrence_civil_date(
    occ: &DateTime<rrule::Tz>,
    all_day: bool,
    ctx: &TodayContext,
) -> NaiveDate {
    if all_day {
        occ.naive_local().date()
    } else {
        occ.with_timezone(&ctx.tz).date_naive()
    }
}

/// How many extra civil days past the start date a single instance covers, so a
/// multi-day event is caught on every day it spans. All-day `DTEND` is exclusive
/// (`[start, end)`), timed `DTEND` is inclusive; both normalize to an inclusive
/// day offset. No `DTEND`, or a zero/negative span, is a single day (offset 0).
fn end_offset_days(start: &DatePerhapsTime, end: Option<&DatePerhapsTime>) -> i64 {
    let Some(end) = end else {
        return 0;
    };
    let raw = (end.date_naive() - start.date_naive()).num_days();
    // All-day (DATE-typed) DTEND is exclusive, so its inclusive last day is one
    // less; timed DTEND is already inclusive.
    let all_day = matches!(start, DatePerhapsTime::Date(_));
    let offset = if all_day { raw - 1 } else { raw };
    offset.max(0)
}

/// Whether `today` falls within `[occ_date, occ_date + end_offset]`.
fn covers_today(occ_date: NaiveDate, end_offset: i64, today: NaiveDate) -> bool {
    if occ_date > today {
        return false;
    }
    let last = u64::try_from(end_offset.max(0))
        .ok()
        .and_then(|days| occ_date.checked_add_days(Days::new(days)))
        .unwrap_or(occ_date);
    today <= last
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `time::Date` from y/m/d for tests.
    fn date(y: i32, m: u8, d: u8) -> time::Date {
        time::Date::from_calendar_date(y, time::Month::try_from(m).unwrap(), d).unwrap()
    }

    /// An ICS with an all-day event dated `today` ("Date Night") plus an event
    /// dated far in the past. `today` is computed dynamically by the caller.
    fn ics_all_day_today(today: time::Date) -> String {
        let ymd = format!(
            "{:04}{:02}{:02}",
            today.year(),
            u8::from(today.month()),
            today.day()
        );
        format!(
            "BEGIN:VCALENDAR\r\n\
             VERSION:2.0\r\n\
             PRODID:-//test//test//EN\r\n\
             BEGIN:VEVENT\r\n\
             UID:today@test\r\n\
             DTSTART;VALUE=DATE:{ymd}\r\n\
             SUMMARY:Date Night\r\n\
             END:VEVENT\r\n\
             BEGIN:VEVENT\r\n\
             UID:past@test\r\n\
             DTSTART;VALUE=DATE:19990101\r\n\
             SUMMARY:Ancient History\r\n\
             END:VEVENT\r\n\
             END:VCALENDAR"
        )
    }

    #[test]
    fn all_day_today_matches_regex_and_ignores_past() {
        let today = crate::stats::today_in_tz(DEFAULT_TZ);
        let ics = ics_all_day_today(today);
        let summaries = todays_event_summaries(&ics, today);

        // Only today's event is reported; the 1999 event is not.
        assert_eq!(summaries, vec!["Date Night".to_string()]);
        assert!(calendar_active(&summaries, Some("(?i)date night")));
        // A non-matching regex → not active.
        assert!(!calendar_active(&summaries, Some("standup")));
        // Empty / None regex → not active.
        assert!(!calendar_active(&summaries, Some("")));
        assert!(!calendar_active(&summaries, None));
    }

    #[test]
    fn weekly_rrule_occurrence_today_is_active() {
        // A weekly-recurring event that STARTED four weeks ago on today's weekday
        // must still surface today (recurrence expansion, not just DTSTART).
        let today = crate::stats::today_in_tz(DEFAULT_TZ);
        let start = today - time::Duration::weeks(4);
        let ymd = format!(
            "{:04}{:02}{:02}",
            start.year(),
            u8::from(start.month()),
            start.day()
        );
        let ics = format!(
            "BEGIN:VCALENDAR\r\n\
             VERSION:2.0\r\n\
             PRODID:-//test//test//EN\r\n\
             BEGIN:VEVENT\r\n\
             UID:weekly@test\r\n\
             DTSTART:{ymd}T190000Z\r\n\
             DTEND:{ymd}T200000Z\r\n\
             RRULE:FREQ=WEEKLY\r\n\
             SUMMARY:Weekly Date Night\r\n\
             END:VEVENT\r\n\
             END:VCALENDAR"
        );
        let summaries = todays_event_summaries(&ics, today);
        assert_eq!(summaries, vec!["Weekly Date Night".to_string()]);
        assert!(calendar_active(&summaries, Some("(?i)date night")));
    }

    #[test]
    fn no_event_today_is_not_active() {
        let today = date(2000, 1, 3); // a fixed date far from any test event
        let ics = ics_all_day_today(date(1999, 1, 1)); // only past + a "today" of 1999
        let summaries = todays_event_summaries(&ics, today);
        assert!(summaries.is_empty());
        assert!(!calendar_active(&summaries, Some("(?i)date night")));
    }

    #[test]
    fn invalid_regex_is_not_active() {
        let summaries = vec!["Date Night".to_string()];
        // Unbalanced bracket → compile error → not active (logged).
        assert!(!calendar_active(&summaries, Some("[")));
    }
}

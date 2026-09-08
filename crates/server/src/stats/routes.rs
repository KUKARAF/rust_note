//! HTTP handlers for `/api/stats`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, put};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};
use yrs::sync::{Message, SyncMessage};
use yrs::updates::encoder::Encode;
use yrs::{GetString, ReadTxn, Text, Transact};

use rust_note_core::frontmatter::Frontmatter;
use rust_note_core::stats::{
    aggregate, append_stat_entry, read_stats, AggKind, StatPoint, StatValue,
};

use super::{
    agg_of, fmt_date, is_valid_metric, parse_date, parse_registry, timezone_of, today_in_tz,
    MetricDef, CHART_KINDS,
};
use crate::auth::session::RequireAuth;
use crate::collab::room::{Room, CONTENT_FIELD};
use crate::db_users::commit_author;
use crate::error::{AppError, AppResult};
use crate::notes::acl;
use crate::notes::fs_store::{is_valid_note_id, note_id_to_path, path_to_note_id};
use crate::settings::store::{load_or_bootstrap, settings_note_id};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats", get(list_stats).post(log_stat))
        .route("/api/stats/docs", get(stats_docs))
        .route("/api/stats/registry", get(get_registry))
        .route(
            "/api/stats/registry/{metric}",
            put(put_registry).delete(delete_registry),
        )
}

// ---- docs -----------------------------------------------------------------

const STATS_DOC: &str = include_str!("../../../../docs/stats.md");
const API_DOC: &str = include_str!("../../../../docs/api_stats.md");

async fn stats_docs(RequireAuth(_user): RequireAuth) -> impl IntoResponse {
    let body = format!("{STATS_DOC}\n\n---\n\n{API_DOC}");
    (
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        body,
    )
}

// ---- GET /api/stats -------------------------------------------------------

#[derive(Debug, Deserialize)]
struct StatsQuery {
    from: Option<String>,
    to: Option<String>,
    metric: Option<String>,
}

#[derive(Debug, Serialize)]
struct StatsResponse {
    series: Vec<Series>,
}

#[derive(Debug, Serialize)]
struct Series {
    metric: String,
    label: String,
    unit: String,
    chart: String,
    agg: String,
    days: Vec<DayValue>,
}

#[derive(Debug, Serialize)]
struct DayValue {
    date: String,
    value: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    points: Vec<StatPoint>,
}

/// If `note_id` is a daily note (`diary/YYYY-MM-DD`), return its date part.
fn diary_date(note_id: &str) -> Option<String> {
    let rest = note_id.strip_prefix("diary/")?;
    if rest.len() != 10 || rest.contains('/') {
        return None;
    }
    let shaped = rest.as_bytes().iter().enumerate().all(|(i, &b)| match i {
        4 | 7 => b == b'-',
        _ => b.is_ascii_digit(),
    });
    shaped.then(|| rest.to_string())
}

async fn list_stats(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Query(query): Query<StatsQuery>,
) -> AppResult<Json<StatsResponse>> {
    let fm = load_settings_fm(&state, &user_id).await?;
    let defs = parse_registry(&fm);
    let tz = timezone_of(&fm);

    let to = query
        .to
        .as_deref()
        .and_then(parse_date)
        .unwrap_or_else(|| today_in_tz(&tz));
    let from = query
        .from
        .as_deref()
        .and_then(parse_date)
        .unwrap_or_else(|| to.checked_sub(time::Duration::days(30)).unwrap_or(to));
    let (from_s, to_s) = (fmt_date(from), fmt_date(to));

    // Aggregate each daily note's stats keyed by date.
    let paths = state.notes_repo.list_notes().map_err(AppError::Internal)?;
    let mut per_day: BTreeMap<String, Vec<(String, StatValue)>> = BTreeMap::new();
    for rel_path in paths {
        let note_id = path_to_note_id(&rel_path);
        if !is_valid_note_id(&note_id) {
            continue;
        }
        let Some(date) = diary_date(&note_id) else {
            continue;
        };
        if date < from_s || date > to_s {
            continue;
        }
        acl::adopt_if_orphaned(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?;
        if !acl::can_read(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?
        {
            continue;
        }
        // Prefer live room text over disk (a note open in the editor lags disk).
        let content = match state.rooms.get(&note_id) {
            Some(room) => room.snapshot_text(),
            None => state
                .notes_repo
                .read_file(&rel_path)
                .map_err(AppError::Internal)?
                .unwrap_or_default(),
        };
        per_day.insert(date, read_stats(&content));
    }

    // Decide which metrics to emit.
    let emit: Vec<MetricDef> = match query.metric.as_deref() {
        Some(m) if is_valid_metric(m) => defs
            .iter()
            .find(|d| d.metric == m)
            .cloned()
            .map(|d| vec![d])
            // An unregistered metric queried directly → boolean parent-any view.
            .unwrap_or_else(|| {
                vec![MetricDef {
                    metric: m.to_string(),
                    unit: String::new(),
                    label: m.to_string(),
                    chart: "boolean".to_string(),
                    agg: "count".to_string(),
                }]
            }),
        Some(_) => Vec::new(),
        None => defs,
    };

    let mut series = Vec::new();
    for def in emit {
        let mut days = Vec::new();
        for (date, stats) in &per_day {
            if def.chart == "boolean" {
                if day_bool_for(stats, &def.metric) {
                    days.push(DayValue {
                        date: date.clone(),
                        value: 1,
                        points: Vec::new(),
                    });
                }
            } else if let Some(points) = numeric_points(stats, &def.metric) {
                let value = aggregate(&points, agg_of(&def));
                days.push(DayValue {
                    date: date.clone(),
                    value,
                    points,
                });
            }
        }
        series.push(Series {
            metric: def.metric,
            label: def.label,
            unit: def.unit,
            chart: def.chart,
            agg: def.agg,
            days,
        });
    }

    Ok(Json(StatsResponse { series }))
}

fn numeric_points(stats: &[(String, StatValue)], metric: &str) -> Option<Vec<StatPoint>> {
    stats
        .iter()
        .find(|(k, _)| k == metric)
        .and_then(|(_, v)| match v {
            StatValue::Nums(p) => Some(p.clone()),
            StatValue::Bool(_) => None,
        })
}

/// Parent-any truthiness: true if `metric` is truthy or any `metric.*` present.
fn day_bool_for(stats: &[(String, StatValue)], metric: &str) -> bool {
    let prefix = format!("{metric}.");
    stats.iter().any(|(k, v)| {
        (k == metric || k.starts_with(&prefix))
            && match v {
                StatValue::Bool(b) => *b,
                StatValue::Nums(p) => !p.is_empty(),
            }
    })
}

// ---- POST /api/stats ------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LogRequest {
    key: String,
    value: i64,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Debug, Serialize)]
struct LogResponse {
    note_id: String,
    key: String,
}

async fn log_stat(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<LogRequest>, AppError>,
) -> AppResult<Json<LogResponse>> {
    let key = body.key.trim().to_string();
    if !is_valid_metric(&key) {
        return Err(AppError::BadRequest("invalid metric key".to_string()));
    }

    let date = match body.date.as_deref() {
        Some(d) => {
            fmt_date(parse_date(d).ok_or_else(|| AppError::BadRequest("invalid date".to_string()))?)
        }
        None => {
            let fm = load_settings_fm(&state, &user_id).await?;
            fmt_date(today_in_tz(&timezone_of(&fm)))
        }
    };
    let note_id = format!("diary/{date}");
    let rel_path = note_id_to_path(&note_id);

    acl::ensure_note_registered(&state.db, &note_id, &user_id)
        .await
        .map_err(AppError::Internal)?;

    // Write through the collab room so a note open in the editor isn't clobbered
    // by the next flush, and connected editors see the change live.
    let room = state.rooms.get_or_create(&note_id, &user_id, &state).await;
    let applied = apply_append(&room, &key, body.value, body.at.as_deref());
    // Persist synchronously so the daily-note file exists on disk immediately —
    // otherwise a `GET /api/stats` right after (it scans on-disk notes) would
    // miss a note that lives only in the room until the debounced flush. Safe
    // for a note open in the editor: the flush writes the room's own text.
    if applied {
        if let Err(err) = crate::collab::persist::flush_room(&room, &state).await {
            tracing::error!(note_id = %note_id, error = %err, "stats flush failed");
        }
    }
    state.rooms.release(room, state.clone());
    let _ = rel_path;

    if applied {
        Ok(Json(LogResponse { note_id, key }))
    } else {
        Err(AppError::BadRequest(
            "could not log value (bad time, or the key already holds a non-numeric value)"
                .to_string(),
        ))
    }
}

/// Append the entry into the room's CRDT doc (frontmatter-region edit),
/// broadcast it to connected sockets, and mark the room dirty. Returns false
/// when the append is refused (bad time / non-numeric existing key).
fn apply_append(room: &Arc<Room>, key: &str, value: i64, at: Option<&str>) -> bool {
    let awareness = room.lock_awareness();
    let doc = awareness.doc();
    let text = doc.get_or_insert_text(CONTENT_FIELD);
    let old = text.get_string(&doc.transact());
    let Some(new) = append_stat_entry(&old, key, value, at) else {
        return false;
    };
    if new == old {
        return true;
    }
    let before_sv = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let (p, s) = byte_diff(&old, &new);
        let del_len = old.len() - s - p;
        if del_len > 0 {
            text.remove_range(&mut txn, p as u32, del_len as u32);
        }
        let ins = &new[p..new.len() - s];
        if !ins.is_empty() {
            text.insert(&mut txn, p as u32, ins);
        }
    }
    let update = doc.transact().encode_state_as_update_v1(&before_sv);
    drop(awareness);

    let frame = Message::Sync(SyncMessage::Update(update)).encode_v1();
    room.broadcast_frame(0, Bytes::from(frame)); // origin 0: no real conn uses it
    room.mark_dirty();
    true
}

/// Longest common (prefix, suffix) byte lengths between `old` and `new`,
/// snapped to char boundaries. The differing middle is `old[p..len-s]` →
/// `new[p..len-s]`. Offsets are UTF-8 bytes to match the server doc's
/// `OffsetKind::Bytes`.
fn byte_diff(old: &str, new: &str) -> (usize, usize) {
    let (ob, nb) = (old.as_bytes(), new.as_bytes());
    let max_p = ob.len().min(nb.len());
    let mut p = 0;
    while p < max_p && ob.get(p) == nb.get(p) {
        p += 1;
    }
    while p > 0 && !old.is_char_boundary(p) {
        p -= 1;
    }
    let max_s = (ob.len() - p).min(nb.len() - p);
    let mut s = 0;
    while s < max_s && ob.get(ob.len() - 1 - s) == nb.get(nb.len() - 1 - s) {
        s += 1;
    }
    while s > 0 && !old.is_char_boundary(ob.len() - s) {
        s -= 1;
    }
    (p, s)
}

// ---- registry (in the user settings note) ---------------------------------

async fn load_settings_fm(state: &AppState, user_id: &str) -> AppResult<Frontmatter> {
    let note_id = settings_note_id(user_id);
    let _guard = state.note_locks.lock(&note_id).await;
    load_or_bootstrap(state, user_id).await?;
    let rel = note_id_to_path(&note_id);
    let raw = state
        .notes_repo
        .read_file(&rel)
        .map_err(AppError::Internal)?
        .unwrap_or_default();
    Ok(Frontmatter::parse(&raw))
}

async fn get_registry(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<Vec<MetricDef>>> {
    let fm = load_settings_fm(&state, &user_id).await?;
    Ok(Json(parse_registry(&fm)))
}

#[derive(Debug, Deserialize)]
struct RegistryUpsert {
    #[serde(default)]
    unit: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    chart: String,
    #[serde(default)]
    agg: String,
}

async fn put_registry(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(metric): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<RegistryUpsert>, AppError>,
) -> AppResult<Json<MetricDef>> {
    if !is_valid_metric(&metric) {
        return Err(AppError::BadRequest("invalid metric name".to_string()));
    }
    let chart = if CHART_KINDS.contains(&body.chart.as_str()) {
        body.chart
    } else {
        "line".to_string()
    };
    let agg = if AggKind::parse(&body.agg).is_some() {
        body.agg.to_ascii_lowercase()
    } else {
        "sum".to_string()
    };
    let label = if body.label.trim().is_empty() {
        metric.clone()
    } else {
        body.label.trim().to_string()
    };
    let unit = body.unit.trim().to_string();

    let note_id = settings_note_id(&user_id);
    let _guard = state.note_locks.lock(&note_id).await;
    load_or_bootstrap(&state, &user_id).await?;
    let rel = note_id_to_path(&note_id);
    let raw = state
        .notes_repo
        .read_file(&rel)
        .map_err(AppError::Internal)?
        .unwrap_or_default();
    let mut fm = Frontmatter::parse(&raw);
    fm.set(&format!("stat.{metric}.unit"), &unit);
    fm.set(&format!("stat.{metric}.label"), &label);
    fm.set(&format!("stat.{metric}.chart"), &chart);
    fm.set(&format!("stat.{metric}.agg"), &agg);

    let (author_name, author_email) = commit_author(&state.db, &user_id)
        .await
        .map_err(AppError::Internal)?;
    state
        .notes_repo
        .write_and_commit(
            &rel,
            &fm.render(),
            &author_name,
            &author_email,
            "update stats registry",
        )
        .await
        .map_err(AppError::Internal)?;
    acl::touch_updated_at(&state.db, &note_id)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(MetricDef {
        metric,
        unit,
        label,
        chart,
        agg,
    }))
}

#[derive(Debug, Serialize)]
struct DeleteResponse {
    removed: usize,
}

async fn delete_registry(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(metric): Path<String>,
) -> AppResult<Json<DeleteResponse>> {
    if !is_valid_metric(&metric) {
        return Err(AppError::BadRequest("invalid metric name".to_string()));
    }
    let note_id = settings_note_id(&user_id);
    let _guard = state.note_locks.lock(&note_id).await;
    load_or_bootstrap(&state, &user_id).await?;
    let rel = note_id_to_path(&note_id);
    let raw = state
        .notes_repo
        .read_file(&rel)
        .map_err(AppError::Internal)?
        .unwrap_or_default();
    let mut fm = Frontmatter::parse(&raw);
    let removed = fm.remove_prefix(&format!("stat.{metric}"));
    if removed > 0 {
        let (author_name, author_email) = commit_author(&state.db, &user_id)
            .await
            .map_err(AppError::Internal)?;
        state
            .notes_repo
            .write_and_commit(
                &rel,
                &fm.render(),
                &author_name,
                &author_email,
                "delete stats metric",
            )
            .await
            .map_err(AppError::Internal)?;
        acl::touch_updated_at(&state.db, &note_id)
            .await
            .map_err(AppError::Internal)?;
    }
    Ok(Json(DeleteResponse { removed }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oidc::OidcClient;
    use crate::config::Config;
    use crate::notes::repo::NotesRepo;
    use axum_extra::extract::cookie::Key;
    use std::marker::PhantomData;
    use std::sync::Arc;

    async fn test_state() -> (AppState, tempfile::TempDir, tempfile::TempDir) {
        let notes_dir = tempfile::tempdir().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");
        let notes_repo = NotesRepo::open_or_init(notes_dir.path().to_str().unwrap()).unwrap();
        let db = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('alice', 'alice@example.com', 'Alice', '2026-01-01T00:00:00Z')",
        )
        .execute(&db)
        .await
        .unwrap();
        let state = AppState {
            db,
            notes_repo,
            config: Arc::new(Config::from_env()),
            oidc: None::<Arc<OidcClient>>,
            cookie_key: Key::generate(),
            note_locks: crate::state::NoteLocks::new(),
            rooms: crate::collab::room::RoomRegistry::new(),
        };
        (state, notes_dir, db_dir)
    }

    async fn seed(state: &AppState, note_id: &str, content: &str) {
        let rel = note_id_to_path(note_id);
        acl::ensure_note_registered(&state.db, note_id, "alice")
            .await
            .unwrap();
        state
            .notes_repo
            .write_and_commit(&rel, content, "alice", "alice@example.com", "seed")
            .await
            .unwrap();
    }

    async fn register(state: &AppState, metric: &str, unit: &str, chart: &str, agg: &str) {
        put_registry(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path(metric.to_string()),
            WithRejection(
                Json(RegistryUpsert {
                    unit: unit.to_string(),
                    label: String::new(),
                    chart: chart.to_string(),
                    agg: agg.to_string(),
                }),
                PhantomData,
            ),
        )
        .await
        .unwrap();
    }

    fn q(from: &str, to: &str, metric: Option<&str>) -> Query<StatsQuery> {
        Query(StatsQuery {
            from: Some(from.to_string()),
            to: Some(to.to_string()),
            metric: metric.map(str::to_string),
        })
    }

    #[tokio::test]
    async fn aggregates_registered_metric_across_days() {
        let (state, _n, _d) = test_state().await;
        seed(
            &state,
            "diary/2026-09-01",
            "---\ncaffeine: [40@0720, 30@1500]\n---\nbody\n",
        )
        .await;
        register(&state, "caffeine", "mg", "line", "sum").await;

        let resp = list_stats(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            q("2026-08-01", "2026-09-30", None),
        )
        .await
        .unwrap()
        .0;

        let s = resp
            .series
            .iter()
            .find(|s| s.metric == "caffeine")
            .expect("caffeine series present");
        assert_eq!(s.unit, "mg");
        assert_eq!(s.days.len(), 1);
        assert_eq!(s.days[0].date, "2026-09-01");
        assert_eq!(s.days[0].value, 70); // 40 + 30
        assert_eq!(s.days[0].points.len(), 2);
    }

    #[tokio::test]
    async fn parent_query_is_boolean_any() {
        let (state, _n, _d) = test_state().await;
        seed(
            &state,
            "diary/2026-09-01",
            "---\nexercise.cardio: 30@0930\n---\n",
        )
        .await;
        seed(&state, "diary/2026-09-02", "---\nprotein: 60\n---\n").await;

        let resp = list_stats(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            q("2026-09-01", "2026-09-30", Some("exercise")),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(resp.series.len(), 1);
        let s = &resp.series[0];
        assert_eq!(s.metric, "exercise");
        assert_eq!(s.chart, "boolean");
        // Only the day with an exercise.* entry is true.
        assert_eq!(s.days.len(), 1);
        assert_eq!(s.days[0].date, "2026-09-01");
    }

    #[tokio::test]
    async fn log_appends_through_room_and_migrates_to_list() {
        let (state, _n, _d) = test_state().await;

        for (value, at) in [(40, "0720"), (30, "1500")] {
            log_stat(
                State(state.clone()),
                RequireAuth("alice".to_string()),
                WithRejection(
                    Json(LogRequest {
                        key: "caffeine".to_string(),
                        value,
                        at: Some(at.to_string()),
                        date: Some("2026-09-01".to_string()),
                    }),
                    PhantomData,
                ),
            )
            .await
            .unwrap();
        }

        // The room is still live (reaper is on a grace timer); its snapshot has
        // both samples migrated into one inline list.
        let room = state.rooms.get("diary/2026-09-01").expect("room live");
        let text = room.snapshot_text();
        assert!(
            text.contains("caffeine: [40@0720, 30@1500]"),
            "expected inline list, got:\n{text}"
        );
    }

    #[tokio::test]
    async fn registry_put_get_delete() {
        let (state, _n, _d) = test_state().await;
        register(&state, "caffeine", "mg", "line", "sum").await;

        let listed = get_registry(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].metric, "caffeine");
        assert_eq!(listed[0].unit, "mg");

        let del = delete_registry(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("caffeine".to_string()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(del.removed, 4); // unit/label/chart/agg

        let after = get_registry(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert!(after.is_empty());
    }

    #[tokio::test]
    async fn docs_endpoint_concatenates_markdown() {
        let (body_headers, body) = stats_docs(RequireAuth("alice".to_string()))
            .await
            .into_response()
            .into_parts();
        assert_eq!(body_headers.status, axum::http::StatusCode::OK);
        // Body is served as markdown; verify both docs are present.
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("# Stats format"));
        assert!(text.contains("`/api/stats` API"));
    }

    #[tokio::test]
    async fn log_rejects_bad_key_and_date() {
        let (state, _n, _d) = test_state().await;
        let bad_key = log_stat(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(LogRequest {
                    key: "stat.caffeine".to_string(),
                    value: 1,
                    at: None,
                    date: Some("2026-09-01".to_string()),
                }),
                PhantomData,
            ),
        )
        .await;
        assert!(matches!(bad_key, Err(AppError::BadRequest(_))));
    }
}

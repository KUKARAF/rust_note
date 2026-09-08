# `/api/stats` API

All endpoints require authentication (session cookie, or `Authorization: Bearer
<device-token>` for the app). Metric data lives in daily-note frontmatter; the
registry lives in your settings note. See the format reference above.

## `POST /api/stats` — log a value

Appends one numeric sample to a metric in a daily note's frontmatter (append-always).

Request body:

```json
{ "key": "caffeine", "value": 40, "at": "0720", "date": "2026-09-01" }
```

- `key` (required) — metric name; dotted allowed (`exercise.cardio`). Lowercase,
  not starting with `stat.`.
- `value` (required) — integer.
- `at` (optional) — `HHMM` 24-hour time. Omit for an untimed sample.
- `date` (optional) — `YYYY-MM-DD`. Defaults to **today in your configured
  timezone** (Europe/Warsaw by default).

The write goes through the live collaborative document, so it is safe even while
the daily note is open in the editor. A second write to the same metric turns a
scalar into an inline list.

Response: `{ "note_id": "diary/2026-09-01", "key": "caffeine" }`.

## `GET /api/stats` — read aggregated series

Query params:

- `from`, `to` (optional) — inclusive `YYYY-MM-DD` range. Defaults to a recent
  window.
- `metric` (optional) — restrict to one metric (or a parent name, see below).

Returns one series per **registered** metric: per-day aggregated value (using the
metric's `agg`), plus the raw timed points, with `unit`/`label`/`chart` from the
registry.

```json
{
  "series": [
    {
      "metric": "caffeine", "label": "Caffeine", "unit": "mg",
      "chart": "line", "agg": "sum",
      "days": [ { "date": "2026-09-01", "value": 70,
                  "points": [ {"value":40,"at":"07:20"}, {"value":30,"at":"15:00"} ] } ]
    }
  ]
}
```

**Parent query:** `?metric=exercise` returns a boolean-per-day series that is
`true` when `exercise` is truthy or any `exercise.*` entry exists that day.

## Registry

- `GET /api/stats/registry` — list defined metrics: `[{metric, unit, label,
  chart, agg}]`.
- `PUT /api/stats/registry/{metric}` — create/update a definition. Body:
  `{ "unit": "mg", "label": "Caffeine", "chart": "line", "agg": "sum" }`.
  `chart ∈ {line,bar,boolean,heatmap}`, `agg ∈ {sum,last,max,min,count}`.
- `DELETE /api/stats/registry/{metric}` — remove a definition (the metric's data
  in daily notes is untouched; it just stops charting).

## `GET /api/stats/docs`

Returns this documentation (the format reference and this API reference,
concatenated) as `text/markdown`.

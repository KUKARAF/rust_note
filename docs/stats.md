# Stats format

`/stats` tracks arbitrary daily metrics stored in the **YAML frontmatter** of
your daily notes (`diary/YYYY-MM-DD`). Nothing is enforced: any key you write is
stored; it just isn't charted until you define it in the `/stats` view.

## Value shapes

| Shape | Example | Meaning |
|-------|---------|---------|
| integer | `protein: 60` | a plain daily amount |
| flag | `exercise: true` | a boolean for the day |
| timed integer | `caffeine: 40@0720` | `40` at **07:20** (`@HHMM`, 24h) |
| inline list | `caffeine: [40@0720, 30@1500]` | several samples in one day |

Rules:

- **Keys** are flat and may be **dotted** for namespacing: `exercise.cardio`,
  `exercise.pushups`. Dots are literal — there is no YAML nesting.
- **Time** is `@HHMM` (four digits, 24-hour): `@0720`, `@2030`. No colon.
- **Integers only.** No floats.
- Repeating a metric appends: a second write turns `caffeine: 40@0720` into
  `caffeine: [40@0720, 30@1500]`.

## Example daily note

```yaml
---
protein: 60
caffeine: [40@0720, 30@1500]
alcohol: 15@2030
exercise.cardio: 30@0930
exercise.pushups: 40
exercise: true
pomodoros: 6
---
today's notes…
```

## Parent (namespace) queries

Querying a parent name aggregates its children: `exercise` is **true** for a day
if `exercise` is truthy or **any** `exercise.*` entry exists that day.

## Registry (units & display)

Each metric's unit, label, chart type, and aggregation are configured per-user in
the `/stats` view (stored in your settings note). A metric with data but no
registry entry is kept in the file but not charted.

- `unit` — label suffix (e.g. `mg`, `g`, `min`).
- `label` — display name.
- `chart` — `line` | `bar` | `boolean` | `heatmap`.
- `agg` — how a day's multiple samples reduce to one number: `sum` | `last` |
  `max` | `min` | `count`.

## Body fat

The **Body fat** tab (`/bodyfat`) is a single form: enter the skinfold readings
(mm) and it computes body-fat % with the **Jackson-Pollock 3-site** equation
(body density from the sum of three skinfolds and age, then Siri:
`BF% = 495 / density − 450`). Sites depend on sex — men: chest, abdomen, thigh;
women: triceps, suprailiac, thigh. Saving appends to the daily note (the date in
the form, default today). Values are integer **tenths** because the format is
integer-only:

```yaml
skinfold.chest: 150     # 15.0 mm
skinfold.abdomen: 250
skinfold.thigh: 200
bodyfat: 179            # 17.9 %
```

`bodyfat` is registered with `agg: last`; `/stats` shows the latest value and a
monthly chart (last reading of each month). The `skinfold.*` keys are stored but
not charted.

Sex and birth date (used to pick the chart row/table and derive age at each
reading) are stored in your settings note frontmatter, not per device:

| Key | Values |
|-----|--------|
| `sex` | `male` \| `female` (empty = unset) |
| `birth_date` | `YYYY-MM-DD`, not in the future (empty = unset) |

Both are readable/writable via `GET`/`PUT /api/settings` (`sex`, `birth_date`).

## Legacy format

An older block-list form is still **read** so existing data keeps charting; it is
rewritten to the inline form above on its next API write:

```yaml
caffeine:
  - value: 40
    time: "07:20"
```

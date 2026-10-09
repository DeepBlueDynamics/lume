# Path list assumed by the golden corpus

This is the Signal K path set the corpus queries reference, and the set `ti-bench gen`
must emit into the synthetic signalk-parquet `raw` layout. It is the coordination
contract between the corpus (this lane) and the W0 generator/catalog (Rigid Roadrunner).
If the generator emits different paths, the corpus and this list must be updated together
in a contracts PR.

## Parquet layout (REAL signalk-parquet, per plan/design/signalk-formats.md)

The generator writes the **real** signalk-parquet raw tier, not a bespoke schema:

- Hive layout `tier=raw/context=<ctx>/path=<path>/year=YYYY/day=DDD/<prefix>_<export>.parquet`.
- `received_timestamp` / `signalk_timestamp` are **ISO-8601 VARCHAR**, not TIMESTAMP.
- No `$source` column; the sourceRef is `source_label` (plus `source`, `source_type`,
  `source_pgn`, `source_src`, `meta` JSON).
- Scalar paths have a `value` column (DOUBLE / BOOLEAN / UTF8, inferred per file).
- Object paths (e.g. `navigation.position`) have **no** `value` column, only
  `value_<key>` columns (`value_latitude`, `value_longitude`, ...). Nested objects and
  arrays are dropped.

The oracles do **not** read these files directly. They target a DuckDB view `raw`
(`tests/golden/raw_view.sql`) that flattens the real layout into the logical schema below,
casting the string timestamps, coalescing the per-file value types, mapping `source_label`
→ `source`, and splitting object paths into `path.<key>` rows.

## Logical `raw` view (what oracles see)

| Column | Type | Meaning |
|---|---|---|
| `context` | VARCHAR | Signal K context URN, e.g. `vessels.urn:mrn:imo:mmsi:367000000` |
| `path` | VARCHAR | Signal K path (object paths flattened to `path.key`) |
| `ts` | TIMESTAMP | `signalk_timestamp`, falling back to `received_timestamp` |
| `value` | DOUBLE | Numeric value (bsi) |
| `value_str` | VARCHAR | String/enum value (set) |
| `source` | VARCHAR | sourceRef (from `source_label`) |

Numeric and set samples are separate rows (a string sample has `value = NULL` and vice
versa).

## `docs` schema (notes, logbook, alerts)

| Column | Type |
|---|---|
| `context` | VARCHAR |
| `kind` | VARCHAR (`notes`, `logbook`, `alerts`) |
| `ts_start` | TIMESTAMP |
| `ts_end` | TIMESTAMP |
| `title` | VARCHAR |
| `body` | VARCHAR |

## Catalog tables

- `vessels(ord, urn, name, mmsi, first_seen, last_seen)`
- `paths(path, field, agg, type, units, scale, depth, description, first_seen, last_seen)`
- `shards(vessel, shard_no, ts_from, ts_to, sealed, bytes, hash)`

## Vessels

| ord | URN |
|---|---|
| 0 | `vessels.urn:mrn:imo:mmsi:367000000` (PV-1, "self") |
| 1 | `vessels.urn:mrn:imo:mmsi:367000001` |

## Paths, aggregate profile, type, scale

`@mean/@min/@max` is the default profile; `@last` and `@count` are opt-in (or `slow`
profile). Scale is the fixed-point factor used by both ingest and the oracle.

| Path | Type | Profile (agg columns emitted) | Scale |
|---|---|---|---|
| `environment.wind.speedTrue` | bsi | @mean @min @max | 3 |
| `environment.wind.speedApparent` | bsi | @mean @min @max | 3 |
| `environment.wind.directionTrue` | bsi | @mean @min @max | 4 |
| `navigation.speedOverGround` | bsi | @mean @min @max @last | 3 |
| `navigation.courseOverGroundTrue` | bsi | @mean @min @max | 4 |
| `navigation.headingMagnetic` | bsi | @mean @min @max | 4 |
| `navigation.position.latitude` | bsi | @mean @last | 7 |
| `navigation.position.longitude` | bsi | @mean @last | 7 |
| `navigation.attitude.roll` | bsi | @mean @min @max | 4 |
| `navigation.state` | set | — | — |
| `propulsion.port.state` | set | — | — |
| `propulsion.starboard.state` | set | — | — |
| `propulsion.main.state` | set | — | — |
| `propulsion.port.motorPower` | bsi | @mean @min @max | 1 |
| `propulsion.starboard.motorPower` | bsi | @mean @min @max | 1 |
| `propulsion.port.revolutions` | bsi | @mean @min @max | 2 |
| `propulsion.starboard.revolutions` | bsi | @mean @min @max | 2 |
| `propulsion.port.temperature` | bsi | @mean @min @max | 2 |
| `propulsion.starboard.temperature` | bsi | @mean @min @max | 2 |
| `electrical.batteries.house.voltage` | bsi | @mean @min @max | 3 |
| `electrical.batteries.house.current` | bsi | @mean @min @max | 2 |
| `electrical.batteries.house.stateOfCharge` | bsi | @mean @min @max | 4 |
| `electrical.solar.house.panelPower` | bsi | @mean @min @max | 1 |
| `electrical.bilge.pumpCycles` | count | @count (also `@starts`) | 0 |
| `environment.depth.belowTransducer` | bsi | @mean @min @max | 2 |
| `environment.outside.temperature` | bsi | @mean @min @max | 2 |
| `environment.outside.pressure` | bsi | @mean @min @max | 0 |
| `tanks.freshWater.port.currentLevel` | bsi (slow → @last) | @last | 4 |
| `tanks.freshWater.starboard.currentLevel` | bsi (slow → @last) | @last | 4 |

`navigation.position` also produces H3 `geo` cells at res 5, 7, 9 (used by
`within_nm`/`in_bbox`), which the corpus exercises but the oracle computes from lat/lon.

### Set field semantics (single-valued per bucket)

Set fields (states, enums, booleans, strings) are **single-valued per bucket**: the last
value from the preferred source. The generator emits exactly one source per state path, so
the oracle's `arg_max(value_str, ts)` matches TI's preferred-source last value. Negation
uses `IS DISTINCT FROM` with a presence join (spec 14). `path$source` is the multi-valued
exception (any-sample semantics) and is not exercised yet.

### Set field enumerations

- `propulsion.port.state`, `propulsion.starboard.state`, `propulsion.main.state`: `started`, `idle`, `reverse`, `off`.
- `navigation.state`: `sailing`, `motoring`, `anchored`, `moored`.

### Planted document keywords (for `match()`)

`ti-bench gen` must plant these tokens in `docs` of the stated kind so the `match()` oracle
(LIKE substring) and BM25 agree:

- `leak`, `water` — `notes`
- `bilge` — `notes`
- `anchorage` — `logbook`
- `mooring` — `notes`  (unambiguous; `anchor` was dropped because it is a substring of `anchorage`)
- `weather` — `notes`
- `alarm` — `alerts`

## Notes / open items

- The bare path `"electrical.batteries.house.voltage"` (no `@agg`) aliases `@mean`; the
  corpus relies on this.
- `qx-002` and `qx-007` reference `propulsion.starboard.state` and
  `.starboard.motorPower` — the generator must emit starboard-side paths even on a
  single-engine vessel (or the corpus narrows to `propulsion.port.*`; unresolved until
  the generator's vessel model is set — flagged to Rigid Roadrunner).
- Meridian VHF transcript format is undefined (spec 08/11); no corpus entry depends on it yet.

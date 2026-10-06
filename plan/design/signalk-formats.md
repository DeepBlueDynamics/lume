<!-- Filed by the lead 2026-10-06 from a background research agent's report. Sources are pinned to plugin/server commits inside. Not yet checked against a real .parquet file: run DESCRIBE on one actual raw-tier file before trusting section 1.3 fully. -->

# Signal K data formats a Lume TI indexer must read

Research date: 2026-10-05. Every claim below cites a source. Source files are pinned to the commit read, so line numbers stay valid:

| Repo | Commit read | Version |
|---|---|---|
| motamman/signalk-parquet | `a03d34f` (2026-09-30) | package.json `1.0.1-beta.1`; npm `latest` = `1.0.0`, `beta` = `1.0.1-beta.1` (https://registry.npmjs.org/signalk-parquet) |
| SignalK/specification | `fb628fb` (2026-10-03) | schemas `1.8.4` |
| SignalK/signalk-server | `faab2be` (2026-10-04) | `2.33.0` |
| tkurki/signalk-to-influxdb2 | `bbdb108` (2026-09-25) | `2.3.0` |
| tkurki/signalk-to-influxdb (v1) | `db84864` (2026-09-28) | `1.11.0` |
| halos-org/halos-marine-containers | `158c07d` (2026-09-22) | n/a |
| halos-org/signalk-server-docker | `799c775` (2026-09-21) | tag `v2.33.0-halos.1` |

Abbreviations used in citations:
- `PQ` = https://github.com/motamman/signalk-parquet/blob/a03d34f1a70c820f71b5ac8376d169775af18b85
- `SPEC` = https://github.com/SignalK/specification/blob/fb628fb4ee569149fd3810b271d0f987428b4703
- `SRV` = https://github.com/SignalK/signalk-server/blob/faab2be886fa7d52f926e7d2acf91c910623d4d7
- `IX2` = https://github.com/tkurki/signalk-to-influxdb2/blob/bbdb10801167368fed86ab12c4957ebe4ad291ca
- `IX1` = https://github.com/tkurki/signalk-to-influxdb/blob/db8486445323f38017fc5c8888fda261cc424dff
- `HMC` = https://github.com/halos-org/halos-marine-containers/blob/158c07d4b5288d577b4a958eec9900a4b3663588
- `HSD` = https://github.com/halos-org/signalk-server-docker/blob/799c7758be697e2bc54a11b79695fe8fef4ead04

---

## 1. signalk-parquet

### 1.1 Stability statement
The README says that from 1.0.0 (2026-09-20), "the on-disk layout, the parquet and buffer schemas ... are stable": data written by 1.0 stays readable by every later 1.x. The raw SQL endpoint is outside that promise. (`PQ/README.md` line 9)

### 1.2 Data flow (this determines what ends up on disk)
1. A delta arrives. `handleStreamData` builds a `DataRecord` (`PQ/src/data-handler.ts` L586-668).
2. The record goes into a SQLite WAL buffer (`buffer.db`), one table per path (`PQ/src/utils/sqlite-buffer.ts` L1-7, L545-596).
3. Once a day (default 04:00 UTC), the buffer for each (context, path, day) is exported to one Parquet file with `@dsnp/parquetjs` (`PQ/README.md` L16-19; `PQ/src/services/parquet-export-service.ts` L303-360, L460-600; `PQ/package.json` L82).
4. DuckDB `COPY` builds the aggregate tiers (`5s`, `60s`, `1h`) from raw (`PQ/src/services/aggregation-service.ts` L394-600).

Consequence: the newest 0-24 h of data is **not in Parquet yet**. It is only in `buffer.db`, which has 48 h retention (`PQ/README.md` L20-25). The plugin's own History API federates "local parquet → cloud supplement → SQLite buffer" (`PQ/README.md` L38-39).

### 1.3 Raw-tier Parquet schema, as the writer actually produces it
The schema is **inferred per file** from the records being written (`PQ/src/schema-service.ts` L55-238, called from `PQ/src/parquet-writer.ts` L391-407). The column set is the union of the record keys, sorted alphabetically, and every column is `optional: true`.

How each record key is produced (`PQ/src/data-handler.ts` L598-622) and what type it is written as:

| Column | Source | Parquet type | Notes |
|---|---|---|---|
| `received_timestamp` | `new Date().toISOString()` when the plugin received the delta | **UTF8 (string)**, forced (`schema-service.ts` L110-125; validator rule "Timestamps should be UTF8", L273-284) | ISO-8601, ms precision, `Z` |
| `signalk_timestamp` | `update.timestamp`, copied as given; falls back to now | **UTF8 (string)**, forced | Precision is whatever the source sent |
| `context` | `normalizedDelta.context`, falling back to path config, then `'vessels.self'` | UTF8, forced | Full context, e.g. `vessels.urn:mrn:imo:mmsi:...` |
| `path` | Signal K path | UTF8, forced | |
| `value` | Scalar delta value only | DOUBLE if every value in the file parses as a number; BOOLEAN if every value is `"true"`/`"false"`; otherwise UTF8 (`schema-service.ts` L143-177) | **Absent from object-path files** (`schema-service.ts` L102-108) |
| `value_<key>` | One column per **primitive** top-level key of an object value (`data-handler.ts` L650-662) | Same inference: DOUBLE / BOOLEAN / UTF8 | e.g. `value_latitude`, `value_longitude`, `value_altitude` |
| `value_json` | `JSON.stringify` of the whole object value | **Not written by the current writer**: `detectOptimalSchema` "Always skip value_json" (`schema-service.ts` L95-99) | Present in the buffer and in older files. Readers check for it (`PQ/src/HistoryAPI.ts` L2030-2035, L2841-2843; `PQ/CHANGELOG.md` L1333) |
| `source` | `JSON.stringify(update.source)` | UTF8, forced (`startsWith('source')`) | e.g. `{"label":..,"type":..,"pgn":..,"src":..}` |
| `source_label` | **`update.$source`** (the sourceRef, e.g. `can0.115`), *not* `source.label` | UTF8 | This is the `$source` column under another name |
| `source_type` | `update.source.type` | UTF8 | |
| `source_pgn` | `update.source.pgn` | **UTF8** (the rule forcing `source*` to UTF8 overrides the README's "number") | |
| `source_src` | `update.source.src` | UTF8 | Missing from the README table |
| `meta` | `JSON.stringify(app.getMetadata(path))`, **repeated on every row** | UTF8, forced | |

Sources:
- README schema table: `PQ/README.md` L215-231.
- README "Smart Data Types" (it claims INT64 for integers): `PQ/README.md` L249-257.
- The code instead says "Always use DOUBLE for numeric maritime data (never INT64/BIGINT)" (`PQ/src/parquet-writer.ts` L574-575), and converts BIGINT to DOUBLE (`schema-service.ts` L134-140).

Round-trip caveat: in the SQLite buffer, scalar `value` is stored as `TEXT` via `String(value)` (`sqlite-buffer.ts` L878-885). On export it is parsed back: anything numeric-looking becomes a Number, and `"true"`/`"false"` become booleans (`sqlite-buffer.ts` L936-944). So a genuine **string** value that looks numeric (e.g. `"123"`) can come out as DOUBLE. Because inference runs per file, the type of `value` can **drift between day files** for the same path. The plugin reads with `union_by_name=true` "to reconcile schema drift across day files" (`PQ/src/utils/parquet-files.ts` L145-150).

### 1.4 Object values (navigation.position etc.)
- `{latitude, longitude, altitude?}` becomes `value_latitude`, `value_longitude`, (`value_altitude`) DOUBLE columns. There is **no** `value` column (`data-handler.ts` L650-662; `schema-service.ts` L84-108). The README's DuckDB examples use `value_latitude` / `value_longitude` (`PQ/README.md` L740-830).
- Only **top-level primitive** keys are flattened. Nested objects and arrays are not, so in current files they survive only in `value_json`, which is not written (see 1.3). Example: `notifications.*` produces `value_state` and `value_message`, but `method` (an array) and v2 `status` (an object) do **not** appear as columns. This is inferred from `data-handler.ts` L652-660 plus `schema-service.ts` L96-99. UNVERIFIED against a real file.
- Objects whose keys are all meta keys (`units`, `meta`, `description`, `displayUnits`, `zones`) are dropped as "meta-only" (`data-handler.ts` L627-647).

### 1.5 Non-numeric values
- Strings and enums: `value` UTF8.
- Booleans: `value` BOOLEAN, but only if every row in that file is boolean; a mix falls back to UTF8.
- `null` values: written as an undefined/absent cell (`parquet-writer.ts` L733-737).

All of this is from `schema-service.ts` L143-177 and `README.md` L249-257. Aggregate tiers skip string paths, which "must always use raw tier" (`HistoryAPI.ts` L2049-2056).

### 1.6 `$source` and `context` columns
- `context` exists as a data column (table in 1.3).
- There is no column literally named `$source`. The sourceRef is in **`source_label`** (`data-handler.ts` L606), and the full source object is in `source` as a JSON string.
- Aggregate tiers have no source columns at all (`aggregation-service.ts` L462-475: `GROUP BY bucket_time, context, path`). Sources are merged in aggregates.

### 1.7 Directory and file layout
Hive-style, **per tier / per context / per path / per UTC day** (`PQ/src/utils/hive-path-builder.ts` L37-58):
```
<outputDirectory>/tier={raw|5s|60s|1h}/context=<ctx>/path=<path>/year=YYYY/day=DDD/<prefix>_<YYYY-MM-DDTHHMM>.parquet
```
- Hive partitioning is always on: `useHivePartitioning: true, // Always use Hive partitioning` (`PQ/src/index.ts` L341). The legacy flat `vessels/<id>/<path/as/dirs>/` layout exists only as a migration source (`PQ/README.md` L158-213).
- Context sanitizing: `.` becomes `__`, `:` becomes `-` (`hive-path-builder.ts` L199-201). Path sanitizing: `.` becomes `__` (L214-216).
  - **Lossy.** Unsanitizing maps every `-` back to `:` (L206-208), so a UUID context `urn:mrn:signalk:uuid:c0d7-...` cannot be recovered from the directory name. The code acknowledges that context directory names "are lossy and may hold several colliding contexts" (`parquet-files.ts` L44-49).
  - **Always take `context` from the data column, not the directory.**
- `vessels.self` is **resolved to the real self context** before the directory is built (`parquet-export-service.ts` L473-475, L546-548). So the self directory is `context=vessels__urn-mrn-imo-mmsi-<mmsi>` (or the uuid form). The README's `context=vessels__self` examples (`README.md` L127, L1588) do not match this code. UNVERIFIED on a real install.
- `day=` is the zero-padded UTC day-of-year (001-366; `hive-path-builder.ts` L55-56, L228-233). It is assigned from **`received_timestamp`**, not `signalk_timestamp`: the buffer selects by `received_timestamp` per day (`sqlite-buffer.ts` L1197-1352).
- Filename: `${filenamePrefix}_${ISO with ':' and '.' removed, first 15 chars}.parquet`, e.g. `signalk_data_2026-03-03T0400.parquet`. The default prefix is `signalk_data` (`parquet-export-service.ts` L486-494; `index.ts` L298; `README.md` L1588). The timestamp is the **export** time, not the data time. The README tree's `data_20250716T120000.parquet` (L131) is stale.
- **Compaction** merges a year's day files into `year=YYYY/year_compact_<year>_<stamp>.parquet`, placed directly in the year directory with no `day=` (`README.md` L301; `hive-path-builder.ts` L111-114). The README's recommended glob `year=*/day=*/*.parquet` (L340) **misses compacted files**.
- Sibling special directories `quarantine/`, `failed/`, `processed/`, `repaired/` sit in the tree. `**/*.parquet` descends into them, and DuckDB aborts on 0-byte quarantined files (`README.md` L340; `parquet-files.ts` L10-12). `_FAILED.json` and `_BACKUP` / `_REPAIRED.parquet` files also appear there (`parquet-writer.ts` L189-201; `schema-service.ts` L455-517).
- Writes go to `<file>.parquet.tmp` and are then renamed atomically (`parquet-export-service.ts` L503-517). Ignore `*.tmp`.

### 1.8 Aggregate tiers (5s / 60s / 1h)
Written with DuckDB `COPY` (`aggregation-service.ts` L462-600). Columns:
- `bucket_time`: `time_bucket(..., received_timestamp::TIMESTAMP)`, so a **TIMESTAMP** bucketed on *received*, not Signal K, time.
- `context`, `path`.
- `value_avg`, `value_min`, `value_max`, `sample_count`.
- `first_timestamp`, `last_timestamp`: MIN/MAX of the string `received_timestamp`. These two columns are missing from the README table at L233-247.
- Angular (`units === 'rad'`) paths also get `value_sin_avg` and `value_cos_avg`, and have NULL min/max (`README.md` L46-49, L237-247).
- Position paths aggregate `value_latitude` / `value_longitude` instead (`aggregation-service.ts` L394, L397-403; `README.md` L32-35).

### 1.9 DuckDB querying
- The plugin's own reads always use `read_parquet([explicit file list], hive_partitioning=false, union_by_name=true)`. `hive_partitioning=false` "is required: DuckDB otherwise derives columns from the key=value path segments, and the sanitized context partition value shadows the files' context data column (issue #71)" (`PQ/src/utils/parquet-files.ts` L140-169). **Any external DuckDB SQL must do the same.**
- It enumerates only the day directories a time window touches, instead of globbing. A `year=*/day=*` glob cost 366 MB of retained memory per call (`parquet-files.ts` L1-27).
- `/api/query` (raw SQL) is disabled by default, runs in a sandboxed DuckDB, and caps results at 10k rows (`README.md` L281).
- The README's "Basic Queries" use legacy flat paths (`/path/to/navigation/position/*.parquet`, `README.md` L344-350). The Claude-analyzer prompt examples do too (`PQ/src/claude-analyzer.ts` L3381-3432). Neither matches the Hive layout.

### 1.10 History API provider
- It registers as a v2 History API provider through `app.registerHistoryApiProvider` (`PQ/src/history-provider.ts` L1-21, L271, L318, L600, L616, L1122-1140). It implements `getValues`, `getContexts` and `getPaths`.
- It also serves v1 routes `/signalk/v1/history/{values,contexts,paths}` and the extension `/api/history/contexts/spatial` (`README.md` L287-292, L1219-1225).

---

## 2. Signal K delta stream

### 2.1 Delta message shape
From the schema at `SPEC/schemas/delta.json` (id `.../1.8.4/schemas/delta.json`):
- Required: `updates[]`. Optional: `context`, which defaults to self when missing (`SPEC/mdbook/src/data_model.md` L216-232).
- Each update has `values[]` and/or `meta[]`, plus optional `timestamp`, and **either `source` or `$source`, never both**. The schema says `"not": {"allOf": [{"required":["source"]},{"required":["$source"]}]}`.
- A `values[]` item is `{path, value}`, where `value` is string | number | object | boolean | null.
- `meta[]` items are `{path, value: <meta object>}`.

Real delta (`SPEC/mdbook/src/data_model.md` L188-212):
```json
{
  "context": "vessels.urn:mrn:imo:mmsi:234567890",
  "updates": [{
    "source": {"label": "N2000-01", "type": "NMEA2000", "src": "017", "pgn": 127488},
    "timestamp": "2010-01-07T07:18:44Z",
    "values": [
      {"path": "propulsion.0.revolutions", "value": 16.341667},
      {"path": "propulsion.0.boostPressure", "value": 45500}
    ]
  }]
}
```

Details:
- **`timestamp`**: an RFC 3339 string, "UTC only without local offset", schema pattern `.*Z$` (`SPEC/schemas/definitions.json` L8-15). Producers without a clock omit it, and the server fills it in. signalk-server sets `update.timestamp = now` if it is missing, or always if `overrideTimestampWithNow` is set (`SRV/src/index.ts` L569-571).
- **`source`**: an object that requires `label`. Optional fields: `type`, `src`, `canName`, `pgn` (number), `instance`, `sentence`, `talker`, `aisType` (`SPEC/schemas/definitions.json` L16-70).
- **`$source`**: a sourceRef string matching `^[A-Za-z0-9-_.]*$`, e.g. `NMEA0183.COM1.GP` (`definitions.json` L74-79).
- **Reality vs. the schema**: signalk-server sets **both**. On ingress it fills `update.$source = getSourceId(update.source)` when missing, or `providerId` when there is no source object (`SRV/src/index.ts` L538-568). `getSourceId` gives `label.canName`, else `label.src`, else the bare `label` for plugins, else `label.talker` or `label.XX` (`SRV/packages/server-api/src/sourceutil.ts` L10-42). Both plugins examined read `update.$source` (`IX2/src/plugin.ts` L171; `PQ/src/data-handler.ts` L606). Whether the outgoing WebSocket JSON carries both fields is UNVERIFIED; I did not trace the ws serializer.
- **Object values**: `navigation.position` is `{"latitude":..,"longitude":..}` (`data_model.md` L268-270). Static data can arrive with **`path: ""`** and an object value to merge at the context root, e.g. `{"name":"WRANGO"}` (`data_model.md` L272-301). An indexer must handle the empty path.
- **Invalid data**: `value: null` (`data_model.md` L346-350). Notifications are also cleared with `null` (§2.4).

Meta delta (`SPEC/mdbook/src/data_model.md` L302-334):
```json
{"context": "vessels.urn:mrn:imo:mmsi:234567890",
 "updates": [{"timestamp": "2014-08-15T19:02:31.507Z",
   "meta": [{"path": "environment.wind.speedApparent",
     "value": {"units": "m/s", "description": "Apparent wind speed",
               "displayName": "Apparent Wind Speed", "shortName": "AWS",
               "zones": [{"upper": 15.4333, "state": "warn", "message": "high wind speed"}]}}]}]}
```
Meta is sent only when it changes, and always as the **full** meta for a leaf, never partial. It is sent on each new subscription and ignores the subscription policy (`SPEC/mdbook/src/subscription_protocol.md` "Meta data" section).

### 2.2 WebSocket endpoint and subscriptions
- `ws://host/signalk/v1/stream?subscribe=self|all|none`. The default is `self`; `none` streams only the heartbeat until you send subscribe messages. `sendCachedValues=false|true` controls the initial cached dump, which defaults to sending. On connect the server sends a hello `{name, version, timestamp?, self, roles}` (`SPEC/mdbook/src/streaming_api.md` L1-55).
- Playback: `/signalk/v1/playback?subscribe=self&startTime=...&playbackRate=5` (`streaming_api.md` "History playback").
- Unsubscribe all: `{"context":"*","unsubscribe":[{"path":"*"}]}`.
- Subscribe (`SPEC/mdbook/src/subscription_protocol.md`):
```json
{"context": "vessels.self",
 "subscribe": [{"path": "navigation.speedThroughWater", "period": 1000,
                "format": "delta", "policy": "ideal", "minPeriod": 200}]}
```
  - `period` in ms, default 1000.
  - `format` is `delta` or `full`, default delta.
  - `policy` is `instant`, `ideal` or `fixed`, default `ideal`:
    - `instant`: send every change, no faster than `minPeriod`.
    - `ideal`: like instant, but resend the last value if nothing changed within `period`.
    - `fixed`: send the last values every `period`.
  - `minPeriod` is only relevant for `instant`.
  - `path` supports `*` wildcards in the middle or at the end. `context` can be `vessels.*`.
  - Per-source subscription: `navigation.speedThroughWater.values[n2kFromFile.43]`.
- Auth on WS: send an `Authorization: Bearer <token>` header or the auth cookie on connect. Non-HTTP transports put `"token"` inside the message (`SPEC/mdbook/src/security.md` L97-125).

### 2.3 Device access-request flow
Spec (`SPEC/mdbook/src/access_requests.md`):
1. Send `POST /signalk/v1/access/requests` with `{"clientId": "<v4 UUID>", "description": "..."}`.
2. The server answers **202** with `{"state":"PENDING","href":"..."}`.
3. Poll `GET <href>`:
   - Still waiting: `{"state":"PENDING"}`.
   - Approved: `{"state":"COMPLETED","statusCode":200,"accessRequest":{"permission":"APPROVED","token":"...","expirationTime":"..."}}` (`expirationTime` is optional).
   - Denied: `permission: "DENIED"`.
   - Error: `statusCode: 400` with a `message`.
4. A 403 later means the token is invalid; re-request.

signalk-server implementation differences:
- The returned href is **`/signalk/v1/requests/<requestId>`**, not `/signalk/v1/access/requests/<id>` as in the spec example (`SRV/src/requestResponse.ts` L117; route `SRV/src/serverroutes.ts` L838).
- With security disabled the server returns **404** (`'Access requests not available...'`), not 501 (`SRV/src/serverroutes.ts` L801-821).
- It accepts an optional `permissions` field from `readonly | readwrite | admin`, defaulting to `readonly` (`SRV/src/tokensecurity.ts` L100, L1930-1990).
- The request body is limited to 10 KB and rate-limited (`serverroutes.ts` L804-811).

### 2.4 notifications.* value shape
- Spec value: `{"state": "...", "method": ["visual","sound"], "message": "..."}`. The notification is cleared by sending `value: null`. Well-known keys: `notifications.mob|fire|sinking|flooding|collision|grounding|listing|adrift|piracy|abandon` (`SPEC/mdbook/src/notifications.md`).
- **Valid states** (`alarmState` enum): `nominal`, `normal`, `alert`, `warn`, `alarm`, `emergency`. The default is `normal` (`SPEC/schemas/definitions.json` L525-538). So **`nominal` is valid**.
- `method` enum: `visual`, `sound` (`definitions.json` L539-544).
- **signalk-server v2 Notifications API** adds fields to the payload:
  - always: `id` (UUID) and `status: {silenced, acknowledged, canSilence, canAcknowledge, canClear, acknowledgedAt?}`;
  - optionally: `createdAt`, `position`, `data`.
  - Clearing through this API sets `state: "normal"` rather than null.
  - NMEA 2000 alarms map Emergency to `emergency`, Alarm to `alarm`, Warning to `warn`, Caution to `alert`. n2k-signalk sets `method: []` when acknowledged or silenced.

  Sources: `SRV/docs/develop/rest-api/notifications_api.md` L50-100, L321-375, L539-556.

Example payload (`notifications_api.md` L64-87):
```json
{"state":"emergency","method":["sound","visual"],"message":"Person Overboard!",
 "id":"a987be59-d26f-46db-afeb-83987b837a8f",
 "status":{"silenced":false,"acknowledged":false,"canSilence":false,"canAcknowledge":true,"canClear":true},
 "createdAt":"2026-04-06T03:34:48.203Z",
 "position":{"latitude":57.73241514375983,"longitude":11.66365146637231},
 "data":{"crewId":"c67345"}}
```

### 2.5 `meta` object fields
From `SPEC/schemas/definitions.json` L546-800. The schema marks **`description` as required**. Fields:

| Field | Type / content |
|---|---|
| `displayName` | string |
| `longName` | string |
| `shortName` | string |
| `description` | string (required) |
| `enum` | array of permissible values |
| `properties` | per-key `{type, title, description, units, example}` for object values |
| `gaugeType` | deprecated |
| `displayScale` | `{lower, upper, type: linear\|logarithmic\|squareroot\|power, power}` |
| `units` | string |
| `timeout` | seconds |
| `alertMethod`, `warnMethod`, `alarmMethod`, `emergencyMethod` | arrays of `visual` / `sound` |
| `zones[]` | `{lower?, upper?, state (required), message?}` |

`displayUnits` appears in signalk-parquet's meta-key list (`PQ/src/data-handler.ts` L637-643), but it is **not in the 1.8.4 spec schema**. It is a server extension; its shape is UNVERIFIED.

### 2.6 Units (`meta.units`)
Allowed units per `SPEC/schemas/definitions.json` L87-288:

`s`, `Hz`, `m3`, `m3/s`, `kg/s`, `kg/m3`, `deg` (lat/lon only), `rad`, `rad/s`, `A`, `C`, `V`, `W`, `Nm`, `J`, `ohm`, `m`, `m/s`, `m2`, `K`, `Pa`, `kg`, `ratio` (0-1), `m/s2`, `rad/s2`, `N`, `T`, `Lux`, `Pa/s`, `Pa.s`, `B`, `b/s`.

The timestamp definition uses the units string `"RFC 3339 (UTC)"` (`definitions.json` L11). signalk-to-influxdb2 special-cases that string as non-numeric (`IX2/src/influx.ts` L464-473).

---

## 3. v2 Resources API: notes

- `GET /signalk/v2/api/resources/notes` returns **an object keyed by resource UUID**, not an array.
  - Query params: `limit`, `distance`, `bbox`, `position`, `zoom`, `href`, `provider`.
  - Sources: `SRV/src/api/resources/openApi.ts` L806-865; `SRV/docs/develop/rest-api/resources_api.md` L7, L62-65.
- Each entry is `NoteResponseModel` = `Note` ∩ `BaseResponseModel`.
- `Note` fields (`SRV/packages/server-api/src/typebox/resources-schemas.ts` L175-216):

| Field | Notes |
|---|---|
| `title?` | |
| `description?` | |
| `mimeType?` | |
| `url?` | |
| `properties?` | free-form object |
| `href?` | link to another resource |
| `position?` | `{latitude, longitude}` |

- `BaseResponseModel` adds **`timestamp`** ("ISO 8601 timestamp of when the resource was last **modified**") and **`$source`** (`resources-schemas.ts` L43-59).
- In the bundled default provider, `timestamp` is the storage **file's `mtime`** (`SRV/packages/resources-provider-plugin/src/lib/filestorage.ts` L136-137, L202-203). It is last-modified, not created. There is **no `createdAt`**. Other providers may differ: UNVERIFIED.
- Inconsistency: the older TS interface `Note` in `SRV/packages/server-api/src/resourcetypes.ts` L46-54 uses `name`, `geohash` and `href`, while the typebox schema uses `title`. Which field real notes carry depends on the writer: UNVERIFIED. Read both `title` and `name`.

Shape (assembled from the schemas above, not a captured response):
```json
{"<uuid>": {"title": "...", "description": "...", "position": {"latitude": 0, "longitude": 0},
            "timestamp": "2024-01-15T12:30:00.000Z", "$source": "resources-provider"}}
```

---

## 4. InfluxDB as used by Signal K

### 4.1 Which plugin HaLOS uses
- HaLOS Marine bakes **`signalk-to-influxdb2`** (InfluxDB v2) and **`signalk-questdb-history-provider`** into its signalk-server image (`HSD/plugins.list` L43-45). Entries are **unpinned**: whatever npm had at image build time (`HSD/Dockerfile` L56-57). App-store updates in the data volume override the baked copy (`HSD/plugins.list` L3-5; `HMC/AGENTS.md` L535-556).
- The signalk image is `ghcr.io/halos-org/signalk-server-docker:v2.33.0-halos.1` (`HMC/apps/signalk-server/docker-compose.yml` L8). InfluxDB is `influxdb:2.9.1` (`HMC/apps/influxdb/docker-compose.yml` L3).
- Plugin version: the tag `v2.33.0-halos.1` was committed 2026-09-21. On npm, `2.2.1` was published 2026-09-08 and `2.3.0` on 2026-09-25 (https://registry.npmjs.org/signalk-to-influxdb2). So the baked version is **probably 2.2.1**. This is UNVERIFIED (inferred from dates). The 2.3.0 changes are History API fixes, not write-format changes (https://github.com/tkurki/signalk-to-influxdb2/releases).
- HaLOS writes this plugin config: `org: "marine"`, `bucket: "marine"`, `url: http://localhost:8086`, `onlySelf: true`, `resolution: 1000`. `useSKTimestamp` is **not set**, so it defaults to false (`HMC/apps/signalk-server/prestart.sh` L417-431).
- History API default provider: if QuestDB is also installed, HaLOS points the history API at it (`HMC/apps/signalk-server/prestart.sh` L318-319; `HMC/AGENTS.md` L467-502). **A HaLOS boat may have history in QuestDB, not InfluxDB.** The QuestDB schema was not researched.

### 4.2 signalk-to-influxdb2 point model
Source: `IX2/src/influx.ts` L393-450; `IX2/src/plugin.ts` L160-181.
- **measurement = the Signal K path** (the empty path becomes `<empty>`; `influx.ts` L117).
- **tags**:
  - `context`: the full context string, e.g. `vessels.urn:mrn:imo:mmsi:...`;
  - `source`: `update.$source`;
  - `self`: `"true"` on self-vessel points. The code sets `SELF_TAG_VALUE = 'true'` (`influx.ts` L25-26), while the README says the value is `t` (`IX2/README.md` L22). The README is stale.
  - Position points also get `s2_cell_id`.
- **fields**:

| Value kind | Field(s) written |
|---|---|
| number | `value` (float). NaN is dropped. |
| string | `value` (string) |
| boolean | `value` (boolean) |
| other object | `value` = JSON string |
| `notifications.*` | always typed as object, so a JSON string in `value` |
| `navigation.position` | `lat`, `lon` floats (no `value` field) plus the `s2_cell_id` tag |
| `navigation.attitude` | split into measurements `navigation.attitude.pitch` / `.roll` / `.yaw`, each with a `value` float |
| `null` | dropped |

- Type is decided per path from the schema `units` (any unit other than `RFC 3339 (UTC)` means numeric), otherwise from the JS type of the first value, and then **cached per path** (`influx.ts` L452-473).
- **Time**: Signal K `timestamp` only if `useSKTimestamp` (default **false**). Otherwise no timestamp is set on the point, so it is effectively write/insert time (`influx.ts` L401-404, `PluginConfigSchema` doc at L96-103). Exactly where the time is assigned (client or server) is UNVERIFIED.
- `onlySelf` defaults to true, so AIS targets are not stored (`influx.ts` L70-75). `resolution` ms throttles writes per context+path+source (`influx.ts` L104-110, L282-286).
- Since 2.2.0, `sourcePolicy: 'all'` records every source (the `unfilteredDelta` event, server ≥ 2.28). Otherwise only the priority-preferred source is recorded (`IX2/src/plugin.ts` L29-34, L77-81, L182-193).

### 4.3 signalk-to-influxdb (v1, InfluxDB 1.x), for comparison
Source: `IX1/src/skToInflux.js` L23-140.
- measurement = path. Tags are `context` and `source` (the `$source`, or `getSourceId(source)`).
- **Fields differ by type**: `value` (number), `stringValue`, `boolValue`, `jsonValue` (object as JSON).
- Position: `jsonValue` = `{"longitude","latitude"}` JSON, plus an optional second point with `lon` / `lat` floats if `separateLatLon` is set.
- Time: the delta timestamp by default (`honorDeltaTimestamp = true`).
- `path: ""` objects are split into one measurement per key.

---

## 5. Signal K History API v2
Source: `SRV/docs/develop/rest-api/history_api.md`; types in `SRV/packages/server-api/src/history.ts`.

- **`GET /signalk/v2/api/history/values`** parameters:

| Parameter | Meaning |
|---|---|
| `paths` | **Required.** Comma-separated `path[:aggregate[:param]][\|sourceRef]`. Aggregates: `average`, `min`, `max`, `first`, `last`, `mid`, `middle_index`, `sma`, `ema`. The default is `average`, except `navigation.position`, which defaults to `first`. The `\|sourceRef` suffix is parsed in `SRV/src/api/history/index.ts` L581-618. |
| `context` | Default `vessels.self` |
| `from` / `to` / `duration` | ISO 8601 times; `duration` in seconds or an ISO duration such as `PT1H` |
| `resolution` | Seconds, or `1s` / `1m` / `1h` / `1d` |
| `provider` | Which history provider plugin to ask |
| `sourcePolicy=all` | Returns one series per source, with `$source` set in `values[]` (`history.ts` L41-50) |

- `rad` paths are vector-averaged.
- Response:
```json
{"context": "vessels.urn:mrn:imo:mmsi:123456789",
 "range": {"from": "2018-03-20T09:12:28Z", "to": "2018-03-20T09:13:28Z"},
 "values": [{"path": "navigation.speedOverGround", "method": "average"},
            {"path": "navigation.position", "method": "first"}],
 "data": [["2023-11-09T02:45:38.160Z", 13.2, [24.94, 60.17]],
          ["2023-11-09T02:45:39.160Z", 13.4, null]]}
```
- Each row is `[timestamp, ...one value per values[] entry]`, and missing values are `null`. **Positions are `[longitude, latitude]` (GeoJSON order)**, not the object form.
- Other endpoints:
  - `GET /signalk/v2/api/history/contexts` returns `string[]`.
  - `GET /signalk/v2/api/history/paths` returns `string[]`.
  - `GET /signalk/v2/api/history/_providers` returns `{id: {isDefault}}`.
  - `GET /signalk/v2/api/history/_providers/_default` returns the default provider.
  - `POST /signalk/v2/api/history/_providers/_default/{id}` sets it.
  - The first provider to register becomes the default.

---

## Spec assumptions vs reality

Each item is marked **WRONG**, **PARTLY WRONG**, **VERSION-DEPENDENT** or **UNVERIFIED**.

### signalk-parquet

1. **Timestamps are strings, not TIMESTAMP (WRONG if timestamp type was assumed).**
   - `received_timestamp` and `signalk_timestamp` exist but are **UTF8 (VARCHAR) ISO-8601 strings**. SQL must cast: `CAST(signalk_timestamp AS TIMESTAMP)` / `::TIMESTAMPTZ`. Comparing strings against ISO literals only works if every value has the same precision and `Z` suffix, and `signalk_timestamp` keeps the source's precision.
   - Only the aggregate tiers' `bucket_time` is a real TIMESTAMP.
2. **`value` is not always present (PARTLY WRONG).** Object-path files have **no** `value` column. Its type is inferred **per file** (DOUBLE, BOOLEAN or UTF8) and can drift across days. Never INT64, despite the README.
3. **`value_*` columns are top-level primitives only (CONFIRMED with limits).** They exist for objects, but nested objects and arrays (e.g. notification `method`, v2 `status`) are not flattened.
4. **There is no `value_json` in current files (VERSION-DEPENDENT).** The README lists it, but the current writer drops it from the schema. Older files may have it. Use `union_by_name=true` and do not depend on it.
5. **`$source` is called `source_label` (WRONG).** There is no `$source` column. The sourceRef is `source_label`; also available are `source` (JSON string), `source_type`, `source_pgn` (UTF8 in code; the README says number), `source_src` and `meta` (JSON per row).
6. **`context` exists as a data column (CONFIRMED).** It also appears as a lossy Hive directory segment. Use the column.
7. **Layout (PARTLY WRONG if per-path flat or per-file-per-day was assumed).**
   - The real layout is `tier=/context=/path=/year=/day=DDD/` (UTC day-of-year, by **received** time).
   - Files are named by **export** time (`signalk_data_YYYY-MM-DDTHHMM.parquet`).
   - The self vessel's directory uses the resolved URN, not `vessels__self` (code vs. README; UNVERIFIED on disk).
   - Compacted files `year=YYYY/year_compact_*.parquet` have no `day=` segment.
   - `quarantine/`, `failed/`, `processed/` and `repaired/` must be excluded.
8. **DuckDB needs special flags (MUST-FIX for teammate SQL).** Use `read_parquet(..., hive_partitioning=false, union_by_name=true)`. Without `hive_partitioning=false`, the sanitized `context=` directory value shadows the real `context` column. Do not use `**/*.parquet`. `year=*/day=*/*.parquet` misses compaction output.
9. **Parquet lags by up to ~24 h (WRONG if freshness was assumed).** Recent data sits only in `buffer.db` (SQLite) until the daily export.
10. **Aggregate tiers have a different schema (NOTE).** Columns are `bucket_time`, `value_avg/min/max`, `sample_count`, `first_timestamp`, `last_timestamp`, `value_sin_avg/cos_avg`. There are no source columns, and buckets use received time.
11. **Not verified against real files (UNVERIFIED).** I did not open a real `.parquet` written by 1.0.x. Before freezing SQL, run `DESCRIBE SELECT * FROM read_parquet('<one raw file>', hive_partitioning=false)` on a real file.

### Delta stream

12. **`source` and `$source` (PARTLY WRONG).** The schema says an update has **either** `source` **or** `$source`. signalk-server populates both internally. Treat both as optional and derive `$source` from `source` when it is absent (the `getSourceId` rules in §2.1).
13. **Empty path and null values (NOTE).** `path: ""` with an object value (static vessel data) is legal. `value: null` means invalid data or a cleared notification.
14. **Subscription fields (CONFIRMED).** `subscribe=none`, `period`, `policy` (instant/ideal/fixed, default ideal), `minPeriod` (instant only) and `format` match the spec.
15. **Access requests (PARTLY WRONG).**
    - The poll href in signalk-server is `/signalk/v1/requests/<id>`. Use the returned `href` verbatim and never construct it.
    - A security-disabled server returns 404, not 501.
    - An optional `permissions` field can be sent: `readonly`, `readwrite` or `admin`.
16. **Notification states (PARTLY WRONG).**
    - The valid states are `nominal, normal, alert, warn, alarm, emergency`, so **`nominal` is valid**.
    - The v2 Notifications API adds `id`, `status{...}`, `createdAt`, `position` and `data`, and clears to `state: "normal"` instead of `null`. Handle both clear styles.
17. **Meta fields (PARTLY WRONG).**
    - `units`, `displayName`, `description` and `zones` are confirmed. The spec also has `shortName`, `longName`, `enum`, `properties`, `displayScale`, `timeout` and the `*Method` arrays.
    - `description` is schema-required but may be missing in practice (UNVERIFIED).
    - `displayUnits` is not in spec 1.8.4.
18. **Units (CONFIRMED).** The list in §2.6 is per spec 1.8.4. Time-valued paths use the units string `"RFC 3339 (UTC)"`.

### Resources: notes

19. **Notes response shape (PARTLY WRONG).**
    - The response is an object keyed by UUID, not an array.
    - Timestamp field: `timestamp` = last-modified (file mtime in the default provider). There is no creation time.
    - Name field: `title` per the schema, `name` per the legacy TS interface.

### InfluxDB

20. **Point model (CONFIRMED for signalk-to-influxdb2).** Measurement is the path, tags are `context` and `source` (plus `self`), and the field is `value`.
    - **Exception: positions** are fields `lat` / `lon` with an `s2_cell_id` tag, and no `value`.
    - Attitude is split into three measurements.
    - Strings and booleans go in `value` with their native field type; objects and notifications are JSON strings.
21. **Point time is insert time by default (WRONG if delta timestamps were assumed).** Default `useSKTimestamp=false`, which HaLOS keeps. HaLOS also sets `resolution: 1000` (≤1 point/s per context+path+source) and `onlySelf: true` (no AIS).
22. **The v1 plugin is different (VERSION-DEPENDENT).** signalk-to-influxdb (1.x) uses `value` / `stringValue` / `boolValue` / `jsonValue` fields and stores positions as `jsonValue`.
23. **HaLOS plugin version (UNVERIFIED).** It is probably 2.2.1, unpinned and overridable by app-store updates. HaLOS may route the History API to **QuestDB** instead (not researched).

### History API

24. **History API response (CONFIRMED with one trap).** Positions come back as `[lon, lat]` arrays, not objects.

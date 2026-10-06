# signalk-lume-ti

Signal K server plugin and web application for **Lume TI** — high-resolution timeseries store and query engine.

Supervises `lume ti ingest --serve` as a managed child process inside the Signal K environment, providing continuous live telemetry recording, WAL crash safety, automated shard sealing, and an in-browser SQL query console.

---

## Architecture

- **Supervision**: Runs `lume ti ingest --signalk ws://127.0.0.1:3000 --store <dataDir>/lume-ti --serve --bind 127.0.0.1 --port 5863` as a child process. Automatically restarts on crash with exponential backoff and gracefully shuts down via `SIGTERM` (which flushes in-memory WAL buffers and bucket accumulators before exit).
- **Storage**: Columnar parquet-backed TI store located under `/home/node/.signalk/lume-ti` (via `app.getDataDirPath()`).
- **Security & Loopback Proxy**: The query server binds `127.0.0.1` only. The browser communicates exclusively via the Signal K plugin router (`/plugins/signalk-lume-ti/api/query`, `/api/schema`, `/api/status`), avoiding open network ports or CORS exposure.
- **Authentication**: Supports Signal K device access-request flow (`POST /signalk/v1/access/requests`, poll until approved) with persistent token storage in `<dataDir>/token.txt`. If Signal K runs with anonymous read-only access enabled (`readOnlyAccess: true`, standard in local marine setups), ingest connects immediately without requiring a token.
- **Webapp**: Full SQL console, preset queries, CSV export, and live schema browser accessible via Signal K Webapps menu (`/signalk-lume-ti/`).

---

## Target Environment & Libc Compatibility

- **Device**: Raspberry Pi 5 (8 GB) running HaLOS (Halos-Marine-RPI).
- **Signal K Container**: `signalk-server-docker` (v2.31.1).
  - OS: Ubuntu 24.04.4 LTS (Noble Numbat)
  - Architecture: `aarch64` / `arm64`
  - C Library: `glibc 2.39`
  - Node.js runtime: Node v18+ / v20+
- **Host**: Debian 13 (Trixie), `glibc 2.41`, `aarch64`.
- **Libc Compatibility**: Because the container runs standard Ubuntu 24.04 with `glibc 2.39`, a musl static build is **not required**. Any `aarch64-unknown-linux-gnu` binary compiled on Debian/Ubuntu with `GLIBC <= 2.39` will run directly inside the container.

---

## Installation into Signal K Docker Container

Signal K server plugins and webapps are installed inside the Signal K data directory, which is host-mounted at `/home/node/.signalk`:

### 1. Install Plugin Package
Inside the container (or on the host at the mounted volume directory `/home/node/.signalk`):

```bash
cd /home/node/.signalk
npm install /path/to/signalk-lume-ti
```

Or copy the plugin directory into `node_modules`:
```bash
cp -r /path/to/plugins/signalk-lume-ti /home/node/.signalk/node_modules/
```

### 2. Install Lume Binary
Place the native `aarch64` `lume` binary in either:

1. **Plugin bundled location**:
   ```bash
   mkdir -p /home/node/.signalk/node_modules/signalk-lume-ti/bin/linux-arm64
   cp /path/to/lume /home/node/.signalk/node_modules/signalk-lume-ti/bin/linux-arm64/lume
   chmod +x /home/node/.signalk/node_modules/signalk-lume-ti/bin/linux-arm64/lume
   ```

2. **Or system PATH inside the container**:
   ```bash
   cp /path/to/lume /usr/local/bin/lume
   chmod +x /usr/local/bin/lume
   ```

3. **Or custom path**: Set `lumePath` in the plugin configuration UI.

### 3. Enable in Admin UI
1. Navigate to Signal K Server Admin UI: `http://<pi-ip>:3000/admin/`.
2. Go to **Server** -> **Plugin Config** -> **Lume TI**.
3. Toggle the plugin to **Enabled**.
4. Configure optional settings:
   - **Signal K WebSocket URL**: `ws://127.0.0.1:3000` (default)
   - **Query Server Port**: `5863` (default)
   - **Custom binary path**: leave empty to auto-detect bundled binary or PATH
5. Click **Submit**.

---

## Documents and chart pins

`lume ti ingest` polls Signal K Resources notes every 60 seconds with the ingest token, or anonymously. The optional signalk-logbook plugin is detected through its v2 logentries API; legacy date-based logbook routes are supported as a fallback. Successful complete snapshots create/update/delete owned documents; failures preserve the last successful documents and never stop telemetry. A note without a timestamp/creation time uses a stable first-seen timestamp. Documents before the existing 2020 epoch are rejected rather than silently shifted; rejected attempts are counted in ingest status and the diagnostics panel.

In the SQL console, run an `intervals()` query with `start`/`end` columns, then click **Pin to chart**. Notes carry the interval range and query text in `properties`, tagged with ownership group `lume-ti`. This tag is used for cleanup; it does not create a Freeboard display-selection group resource. Include `latitude`/`longitude` columns or fill the optional coordinate fields to place an interval note on the chart; unlocated notes remain in the Resources list. A query with a single literal `in_bbox(lat_min, lon_min, lat_max, lon_max)` also creates a GeoJSON region and links its notes to it; boxes crossing the dateline are split.

Pin/unpin uses the logged-in browser's same-origin Signal K session. Sign in to Signal K if its security settings require write access. The ingest token remains read-only. Neither startup nor running a query writes chart resources. **Unpin all lume-ti** asks for confirmation and removes only notes/regions marked as created by this feature. Truncated results must be narrowed before pinning. A failed partial write reports how many resources were written; unpin can clean them up. Resource changes do not write any vessel/data path.

## Signal K History API (Signal K 2.31+)

The plugin registers a read-only provider named `signalk-lume-ti` with
`app.registerHistoryApiProvider`, and unregisters it on stop. Signal K owns
routing, authentication and parameter parsing for these standard endpoints:

- `GET /signalk/v2/api/history/values`
- `GET /signalk/v2/api/history/contexts`
- `GET /signalk/v2/api/history/paths`

Select it explicitly with `provider=signalk-lume-ti`, or choose it as the
server's default History API provider in Admin → Data → Preferences.
Existing KIP/Freeboard requests without a provider then use Lume. Provider
registration itself does not change the server's default.

For example, open these URLs on your authenticated Signal K server:

```text
/signalk/v2/api/history/values?provider=signalk-lume-ti&paths=navigation.speedOverGround:average&duration=PT1H&resolution=30s
/signalk/v2/api/history/values?provider=signalk-lume-ti&paths=navigation.position:first&duration=PT1H&resolution=30s
/signalk/v2/api/history/contexts?provider=signalk-lume-ti&duration=PT1H
/signalk/v2/api/history/paths?provider=signalk-lume-ti&duration=PT1H
```

Values return `{context, range:{from,to}, values:[{path,method}], data:[[timestamp,...values]]}`;
contexts and paths return string arrays. Missing values in a populated time
bin are null; empty bins are omitted. The time range is half-open, and bins
are anchored at its start. `vessels.self` resolves using Signal K's own UUID.
Duration-only, from/to, from/duration, to/duration and from-until-now work.

Resolution is in seconds and maps to `date_bin`. Numeric average/min/max
roll up retained `@mean/@min/@max`; first/last select the earliest/latest
retained `@last` bucket. A coarse store without `@last` rejects first/last
instead of substituting means. These are bucket-level historical values:
raw samples and their original timestamps cannot be recovered from a
10-second store. If the configured 1-second HR store has the field, it is
preferred for sub-10-second resolution; its `@last` values also support
numeric roll-ups. Position first/last combines retained latitude/longitude
from the same bucket and returns Signal K position objects for track clients.

Unsupported aggregate methods/parameters and per-source requests fail
explicitly. Limits are 16 paths, 100,000 requested bins, and 30 seconds per
backend request. Queries are split at bin boundaries, and truncated backend
results are retried in smaller chunks; an unsplittable truncated bin fails
rather than returning incomplete history. All backend operations are SELECTs
against the managed Lume HTTP server on loopback. Telemetry lag remains the
ingest lag shown in status.

### Verify in KIP

1. Confirm the explicit-provider speed URL above returns populated data for a
   time range already recorded by Lume.
2. Set Lume as Signal K's default History provider. In KIP, add a Data Chart
   using a recorded numeric path, select a historical range and reload it.
   KIP's [History support](https://github.com/mxtommy/Kip/blob/master/CHANGELOG.md)
   uses the History API to populate charts. Widget history is available via
   right-click/two-finger tap in supported KIP versions.
3. In browser developer tools, check the chart's `/history/values` request
   and its response. Compare the first/last timestamps and sample values with
   the explicit-provider URL using the same range/resolution. If KIP supplies
   a different provider explicitly, select Lume there or remove that override.
4. For tracks, check the position URL returns latitude/longitude objects;
   select Lume for Freeboard-SK history and request the same recorded range.

The automated checks below verify the provider and real Lume transport.
Actual Pi/KIP/Freeboard UI verification remains a deployment check.

## Verification & Testing

Run unit tests via Node's native test runner:

```bash
cd plugins/signalk-lume-ti
npm test
```

Test coverage includes:
- Process supervision and argument construction (`--signalk`, `--store`, `--serve`, `--port`, `--bind`, `--token`).
- Automatic crash recovery with exponential backoff.
- Clean shutdown via `SIGTERM` triggering WAL synchronization.
- Signal K device access-request flow and polling.
- Status API and query/schema HTTP proxying.
- History provider registration, range forms, aggregate mapping, HR selection,
  position pairing, response alignment, truncation splitting and loopback JSON transport.

Run the provider against a real Lume server and generated TI fixture:

```bash
CARGO_INCREMENTAL=0 cargo test --features ti --test ti_http history_provider_real_http
```

This test starts Lume on an ephemeral loopback port and invokes the Node
provider against its actual schema/query endpoints. Node must be on PATH.


## PostgreSQL / Grafana on the HaLOS Pi

PostgreSQL is off by default. In the Lume webapp's **PostgreSQL / Grafana**
tab, log in as a Signal K administrator, enter a password and save:

| Option | Default | HaLOS Grafana setting |
|---|---|---|
| enablePg | false | true |
| pgPort | 5864 | 5864 (avoids PostgreSQL's 5432) |
| pgUser | grafana | grafana |
| pgBind | 127.0.0.1 | **172.17.0.1 (docker0)** |

The lead verified the live Pi with docker inspect: Signal K/Lume is host-networked;
Grafana shares InfluxDB's bridge namespace, so Grafana's localhost is not the Pi.
Its ExtraHosts maps halos.local to host-gateway, resolving to 172.17.0.1.
Use halos.local:5864 in Grafana, and bind pgwire specifically to that docker0 IP.
Do not use 0.0.0.0. The plugin rejects wildcard PG addresses. HTTP always stays
127.0.0.1:5863, preserving its existing proxy and History provider.

The password exists only in the dedicated form/request. Printable ASCII
passwords (1–1024 characters) are supported so Node and PostgreSQL SASLprep
cannot diverge. Node crypto.pbkdf2 derives SCRAM-SHA-256 with 4096 iterations
and a fresh 16-byte random salt before app.savePluginOptions is called.
Only pgVerifier and an empty pgPassword are persisted; the form clears its
password on every submission, including failures. Blank keeps the current
verifier. Saving restarts a running plugin; a disabled Signal K plugin remains
disabled until enabled in Admin.

Do not enter passwords in Signal K's ordinary plugin options: its
[v2.31 configuration route](https://github.com/SignalK/signalk-server/blob/v2.31.0/src/interfaces/plugins.ts)
persists options before start(), without a pre-save plugin hook. The schema
therefore contains safe options and the verifier only. The dedicated
POST /plugins/signalk-lume-ti/api/pg/config route checks Signal K's
[allowConfigure / authenticated admin principal](https://github.com/SignalK/signalk-server/blob/v2.31.0/src/tokensecurity.ts).
Readonly/readwrite users get 403, anonymous users 401; Signal K's explicitly
disabled security strategy retains its own open configuration behavior.
Requests must be JSON, at most 4096 bytes. Pre-parsed requests require a
bounded Content-Length; raw chunked requests are counted as read.
No password or request body is echoed or logged.

The plugin atomically writes a verifier-only ti.toml in its data directory
with mode 0600 and supplies --pg-auth-config <path> to Lume. This **replaces**
store_root/ti.toml's auth section, **without merging users**, and ignores all
other sections of the auth file. The store's ingestion/query settings are
unaffected. Unix rejects group/world-accessible auth files; secure the parent
directory too. Changes take effect on restart. --pg-bind applies only to PG
and defaults to --bind when omitted from the CLI.

Copy [the provisioning file](../../bench/grafana/lume-ti-datasource.yaml) into
/etc/grafana/provisioning/datasources/ in the Grafana container and provide
LUME_PG_PASSWORD through its environment/secret configuration, matching the
password set in the webapp. It uses secureJsonData.password with
$__env{LUME_PG_PASSWORD}, following the Pi's existing Influx datasource pattern.
Set enablePg first, then restart Grafana and use datasource Save & Test.
sslmode disable is only for loopback/local Docker-host transport; do not
expose this listener to the LAN or shore.

On the Pi, with psql and Python 3 installed and PGPASSWORD or PGPASSFILE set:

```bash
bash tests/pg_smoke.sh halos.local:5864 grafana ti
```

This runs twenty source-pinned SQL cases: Grafana connection/metadata probes,
pg_catalog and psql describe probes, expanded $__timeFilter(ts) and
$__timeGroup(ts,'1m') expressions, date_bin, timestamps/doubles and typed nulls.
It also invokes psql's actual \\d telemetry and rejects a wrong SCRAM password.
Runtime macro bounds use the last 24h. Unit tests replay the same twenty cases
against real Lume on loopback and decode timestamp/double rows through
tokio-postgres; the Node-derived deterministic verifier authenticates there.
The live Pi/Grafana Save & Test and psql smoke remain deployment checks.

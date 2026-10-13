<p align="center"><img src="https://raw.githubusercontent.com/DeepBlueDynamics/lume/main/assets/lume-icon.svg" width="96" alt="Lume"></p>

# Lume TI for Signal K

**Your boat's history, kept on the boat, and easy to ask about.** Lume TI records what your Signal K server sees, in 10-second buckets, on your own hardware. Then it gives you a SQL console, an Ask tab, a Library for your manuals, and a Signal K History API provider for other apps. It runs [Lume](https://github.com/DeepBlueDynamics/lume), one small Rust program, beside Signal K.

## What you get

- **History of your Signal K data**, stored on the server in 10-second buckets.
- **Webapps → Lume TI:**
  - a SQL console with ready-made queries, such as **Recent SOG & Wind**, plus CSV export and a schema browser;
  - an **Ask** tab for plain-language questions about your data, using a cloud model (`glm-5.3:cloud` at ollama.com, needs a key) or a model on your own network;
  - a **Library** for manuals and PDFs, searchable offline.
- **A History API provider** named `signalk-lume-ti`, so Freeboard, KIP and other apps can read that history.
- **Optional extras:** a PostgreSQL endpoint for Grafana, and query results pinned to the chart as notes.
- **No new ports on the boat network.** The query service listens on `127.0.0.1` only.

## Install

From the Signal K App Store:

1. In Signal K admin, open **Appstore → Available**, find **Lume TI**, and press **Install**.
2. Press **Restart** at the top right.
3. Open **Server → Plugin Config → Lume TI**, tick **Enabled**, and press **Submit**.
4. Open **Security → Access Requests** and approve the Lume TI device. Lume records nothing until you do, unless the server allows anonymous read-only access.

Or with npm, as the user Signal K runs as:

```sh
cd ~/.signalk
npm install signalk-lume-ti
```

Then restart Signal K and do steps 3 and 4.

npm installs the plugin plus one `lume` program for your machine: `signalk-lume-ti-linux-arm64` on a Raspberry Pi, `signalk-lume-ti-linux-x64` on a PC. That's about 35 MB. You need 64-bit Linux (arm64 or x64) with glibc 2.35 or newer, Signal K 2.31 or newer, and Node 18 or newer. 32-bit Raspberry Pi OS, Victron Venus OS, macOS and Windows are not supported. No compiler and no install script are involved.

**On HaLOS,** use the App Store as above, or the `marine-lume` package when the boat has no internet at install time. Use one of them, not both. [Install Lume on HaLOS](https://github.com/DeepBlueDynamics/lume/blob/main/docs/HALOS.md) walks through both, including the Cockpit login.

**Offline on another Signal K server,** install the plugin tarball and the matching program tarball from the [release](https://github.com/DeepBlueDynamics/lume/releases/latest) together, as [section 9 of the install guide](https://github.com/DeepBlueDynamics/lume/blob/main/docs/HALOS.md#9-install-on-other-64-bit-linux-signal-k-servers) shows.

## Check it works

- In **Server → Plugin Config → Lume TI**, the status starts with `Running` and shows how much has been ingested.
- Open **Webapps → Lume TI**, then the SQL console, and press **Recent SOG & Wind**. Rows appear once the boat has sent data.
- Problems, from "page not found" to "no boat data", are covered in [Troubleshooting](https://github.com/DeepBlueDynamics/lume/blob/main/docs/HALOS.md#8-troubleshooting).

Questions and bugs: [GitHub issues](https://github.com/DeepBlueDynamics/lume/issues). License: BSD-3-Clause.

---

## How it works

### Architecture

- **Supervision**: Runs `lume ti ingest --signalk ws://127.0.0.1:3000 --store <dataDir>/lume-ti --serve --bind 127.0.0.1 --port 5863` as a child process. Automatically restarts on crash with exponential backoff and gracefully shuts down via `SIGTERM` (which flushes in-memory WAL buffers and bucket accumulators before exit). Supports `--otlp` and `--otlp-token-file <path>` when OTLP is enabled, and `--pg`, `--pg-bind`, `--pg-auth-config`, `--pg-require-tls` when PostgreSQL is configured.
- **Storage**: Columnar parquet-backed TI store located under `/home/node/.signalk/lume-ti` (via `app.getDataDirPath()`).
- **Security & Loopback Proxy**: The query server binds `127.0.0.1` only. The browser communicates exclusively via the Signal K plugin router (`/plugins/signalk-lume-ti/api/query`, `/api/schema`, `/api/status`), avoiding open network ports or CORS exposure. Any configured `serveBind` is ignored for the query server to prevent unauthenticated network exposure.
- **Authentication**: Supports Signal K device access-request flow (`POST /signalk/v1/access/requests`, poll until approved) with persistent token storage in `<dataDir>/token.txt`. If Signal K runs with anonymous read-only access enabled (`readOnlyAccess: true`, standard in local marine setups), ingest connects immediately without requiring a token.
- **Webapp**: Full SQL console, preset queries, CSV export, live schema browser, and Ask tab accessible via Signal K Webapps menu (`/signalk-lume-ti/`).

---

### Target environment and glibc

- **Device**: Raspberry Pi 5 (8 GB) running HaLOS (Halos-Marine-RPI).
- **Signal K Container**: `signalk-server-docker` (v2.31.1).
  - OS: Ubuntu 24.04.4 LTS (Noble Numbat)
  - Architecture: `aarch64` / `arm64`
  - C Library: `glibc 2.39`
  - Node.js runtime: Node v18+ / v20+
- **Host**: Debian 13 (Trixie), `glibc 2.41`, `aarch64`.
- **Libc Compatibility**: Because the container runs standard Ubuntu 24.04 with `glibc 2.39`, a musl static build is **not required**. Any `aarch64-unknown-linux-gnu` binary compiled on Debian/Ubuntu with `GLIBC <= 2.39` will run directly inside the container.

To deploy the plugin and the arm64 program straight from a workstation, use `scripts/deploy-pi.sh <ssh-host>` or `scripts/provision-pi.sh <ssh-host>`.

### Build a release package

Both architectures must be built from the same reviewed revision before
publication. Build arm64 natively inside the Pi's Ubuntu 24.04 Signal K
container, or a matching build container, rather than on the Pi's Debian 13
host (glibc 2.41). Thin LTO and symbol stripping reduce release size:

```bash
CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_LTO=thin \
CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --locked --release --features ti
```

The same command builds x64 natively on glibc 2.36 Linux. A cross-build
alternative is [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)
with `--target aarch64-unknown-linux-gnu.2.36`; install the Rust target and
Zig toolchain first, and inspect the resulting binary's actual requirements.

From the repository root:

```bash
bash scripts/package-plugin.sh \
  --arm64 /path/to/arm64/lume --x64 /path/to/x64/lume \
  --output /path/to/release-output
```

The builder requires Node, npm and readelf. Strip binaries on their build host
with native `strip --strip-all`, or use release profile `strip = "symbols"`.
Already-stripped inputs are accepted only when `.symtab` and `.debug_*`
sections are absent. Unstripped inputs additionally require llvm-strip or GNU
strip for each target architecture. `--strip-tool /path/to/llvm-strip` selects a
cross-architecture tool explicitly. It strips staged copies, preserving the
input artifacts, checks ELF architecture and Linux interpreter before and
after stripping, and rejects any GLIBC requirement above 2.39. It also runs
`file` when available; ELF headers and readelf remain authoritative when
that command is absent. It never uses architecture-changing generic objcopy.

The output contains three npm tarballs, the plugin and
`signalk-lume-ti-linux-arm64`/`-x64`, and `package-report.json`, recording
input/output sizes, SHA-256 checksums, glibc requirements and the packed file
lists. Packing is offline. Before packing, a check verifies each binary
against its package's manifest and that the plugin pins both binary packages
to its own version. The release workflow publishes the binary packages to npm
before the plugin. [Publishing Lume to npm](https://github.com/DeepBlueDynamics/lume/blob/main/docs/NPM.md) covers trusted
publishing and setting up a new package. Installation works with Signal K's
[`--ignore-scripts` policy](https://github.com/SignalK/signalk-server/blob/v2.31.0/src/modules.ts).

The included 128×128 SVG is a placeholder. Replace `public/icon.png` and set
`signalk.appIcon` to `./icon.png` in package.json for the public icon.
The builder mirrors it at the package root because Signal K's
[plugin publishing](https://github.com/SignalK/signalk-server/blob/v2.31.0/docs/develop/plugins/publishing.md)
and [webapp](https://github.com/SignalK/signalk-server/blob/v2.31.0/docs/develop/webapps.md)
icon paths differ.

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
retained `@last` bucket. On store creation/open, the plugin atomically adds
`[profiles] opt_in = ["last"]` only when that key is absent. Existing explicit
lists (including an empty list), other settings and comments are preserved;
older sealed data is not rewritten.

A `:last` request uses retained `@last` where present and falls back to
`@mean` for older buckets that lack it. If any fallback is used, that path's
response descriptor includes `method_used: "mean"` and a note explaining
the mixed coverage, while `method: "last"` preserves the requested method.
The latest bucket mean in each resolution bin is used, not a raw last sample.
This keeps SKIP/KIP charts populated while making the approximation explicit.
A `:first` request still requires retained `@last`. These are bucket-level historical values:
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
- Process supervision and argument construction (`--signalk`, `--store`, `--serve`, `--port`, `--bind`, `--token`, `--otlp`, `--otlp-token-file`, and PostgreSQL flags).
- Automatic crash recovery with exponential backoff.
- Clean shutdown via `SIGTERM` triggering WAL synchronization.
- Signal K device access-request flow and polling.
- Status API and query/schema HTTP proxying.
- Ask tab chat execution, schema defaults, and API key environment isolation (`OLLAMA_API_KEY` in child env only).
- OTLP receiver schema defaults, loopback validation, and error reporting.
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

Copy [the provisioning file](https://github.com/DeepBlueDynamics/lume/blob/main/bench/grafana/lume-ti-datasource.yaml) into
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

## Ask Tab & Cloud LLM (`lume chat`)

The plugin webapp includes an **Ask** tab (`lume chat`) enabling natural language questions over telemetry:
- **Default model**: `glm-5.3:cloud` via `https://ollama.com` directly (`chatApiKeyFile` setting).
- **Key security**: The plugin reads `chatApiKeyFile` (mode 0600 on the host) at spawn and passes `OLLAMA_API_KEY` in the child process environment only. The key is never placed on argv, in logs, or in configuration files (SETUP §13).
- **Fallbacks**: Local Pi Ollama (`http://127.0.0.1:11434`) or a laptop on the LAN are supported via the comma-separated `chatOllamaUrl` setting.

## OTLP Agent Telemetry Receiver

When enabled, the supervisor passes `--otlp` to expose OpenTelemetry HTTP/JSON endpoints (`/v1/metrics`, `/v1/logs`) directly on the supervisor query server (`127.0.0.1:5863`):
- **`otlpEnabled`** (boolean, default `false`): enables the OTLP receiver.
- **`otlpTokenFile`** (string path, optional): optional bearer token file path (mode 0600).
- **Loopback enforcement**: The supervisor query server is strictly pinned to `127.0.0.1`, ignoring any `serveBind` configuration, to prevent unauthenticated network exposure.

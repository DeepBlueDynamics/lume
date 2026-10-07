# Lume TI — Developer and agent setup

Read this first if you are joining the Lume TI build. Current assignments are in [STATUS.md](STATUS.md).
The plan itself is in [README.md](README.md).

Items marked *(unconfirmed)* are conventions the docs keeper has not verified. Ask the lead before relying on them.

## 1. The repo in one paragraph

Lume is a Rust crate (`lume` 0.12.0, edition 2021): `src/lib.rs` plus a CLI in `src/main.rs`.
By default it has four direct dependencies (`tantivy-fst`, `ureq`, `serde`, `serde_json`) and a committed `Cargo.lock`.
The repo is now a Cargo workspace. The root `lume` package stays at `.`, and Lume TI (the telemetry index) lives in members under `crates/ti-*` (merged so far: `crates/ti-contracts`, `crates/ti-core`, `crates/ti-store`, `crates/ti-sql`, `crates/ti-ingest`, `crates/ti-bench`, `crates/ti-geo` and `crates/ti-sync`). The Signal K plugin lives in `plugins/signalk-lume-ti/` (Node, not a Cargo member). TI is compiled only behind the `ti` cargo feature, which is off by default.

**`ti-contracts` is frozen** (`96ac45d`). It holds the shared types and traits, the catalog, the Arrow schemas, the WAL/shard envelopes, the `TiEngine` facade and the `ti.toml` schema (`config.rs`). The first contracts PR after the freeze, the D30 `[stores.*]` multi-store config (`fe5ea3a`), shows the process. Empty `stores` means the legacy single store. Otherwise a `"default"` store is required, each width must divide 3600, and retention is typed (`s/m/h/d` or `"forever"`). The spec/10 mirror of the new `config.rs` is still pending, so read the source until it lands. [spec/10](spec/10-contracts.md) mirrors its source, and [spec/14](spec/14-semantics.md) freezes the behavioral rules. **Any change to a boundary in it needs a contracts PR approved by the lead (integrator).** Build your lane against the crate as it is, and mock other lanes behind its traits. If you think a contract is wrong, mail the lead. Don't patch it in your lane.

**`ti-core`** (W1, `f7faf5f`) holds the in-memory bitmap rows (presence, set, BSI, count), the BSI algorithms, and the three-valued predicate evaluator with the `MemoryShard`/`MemorySource` fixtures. Its [README](../crates/ti-core/README.md) is the reference for the row and evaluator API, and for what is deliberately left to other lanes (Arrow `read` goes to W4, durable WAL/flush/seal to W2, the geo refinement under `NOT` to W6).
The golden SQL corpus is in `tests/golden/` (see its `README.md`).

**Shared data dir: `.lanes/data/`.** Generated correctness data and expected outputs go here. They never go in a lane clone's tracked files or in git. The dir is under `.lanes/`, so it is gitignored. All containers and the host can see it: in a container it is `/workspace/lume/.lanes/data/`, and on the host it is `C:\Users\kordl\Code\DeepBlueDynamics\lume\.lanes\data\`. The correctness set (~1.9 GB) goes in `.lanes/data/correctness/`. It is **ready**: regenerated on the host from `71b7fcd`, 1.8 GB, 11,508 files. Its manifest hash is in `.lanes/data/correctness.sha256` (`edfef2d8…`). Check against that file before you rely on the data. W3 keeps its own smoke set in `.lanes/data/w3-smoke/`. Treat data another lane wrote as read-only unless you own it *(unconfirmed convention)*.
The work happens on branch `plan/lume-ti`, not `main`.

## 2. Build and test the existing crate

These are the commands CI runs (`.github/workflows/ci.yml`, on Ubuntu, macOS and Windows, stable toolchain):

```sh
cargo build --locked --verbose
cargo test --locked --verbose
cargo clippy --all-targets || true   # informational only; the existing crate has warnings
```

- At `8e87a11` the root crate has **46 tests**. The 10 search golden outputs in `tests/search_golden/` must stay byte-identical. Check them with `tests/search_golden/capture.sh [lume-binary]`, which defaults to `./target/debug/lume` and verify mode, so build first.
- Always pass `--locked`. CI fails if `Cargo.lock` would change.
- There is no `rust-toolchain` file yet. The host build pane runs rustc **1.96.1** and the containers run **1.99**, and their clippy lints differ. A pinned `rust-toolchain.toml` has been proposed to the user (unconfirmed until accepted). The strict TI CI job pins Rust **1.96.0** for itself only.
- CI runs only on pushes and PRs to `main`. Lane branches and `plan/lume-ti` get **no CI**, so run the commands above yourself before you report a commit.
- **Strict TI CI job** (`ti` in `ci.yml`, `ec48673`; legacy jobs unchanged), on Ubuntu with Rust 1.96.0: `cargo fmt --check` on the 8 TI crates plus the root TI files, `clippy -D warnings` on the TI crates, `cargo test --features ti`, plugin `npm test` on Node 20, and the bench Python tests. It first ran on GitHub on PR #4 (`plan/lume-ti` to `main`) and passed. Lane branches still get no CI, so run its steps yourself.
- **Release pipeline** (`2a456bb`): `.github/workflows/release.yml` builds 5 targets with `--features ti`, gates on glibc <= 2.39, and publishes the plugin `.tgz`, `SHA256SUMS`, `install.sh` and `install.ps1`. `bump-version.yml` is a one-click patch/minor/major bump of `Cargo.toml`, `Cargo.lock` and the plugin `package.json`, followed by the release. The plugin version is synced to 0.12.0.

## 3. The `--features ti` build

Landed with W0 part 1 (`06dd5c5`). The root `Cargo.toml` now has:

- `[workspace]` with `resolver = "2"`, `members = ["crates/ti-contracts"]`, `default-members = ["."]`
- `[features] default = []`, `ti = ["dep:ti-contracts"]`, with `ti-contracts` as an optional path dependency of the root

```sh
cargo build --locked                       # default build, unchanged, still 4 direct deps
cargo build --locked --features ti         # lume binary with TI compiled in
cargo test  --locked -p ti-contracts       # TI crates are not default members; name them with -p (or use --workspace)
cargo test  --locked -p ti-core            # includes 10,000-case property suites (~40 s on the host)
cargo test  --locked -p ti-store           # includes the 1,000-run kill -9 crash test (~75 s)
cargo test  --locked -p ti-sql             # DataFusion SQL layer
cargo test  --locked -p ti-ingest          # decode, bucketer, sources, recorder/replay
cargo test  --locked -p ti-bench           # generator, including the window pin test
cargo test  --locked -p ti-geo             # H3 covers, in_bbox/within_nm, proptests
cargo test  --locked -p ti-sync            # manifest diff, chunked shipping, shore import, lossy two-node test
```

**Data-dependent tests (M2 gate).** These need a dataset in `.lanes/data/` and run in release:

```sh
TI_DATA_DIR=<dir> cargo test --release -p ti-ingest --test m2_gate -- --ignored --nocapture
# e.g. TI_DATA_DIR=.lanes/data/w3-smoke (smoke) or .lanes/data/correctness (full set)
# one day only:  TI_REPLAY_DAY=060 TI_DATA_DIR=<dir> cargo test --release -p ti-ingest --test m2_gate test_m2_oracle_replay_24h -- --ignored --nocapture
```

On the host in PowerShell, use `$env:TI_DATA_DIR = '<dir>'; cargo test --release -p ti-ingest --test m2_gate -- --ignored --nocapture`.
Since `2c8ba1c`, the backfill-idempotence and 24 h replay tests are `#[ignore = "needs TI_DATA_DIR"]`. **They only run with `--ignored`**, and they fail fast if the dir is missing; they no longer pass silently. The replay day is auto-detected (`day=060` on the correctness set) unless `TI_REPLAY_DAY` is set. The throughput/RSS test isn't ignored. RSS shows "n/a" on non-Linux.

**Q4 benchmark (M4 item 2).** This is `synthetic_year_benchmark` in `crates/ti-sql/tests/m4.rs`, which is also `#[ignore]`. You can tune it with `TI_Q4_WIDTH_SECONDS` (default 60), `TI_Q4_VESSELS` (50), `TI_Q4_DAYS` (365) and `TI_Q4_RUNS` (3, must be odd):

```sh
TI_Q4_WIDTH_SECONDS=10 TI_Q4_VESSELS=5 cargo test --release -p ti-sql --test m4 synthetic_year_benchmark -- --ignored --nocapture
```

**Golden corpus verify (full store).** This closed M3 (`711d2c4`) and now covers the full corpus, including geo and `intervals()` (`cb39a4d`: 58 passed, 0 failed, 4 excluded). Since `a264ed2` it is **61 passed, 0 failed, 1 excluded (`qx-003`)**, which passes M4 item 1 on the host. Build the store once, then verify the golden corpus against it:

```sh
TI_OPT_IN=last cargo run --release -p ti-ingest --example backfill_store -- .lanes/data/correctness/tier=raw .lanes/data/store-full
cargo build --release --features ti --bin lume
target/release/lume.exe ti verify --store .lanes/data/store-full --corpus tests/golden
```

- In PowerShell, set `$env:TI_OPT_IN = 'last'` first. `TI_OPT_IN=last` adds the `@last` opt-in aggregate the corpus needs. `backfill_store` also pre-registers vessels from `catalog/vessels` and path scales from `catalog/paths`, then reports raw vs index bytes. On the host: 436.5 s backfill (219,779 rows/s), 65 shards sealed in 11.6 s, index 729.3 MB vs 1,892.7 MB raw (0.39×). The `count_paths` host run (`7bf038d`): 95.9 M rows in 744.5 s (128,847 rows/s), index 731 MB (0.39× raw), and the 65 empty-list shard hashes match. The store needs about 0.7 GB of disk.
- **`count_paths`** ([design/count-paths.md](design/count-paths.md)): `[ingest] count_paths = ['<path>', ...]` in `ti.toml` selects paths that count every finite sample. Representable values still feed mean/min/max/last, and `path@skipped_magnitudes` is reported. Oracle: `tests/golden/count_paths_oracle.py`.
- **The `ti` feature is required.** Without `--features ti`, `lume` reports `Unknown subcommand: ti`.
- Verify tolerance is ±1 × 10^−scale (D34). A corpus entry with an `exclude` reason in `tests/golden/corpus.json` is skipped and counted as excluded. Geo and `intervals()` always run. Text entries are skipped only when the store has no document index.
- On the host, opening `store-full` is fast: `Store::open` 0.07 s, `session_from_store` 1.78 s, `lume ti status` 2.17 s wall. Through a container bind mount it can take about 70 s. The `ti-sql` example `open_timing` prints per-stage timings.
- **Text (`match()`, `docs`):** `backfill_store` now imports the docs automatically from `<ancestor>/docs`. To load the 1,610 golden documents into an existing store, run `cargo run --release -p ti-ingest --example import_docs -- .lanes/data/correctness/docs .lanes/data/store-full`. They are stored in `<store>/docs/documents.json`. `lume ti import-docs` (below) does the same thing.
- Regenerate the golden expected outputs (DuckDB, testing only) with `py -3 -E tests/golden/gen_expected.py --data-dir .lanes/data/correctness`. See §8 for its other flags. The oracles round per-bucket aggregates to the path scale before filtering (D35), and text oracles cover `[ts_start, ts_end)` (D36).

**`lume ti` CLI** (`31fd76a`; needs `--features ti`, like `verify`):

```sh
lume ti query "<sql>" --store <root> [--json] [--width <seconds>] [--docs-index <lume-index>]
lume ti explain "<sql>" --store <root> [--json] [--width <seconds>]
lume ti status --store <root> [--width <seconds>]
lume ti import-docs <docs_dir> --store <root> [--width <seconds>]
lume ti import-docs --parquet <glob> --entity <col> --time <col> [--time-end <col>] --title <col> --body <col> --store <root>
lume ti repl --store <root> [--width <seconds>]
```

- `--store` is required. By default the bucket width is read from the store. `--width` overrides it.
- Top-level `lume --help` lists `ti query`, `repl`, `ingest` and `status`, but only in `--features ti` builds (`b3cce8b`).
- `lume sql` (`c130bc1`) is read-only SQL over ordinary Lume indexes (MCP tool `lume_sql`); `--docs-index` attaches a Lume index to TI sessions.
- `main` runs on a 64 MB thread (`2bbcc4b`), because the debug build overflowed the 1 MB Windows main-thread stack.
- `import-docs --parquet` is the W9 mapped document import (`ebdb848`). See `tests/golden/robots/README.md` for a full example with `--time-unit`, `--id` and `--kind`.
- `repl` is interactive SQL over one opened store (`f1bb15a`).
- **`lume chat`** (`492f12b`): `lume chat --ti-store <store> [--docs-index <lume-index>] [--json]`. A model writes and runs SQL with `ti_schema`, `ti_query`, `ti_explain` and `lume_sql`: schema first, up to 3 SQL retries. Logic is in `src/chat_sql.rs`. 3 chat tests skip on Windows.
- **MCP behaviour** (`899898b`, `2d5e681`): `ti_schema` lists columns and counts and explains unmatched prefixes. Tool descriptions carry a data-model guide with the live bucket width. Errors list available tables and say to omit `width_seconds`. Unknown arguments are ignored with a note, and `store: ""` means the served store.
- The same engine backs the MCP tools `ti_query`, `ti_schema`, `ti_explain`, `ti_status` and `ti_resolve` (`src/ti_mcp.rs`). `ti_query` is read-only and capped at 500 rows and 64 KiB. pgwire has its own caps in `ti.toml` `[query] pg_max_rows` / `pg_max_bytes` (default 100,000 / 16 MiB; `0798966`). Lower them on small Pis or busy dashboards. pgwire streams batches with flushes, suspends portals when `max_rows > 0`, and treats `BEGIN`/`COMMIT` as no-ops. HTTP and MCP stay at 500 / 64 KiB.
- **Sealed-shard query cache** (`f17885d`, [design/query-cache.md](design/query-cache.md)): `[query] sealed_cache_bytes` in `ti.toml` (default 256 MiB, `0` disables) keeps decoded sealed-shard fields in a bounded LRU shared across snapshots. Warm p50 drops by 1–2 orders of magnitude (e.g. Q4 1003 → 3.1 ms). 64 MiB performs nearly the same, and the Pi runs with 64 MiB.

**`lume serve` with TI** (`bce7779`, `39c0096`; needs `--features ti`):

```sh
lume serve --ti-store <store> [--bind <IP>] [--port <PORT>] [--pg <port>]
```

- With `--ti-store`, the server binds to **loopback `127.0.0.1` by default**. Pass `--bind <IP>` to expose it. Plain `lume serve` (no TI) still binds `0.0.0.0`. `/ti` and the TI server's `/mcp` send no wildcard CORS.
- One shared engine serves both MCP and HTTP:

  | Endpoint | Does |
  |---|---|
  | `POST /ti/query` | Read-only SQL. Returns Arrow IPC (`application/vnd.apache.arrow.stream`) or JSON, chosen by the `Accept` header |
  | `GET /ti/schema` | Tables and columns |
  | `POST /ti/explain` | Query plan and pushdown |
  | `GET /ti/status` | Store status |
  | `GET /ti/resolve?q=<phrase>` | `ti_resolve`: phrase to column (100/100 top-3 on the 100-phrase live MCP eval, holdout 30/30, since `8d7cdbf`; it was 93/100 on `tests/golden/resolve.json` at `39c0096`). Handles nautical vocabulary, sailor idioms, typos and units, excludes `$source`, and never returns empty |

- **Postgres wire** (`451bfc7`, D37): `--pg <port>` adds a read-only Postgres listener on the same bind address. It is off by default and needs `--ti-store`. Since `7c4cb23` it is typed and Grafana-compatible: extended protocol, `pg_catalog`, and Grafana macros. Auth is verifier-only SCRAM-SHA-256 (D42: no plaintext password storage). TLS is offered since D46 (`45ae6ff`): see the flags below.
- **`--pg-bind <IP>`** moves only the PG listener (default: `--bind`). **`--pg-auth-config <path>`** reads only the `auth` section of a private `ti.toml`, replaces the store's users entirely (no merging), and on Unix needs mode 0600 (`1bbbac2`). Details in §11.
- **pgwire TLS** (D46 Option 2, tokio-rustls): TLS is required off loopback. `--pg-require-tls[=bool]` (or `[bind] pg_require_tls` in `ti.toml`) sets the policy, `--pg-tls-cert` and `--pg-tls-key` supply a certificate, and without them an auto self-signed cert is generated at `<store>/pg_cert.pem` (rcgen). `--pg-allow-plaintext` opts out. Plaintext trust on docker0 is narrowed to exactly `172.17.0.1`.

**`lume ti ingest`: the live service** (`75a1a4f`; needs `--features ti`):

```sh
lume ti ingest --signalk ws://<host>:3000 --store <root> [--config <path>] [--token <file|token>] [--self-urn <urn>] [--serve] [--bind <IP>] [--port <port>] [--pg <port>]
```

- It connects to the Signal K WebSocket, subscribes per spec/06 and commits under the D16 group-commit WAL. It reconnects with exponential backoff. Live timestamps use the receive time (`8d232ca`). Notification errors are not fatal; notifications become `alerts` documents (`d656dd4`).
- **Timers:** WAL group-commit fsync every 1 s. Flush and close mature buckets every 60 s or 50,000 records. Seal shards whose span ended more than 1 h ago. Retention sweep across the configured stores (`StoreSet`). Alert rules run on closed buckets (W10).
- **Status:** `<store>/ingest_status.json` reports lag, the last delta timestamp, reconnects and `running`.
- **No silent loss** (`3896e5c`): bucket windows are kept until the sink acknowledges them, retried at 1/2/4/8/16/30 s, and capped at 64 windows / 64 MiB per store. Past the cap, ingest reports `ingest_blocked` instead of dropping. Six drop counters appear in `ingest_status.json` and `/ti/status`; check them if counts look short. Window granularity is vessel-wide per bucket.
- **Token:** pass a file path or a literal with `--token`, or set `[signal_k] token` in `ti.toml`. Without `--config`, `<store>/ti.toml` is used if it exists.
- **`--serve`** runs the query server in the **same process**, because two processes must not open one live store. It binds loopback `127.0.0.1:5863` by default (`--bind`, `--port`), with the same `/ti` and `/mcp` surface as `lume serve --ti-store`. `--pg <port>` adds the read-only Postgres listener.
- **Shutdown:** SIGTERM or Ctrl-C flushes open buckets and dirty shards, syncs the WALs, sets `"running": false` and exits.
- **Pi 5 systemd unit** (when not running under the Signal K plugin):

  ```ini
  [Unit]
  Description=Lume Telemetry Ingest & Query Service
  After=network-online.target signalk.service
  Wants=network-online.target

  [Service]
  Type=simple
  User=lume
  Group=lume
  WorkingDirectory=/var/lib/lume
  ExecStart=/usr/local/bin/lume ti ingest --signalk ws://127.0.0.1:3000 --store /var/lib/lume/store --token /etc/lume/signalk-token --serve
  Restart=always
  RestartSec=5s
  CPUWeight=50
  Nice=10
  MemoryMax=1.5G
  TimeoutStopSec=30
  KillSignal=SIGTERM

  [Install]
  WantedBy=multi-user.target
  ```

**`lume ti backfill`** (needs `--features ti`):

```sh
# Signal K signalk-parquet raw tier
lume ti backfill --signalk <raw_dir> --store <root> [--self-urn <urn>] [--width <seconds>]
# Generic long or wide Parquet (W9, D38)
lume ti backfill --parquet <glob> --entity <col> --time <col> (--metric <col> --value <col> | --wide) [--time-unit s|ms|us|ns|rfc3339] [--timezone UTC|+HH:MM] [--units units.toml] [--prefix <path prefix>] --store <root>
```

- `--signalk` defaults `--self-urn` to `vessels.urn:mrn:imo:mmsi:367000000`. The width comes from `--width`, then `<store>/ti.toml`, then 10 s for a new store.
- `--parquet` maps columns explicitly. Entity ids are opaque `<kind>.urn:<id>` strings (D38). Backfill is bounded per entity by a watermark with a scratch journal (`c4e3a63`). On the host it ran at 174,769 rows/s, and re-backfilling the boat store this way gave 65/65 identical seal hashes and 58/0/4.
- The robot-fleet example (`--wide --prefix robot. --units ...`) is in `tests/golden/robots/README.md`.

**`lume ti rules`** (W10, needs `--features ti`). Rules are defined in `<store>/ti.toml`:

```sh
lume ti rules list --store <root> [--json]
lume ti rules test <name> --store <root> [--json]   # dry run: alerts the rule would write, nothing stored
```

**`lume ti sync`: M6 fleet sync over HTTP** (`ddd6398`; needs `--features ti`):

```sh
lume ti sync --to <shore url> --store <root> [--token <token>] [--token-file <path>] [--chunk-size <bytes>] [--link-budget <bytes>] [--idle]
```

- The shore is a `lume serve --ti-store` / `ingest --serve` node. It exposes `/ti/manifest` and `/ti/shards/...` (chunks, status, commit) behind a shared bearer token, compared in constant time. Vessel URNs are validated.
- Staging is capped at 64 concurrent transfers and 256 MB, with a TTL. **Sync is disabled without a token.**
- Config: `[sync]` in `ti.toml` (`token_file`, `token`, `link_budget_bytes`, `idle_priority`). Prefer `token_file`; an inline `token` is accepted only if `ti.toml` is mode 0600. A set but unreadable or empty `token_file` is an error, with no fallback to the inline token.
- Tests: `cargo test --locked -p ti-sync --test two_node_sync_http` (20 % loss, 30 min outage over loopback HTTP) and `cargo test --locked --features ti --test fleet_sync_m6`. The fleet test runs 5 vessels in debug and 50 in release (`4400327`); `TI_FLEET_VESSELS` overrides. Debug: 5 vessels 107 s, 10 vessels 243 s. Release: 50 vessels in 67.32 s (M6 item 2 passed).

**Signal K plugin `signalk-lume-ti`** (`31841c3`). Full instructions are in [plugins/signalk-lume-ti/README.md](../plugins/signalk-lume-ti/README.md). In short:

1. Install the plugin into the Signal K data dir: `cd /home/node/.signalk && npm install /path/to/plugins/signalk-lume-ti`, or copy the folder into `node_modules/`.
2. Put an `aarch64` `lume` binary (built with `--features ti`) at `node_modules/signalk-lume-ti/bin/linux-arm64/lume` (`chmod +x`), or on `PATH`, or set `lumePath` in the plugin config. The `signalk-server-docker` container is Ubuntu 24.04 with glibc 2.39, so a glibc `aarch64-unknown-linux-gnu` build linked against glibc ≤ 2.39 works; musl is not needed.
3. Enable **Lume TI** under Server → Plugin Config in the Signal K admin UI.
4. **Select Lume TI as the server's default history provider.** `signalk-to-influxdb2` also registers one, so don't assume Lume is chosen.
5. **Pin the self vessel identity.** Set a vessel UUID or MMSI in Server → Settings → Vessel Base Data. Without it, Signal K on HaLOS regenerated its self UUID on every restart, which split history in Lume and Influx. On the lead's Pi it is pinned in `data/baseDeltas.json` (`urn:mrn:signalk:uuid:0eb191d0-1f5a-42da-979e-ead792d676ee`).
6. Optional: PostgreSQL for Grafana. Use the plugin webapp's admin-only form (`enablePg`, `pgPort` 5864, `pgUser`, `pgBind`); see §11.
7. Optional: the **Ask** tab (`lume chat`) needs an Ollama endpoint and model in the plugin options `chatOllamaUrl` and `chatModel`.

The plugin supervises `lume ti ingest --signalk ws://127.0.0.1:3000 --store <dataDir>/lume-ti --serve --bind 127.0.0.1 --port 5863`, restarts it with backoff, and stops it with SIGTERM. It handles the Signal K access-request token (`<dataDir>/token.txt`) and proxies the SQL console webapp through `/plugins/signalk-lume-ti/api/*`, so nothing listens off loopback. The webapp's `apiBase` is `/plugins/signalk-lume-ti` (`530f6b1`). Plugin tests: `cd plugins/signalk-lume-ti && npm test` (17/17 at `e09bb87`).

- **History API** (`e09bb87`): the plugin is a Signal K v2.31 History API provider, answering from Lume's loopback HTTP. Since `c592a17` the plugin defaults the store to `[profiles] opt_in = ["last"]`, so `first`/`last` (SKIP/KIP `:last` requests) work on new buckets. Older buckets without `@last` fall back to `@mean`, and the response reports `method_used`. Rust side: `cargo test --features ti --test ti_http` (8/8).
- **Logging in on HaLOS:** Signal K on HaLOS uses OIDC (HaLOS SSO), and the login is bound to the host name. Open `https://halos.local:4430/admin/` → Login → **HaLOS SSO**, then open the Lume TI webapp and other apps from the same host. An IP-address origin can't complete the login. Admin access needs the HaLOS `admins` group. The webapp shows a not-logged-in banner and a Log in link on 401 (`9d90cf9`, `78b326e`, `472d6e6`).
- **Webapp results:** the webapp requests JSON from `/api/query` and `/api/schema` (`3aec284`). The TI server answers Arrow by default, so before this fix the console never showed results.
- **Native Pi 5 build:** the shipped release profile (D45, `8e7fcfe`) is fat LTO, `codegen-units = 1`, `opt-level = 3`, `panic = "unwind"`, `strip = "symbols"`. Fat LTO OOMs on the Pi, so override it to thin LTO with one codegen unit and one job. The resulting binary runs in the plugin container (glibc 2.39 OK):

  ```sh
  CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 CARGO_PROFILE_RELEASE_STRIP=symbols \
  CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 \
    cargo build --release --locked --features ti --bin lume
  ```

  Measured on the Pi: `lume` 88.3 MB, `ti-bench` 81.1 MB, peak rustc RSS 2.01 GiB, 54 minutes. (Earlier Pi builds used thin LTO with 16 codegen units and 2 jobs.)

- **Influx-vs-Lume benchmark (Pi):** `signalk-to-influxdb2` 2.3.0 writes InfluxDB bucket `marine` at 1 s (self vessel only). The harness `bench/influx_vs_lume.py` (`e9d95fc`, `9e823ea`, `0c4cdf6`) is read-only and Python stdlib only. Connection settings and sampling semantics are in [bench/influx_vs_lume.md](../bench/influx_vs_lume.md).

  ```sh
  python3 bench/influx_vs_lume.py --window 24h --runs 20          # first run cold, the rest warm
  python3 bench/influx_vs_lume.py --dry-run --from <rfc3339Z> --to <rfc3339Z>   # print query texts offline
  PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bench -p test_influx_vs_lume.py -v   # 19 tests
  ```

  - Statuses: **PASS** (exact within `--abs-tol`/`--rel-tol`), **FIDELITY** (mean deviation within `--fidelity-rel`, default 1 %), **MISMATCH**, **EMPTY**. Windows are bucket-aligned. Results also flag threshold-edge cases and path-type differences.
  - The Influx writer's 1 s resolution drops samples, so raw bucket means can differ by up to about 2 %. Expect FIDELITY, not PASS, on mean-type queries.
  - The 50-min Pi results (23:00–23:50Z, 20 runs) are in [STATUS.md](STATUS.md).
- **Absent document sources:** the notes/logbook poller treats 404, or 401 for an anonymous request, on an optional source as absent. It logs once, then hourly (`0c4cdf6`).
- **Grafana on HaLOS** shares the `influxdb` container's network namespace, so it reaches the host as `halos.local` = docker0 `172.17.0.1`, not loopback. Plugin-managed pgwire and the Grafana provisioning merged in `1bbbac2`; see §11. The admin form exists because Signal K 2.31 persists plugin config before start and has no validate hook, so a password can't go through Plugin Config.
- **Offline package (D44, `af79926`):** `bash scripts/package-plugin.sh --arm64 <lume> --x64 <lume> --output <dir>` builds one npm tarball with stripped linux-arm64 (glibc ≤ 2.39) and linux-x64 binaries and no install scripts, plus `package-report.json`. The measured tarball was 82.77 MB gzip / 235 MB unpacked (arm64 133.9 MB, x64 100.8 MB at glibc 2.35). **Build both binaries from one revision before publishing**; the measured one mixed revisions. Size-reduction options are listed in [decisions/D44](decisions/D44-plugin-package.md) but not implemented. Install steps for HaLOS and OpenPlotter are in the plugin README.
- **Access request:** on first start the plugin files a Signal K access request. It has no token until an admin approves it under Security → Access Requests. With `allow_readonly` true, live ingest works without the token, but the notes/logbook poller needs it.

**Cruiser library: `lume crawl --list` and PDF/EPUB indexing** (`0ad839c`, D43 `ac8c6d8`). Details and guards: [design/library-index.md](design/library-index.md).

```sh
lume crawl --list docs/cruiser_library.csv [--out <dir>] [--only <id,...>] [--formats pdf,epub,txt,html] [--category <name>] [--limit <n>] [--max-mb 128] [--force] [--dry-run]
lume index <dir>                                   # then attach it with --docs-index
lume serve --ti-store <store> --docs-index <lume-index>   # also on lume ti ingest --serve
```

- `docs/cruiser_library.csv` has 471 rows. `--out` defaults to `library`. `<dir>/library.json` maps files back to title, publisher and URL, and reruns skip fetched rows.
- A local Grub (`GRUB_BASE_URL`) handles HTML and retries blocked downloads when reachable; otherwise rows are fetched directly. `--max-mb` defaults to 128.
- PDF (lopdf) and EPUB extraction is pure Rust, behind the `pdf` feature, which `ti` includes. It runs in an isolated worker with 128 MiB input, 120 s and 512 MiB RSS limits.
- `--docs-index` tables on `serve` and `ingest --serve` hot-reload when the index changes.
- Host timing, 7 default PDFs (8.9 MB): fetch 8.4 s, uv extraction 6.35 s, index build 51 ms for 356 sections, search 83–121 ms including process start.
- D43 grew the release binary from 112,273,408 to 114,342,400 bytes (+1.84 %, an upper bound that includes `count_paths`).
- Plugin **Library** tab: 7 default picks, an admin-only Index button, search, and alert references (`library/alert_references.json`). Alert rules that fire on the Pi replay data, with tested SQL, are in `docs/alert-reference-searches.md`.

**M5 item 2 agent run** (`02f826a`; Python stdlib). Start a loopback `lume serve --ti-store <store>` first; the runner starts no processes.

```sh
python3 bench/agent_mcp_run.py --mcp-url http://127.0.0.1:<port>/mcp --model <model> [--llm-url http://localhost:11434/v1] [--output <run_dir>]
python3 bench/agent_mcp_expected.py --data-dir .lanes/data/correctness --output .lanes/data/agent-mcp-run/expected.json --scratch .lanes/data/agent-mcp-run/oracle-scratch   # host only, once
python3 bench/agent_mcp_grade.py <run_dir> --expected .lanes/data/agent-mcp-run/expected.json   # host only
```

- The runner offers only the MCP server's read-only `ti_*` tools (`--allow-tools`, default `ti_schema,ti_query,ti_explain,ti_status,ti_resolve`) to any OpenAI-compatible chat endpoint (Ollama by default). It warns if some allowed tools are missing and fails only if none are offered.
- Questions: `tests/golden/agent_questions.json` (20). The DuckDB answers are hidden from the runner. The grader matches row sets by value, not column name. Prose-table answers need hand grading.
- **M5 item 2 passed** (`7f048e6`): `glm-5.3` 17/20, `qwen2.5:7b` 5/20. The grader also treats constant expected columns (the named vessel) as optional, and DATE values equal midnight bucket starts (`02dc763`). The runner stood in for a nemesis8 agent; that is a recorded deviation.
- Results and all other measured numbers are in `docs/performance-comparisons.md` and [STATUS.md](STATUS.md).

**M2 item 3 on the Pi: load feed and sampler** (`eba3ce8`, `7407378`). The gate is ≥ 20,000 values/s for 1 h at ≤ 25 % of one core and ≤ 400 MB RSS. Point a separate `lume ti ingest` at a **temporary store**, never the live one, and feed it synthetic Signal K:

```sh
ti-bench sk-feed --port <port> --ramp 20000:3600,25000:300,30000:300,40000:300   # rate:seconds,...
lume ti ingest --signalk ws://127.0.0.1:<port> --store <temp store>
bash bench/pi_ingest_run.sh --store <temp store> [--duration 3600] [--interval 10] [--out <csv>]
```

- `sk-feed` also takes `--values-per-sec` (alias `--rate`, default 20,000), `--vessels` (21: 1 self + 20 AIS), `--batch-size` (100), `--seed`, `--duration`, `--max-values` and `--bind` (default `127.0.0.1`).
- `pi_ingest_run.sh` is read-only: it samples RSS, CPU, `ingest_status.json` counters, WAL/shard sizes, temperature and throttling every 10 s, then writes a CSV and a Markdown summary (`bench/summarize_ingest.py`). It auto-detects the `lume ti ingest` PID unless you pass `--pid`.
- The first 1-hour live run (`docs/bench/pi5-ingest-1h-2026-10-07.md`) was input-bound at 46.6 values/s: a stability result, not the gate.
- `sk-feed` subscriptions are additive and the feed handles socket backpressure (`35950de`).
- The first load attempt found two bugs. `7ea727d`: the retained-window admission cap (64 windows / 64 MiB) is now charged once per accumulator, so a healthy 20k values/s stream no longer reports INGEST BLOCKED. D47 (`80c7f7e`): each open-shard flush rewrote 4,704 files with 2 fsyncs each (about 140 s per flush on the SD card). A flush now writes only changed fields behind one `syncfs` on Linux, then renames and syncs each directory once; seal uses the same path. Ingest keeps its 5 s freshness flush, capped at about 10 % duty, with a hard flush past 2M records. The rebuilt Pi binary and the load rerun are in progress.

**Host oracles for W9 and W10** (Python DuckDB, testing only; run from the repo root on the host):

```powershell
$env:CARGO_INCREMENTAL = '0'
# W10 rules: runs the ignored golden_battery_rule_ranges test and checks it against DuckDB (accepted at 118/118/118)
py -3 -E tests/golden/rules_oracle.py --data-dir .lanes/data/correctness --store .lanes/data/store-full
# W9 robots: build the debug binaries first, then generate, import, compare and verify (accepted at 14/14 and verify 14/0/0)
cargo build --features ti --bin lume
cargo build -p ti-bench
py -3 -E tests/golden/robots/oracle.py --data-dir <data> --store <store> --prepare
```

- `robots/oracle.py` expects `target/debug/lume` and `target/debug/ti-bench` unless you pass `--lume-bin` / `--bench-bin`. `--prepare` generates the robot data (`ti-bench gen --profile robots`) and imports the boat, robots and incident documents. `--write-expected` refreshes the checked-in outputs from DuckDB.
- Delete the robot store and data when you finish (see the disk policy in §8).

A plain `cargo test` only tests the root crate, because `default-members = ["."]`.
**Tests bind loopback only** (`0c169c2`). Never bind `0.0.0.0` in a test: on Windows it triggers Firewall prompts.

**Strict checks, scoped to each TI crate.** Run both for every `crates/ti-*` crate you touch (currently `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`, `ti-geo` and `ti-sync`), and for the root crate with `--features ti` when you touch `src/ti_*.rs`:

```sh
cargo clippy -p <ti crate> -- -D warnings
cargo fmt -p <ti crate> --check
```

A root-wide `cargo fmt --check` is **not** required. The baseline `src/` isn't rustfmt-clean ([repo-fit §7](repo-fit.md)), so don't reformat `src/` as a side effect of TI work.
**Strict clippy must pass on the host's rustc 1.96** (Compact Echidna, formerly Regular Pheasant), not only in a container on 1.99. The two versions report different lints. For example, `8e87a11` fixed a `nonminimal_bool` lint the host reported. These checks are not in `ci.yml` yet, so run them yourself.

## 4. Joining as a new agent: the lane-clone workflow

Each agent works in its own git clone under `.lanes/<name>/` in the shared repo. `.lanes/` is gitignored.
The clone's `origin` is the shared repo itself (`C:/Users/kordl/Code/DeepBlueDynamics/lume/.`).

1. **Get a clone and a branch.** The lead assigns a lane name and a branch `ti/<lane>`, cut from `plan/lume-ti`.
   If you are told to create the clone yourself, run this from the shared repo root:
   ```sh
   git clone --branch plan/lume-ti . .lanes/<name>
   git -C .lanes/<name> switch -c ti/<lane>
   git -C .lanes/<name> config core.autocrlf true
   ```
   Who creates clones (lead or agent) is not written down *(unconfirmed)*.
2. **Check identity** (section 7) and confirm which pane you are.
3. **Read your lane file** in `plan/lanes/` or `plan/design/`, plus [repo-fit.md](repo-fit.md) and [spec/10-contracts.md](spec/10-contracts.md).
4. **Only ever write in your own clone.** Never edit the shared working tree at the repo root. You may read other lanes' clones with git commands only (`git -C .lanes/<other> log`, `status`, `show`, `diff`). Never edit, check out, reset, stash or commit in a clone that isn't yours. If another clone needs fixing, ask the lead.
5. **Don't push.** The lead fetches your branch straight from your clone.
6. **Stay current when asked.** The clone's `origin/plan/lume-ti` is a snapshot from when it was cloned. `git fetch origin` refreshes it. Ask the lead before you merge or rebase onto a newer `plan/lume-ti` *(unconfirmed convention)*.
7. **Before reporting a commit,** run the section 2 commands, plus the section 3 commands for any TI crate you touched, in your clone, and check line endings (section 6).

### Building on another lane's unmerged work: the side-branch pattern

If you need commits from another lane that the lead hasn't merged yet, fetch them **into your own clone** on a new branch. Don't put them on your lane branch, and don't work in their clone.

```sh
# in your own clone, e.g. .lanes/w3
git fetch ../corpus ti/w0-corpus             # read-only fetch from the other clone
git switch -c ti/<topic> FETCH_HEAD          # new side branch on top of their commit
# ...commit your changes here...
git switch ti/<your-lane>                    # go back to your own lane branch
```

Then tell the lead the side branch name, its base commit, and which lane it should merge with. Example: `ti/bench-days` (`d3e8f66`) in `.lanes/w3` adds `ti-bench` date flags on top of the corpus lane's `86f0ffb`, and was merged together with the corpus lane in `c65e515`.

## 5. Reporting protocol

- Report to the lead (**Industrial Pike**, pane `ee764a09`) by Hyperia mail. Agents may also mail each other directly.
- A report should give your branch, the commit hash(es) ready to merge, what changed, which tests you ran (and on which rustc), **whether fmt and strict clippy ran, or that your container lacks them**, and anything blocked or decided. The field list is a suggestion *(unconfirmed)*.
- The lead checks mail and agent panes **every 15 minutes**. Don't expect a faster reply.
- Decisions that touch contracts, dependencies or scope go to the lead. They get recorded in the decisions log ([spec/11](spec/11-risks-decisions.md)), not only in mail.

## 6. Line endings

The clones were made by Windows git, so each clone sets `core.autocrlf=true` in its own `.git/config`.
Since `a1b631d`, `.gitattributes` forces LF for `*.sh` (CRLF checkouts broke bash on the Pi). Nothing else is listed there.

- Keep `core.autocrlf=true` in your clone, including when you run git from a Linux container.
- **Never commit line-ending-only changes.** Before you commit, compare:
  ```sh
  git diff --cached --stat
  git diff --cached --stat --ignore-cr-at-eol
  ```
  If a file shows up in the first list but not the second, its only change is line endings. Unstage it with `git restore --staged <file>` and leave it alone.
- Warnings like `LF will be replaced by CRLF` are expected and harmless on their own.

## 7. Hyperia: CLI and identity check

Agents in containers (nemesis8 / Codex) use the **`hyperia` CLI** (`/usr/local/bin/hyperia`), not the Codex MCP tools.
Codex rejects unannotated MCP tools when its approval policy is `never`.

- Inside a container, Hyperia is at **`host.docker.internal:9800`**, not `localhost`.
- **Identity check, first thing:** run `hyperia whoami` and compare the pane ID with the one the lead gave you. There is a known nemesis8 bug: all containers share one `~/.codex/config.toml`, so you may be reporting as another agent's pane. If the IDs don't match, tell the lead before you send anything else.
- **Identities change when a container is restored.** The `n8-*` names and pane names are not stable. On 2026-10-06 around 06:00 UTC, every agent pane closed and its session came back in a new pane under a new identity. For example, the W4 owner went from Rigid Roadrunner `d58ca1b1` / n8-sly-viper to Long Horse `888bff45` / n8-hazy-badger. **Address agents by pane id, and re-check the current panes (Hyperia `terminal_status`) before you mail anyone.** [STATUS.md](STATUS.md) keeps the current pane list and "formerly" names.
- For subcommands (mail, pane access), run `hyperia --help`. The exact CLI syntax is not documented here *(unconfirmed)*.
- Agents running as Claude Code on the host (such as the lead) use the Hyperia MCP tools instead.

## 8. The host build pane

**Compact Echidna** (pane `6914c38e`; it replaced Regular Pheasant `364a3fc7`, which is gone) is a PowerShell 7 pane on the Windows host with the Windows Rust toolchain.

- Use it when your container has no cargo, or when you need a Windows build.
- A restored container can be missing `rustfmt` and `clippy`, as Long Horse's is. **If yours lacks them, you must say so in your report.** The lead then runs `cargo fmt -p <crate>` and rustc 1.96 strict clippy on the host at merge, and commits any fixes separately (for example `c92c325`). You may also run them yourself in a host split.
- Each agent may **split it once**. Run your builds in your split, not in the original pane.
- **A cold `--features ti` build takes about 10 minutes on the host** (DataFusion). Don't start cold builds in parallel with other agents. Check the other splits first, and reuse your clone's warm `target/` when you can.
- Build inside your own clone, e.g. `cd C:\Users\kordl\Code\DeepBlueDynamics\lume\.lanes\<name>`. Each clone has its own `target/`, so lanes don't overwrite each other's builds.
- Use PowerShell syntax there (`$env:VAR = 'x'`, not `VAR=x`).
- Some containers also have cargo locally. Either is fine; say which one you used in your report.

### Disk budget

The host's C: drive is shared by every clone, every `target/` and `.lanes/data/`. On 2026-10-06 it hit **0.1 GB free**, and the full-set M2 idempotence run died after 43 min with `StorageFull`. A single clone's `target/` reached 48.6 GB.

- **Check free space before big builds and full-set tests**, for example `Get-PSDrive C` in PowerShell or `df -h /c` in Git Bash. If space is tight, tell the lead before you start.
- **Build with `CARGO_INCREMENTAL=0`** (`$env:CARGO_INCREMENTAL = '0'` in PowerShell). Incremental caches are the largest part of `target/`.
- Since `3354bd0` the dev profile uses `debug = "line-tables-only"` and no debug info for dependencies. Full DataFusion debug info had grown the caches to about 81 GB across `target/` and the clones.
- **Disk policy (lead, current):**
  - Keep **at least 25 GB free** on C:.
  - Keep **all build caches together at 25 GB or less** (root `target/` plus every clone's `target/`).
  - Keep **each agent's `target/` at 8 GB or less**.
  - Run **`cargo clean` after each handoff**.
  - **Delete verification stores as soon as the run is done** (robot stores, temp boat stores, oracle scratch).
- **Delete retired clones' `target/` directories.** Ask the lead first, unless the clone is your own.
- **Shrink your own `target/`** when you finish a lane or switch branches, with `cargo clean` or by deleting `target/debug/incremental`.
- **Never build two full Stores at once.** Test code included: build, measure and drop one store before the next.
- **Clean test scratch dirs**, such as `.test-tmp/` and temp Stores, when a run ends, including after a failure.

### Bulk data generation: run it on the host

Writing generated data through a container's bind mount is very slow. On the host, `ti-bench` writes about 2 s per day of the correctness set. **Run bulk generation on the host**, into `.lanes/data/`, and coordinate with the lead first so two agents don't regenerate the same set.

```powershell
cargo run --release --locked -p ti-bench -- gen --root <dir> [--days N]
```

| Flag | Meaning (from `crates/ti-bench/src/main.rs`) |
|---|---|
| `--root <dir>` | Output directory, in the signalk-parquet layout. Defaults to `ti-bench-out` |
| `--days N` | Generate N days from the start (sets the end to start + N × 86,400 s) |
| `--start <unix s>`, `--end <unix s>` | Explicit window. `--end` wins over `--days` |
| `--seed <n>` | RNG seed. Defaults to the crate's `DEFAULT_SEED` |
| `--vessels <n>` | Vessel count. Defaults to the correctness set's count (the performance count with `--perf`) |
| `--perf` | Performance-set defaults (window and vessel count) |

Set `TI_BENCH_VERIFY_DETERMINISM=1` to regenerate into `<dir>.regen` and fail if any file differs. The lead's M0 check was `gen --days 1` run twice, which gave an identical sha256 over 258 files (about 20 MB/day). Without `--days`, it generates the default correctness window `START_SECS..END_SECS` in `crates/ti-bench/src/gen.rs`. That is 2026-03-01T00:00Z..2026-06-01T00:00Z (fixed in `71b7fcd`, pinned by `crates/ti-bench/tests/window.rs`). If you change the window, update `tests/golden/corpus.json` and the pin test together.

### DuckDB on the host (oracle and expected outputs only)

**DuckDB 1.5.6 is installed on the host** as a Python package (user-approved, `pip --user`). It is for oracle and expected-output work only. It is **not a product dependency** (D14: the oracle runs outside the Rust build). Run DuckDB jobs on the host. In a container, DuckDB reading through the bind mount took about 126 s per query.

```powershell
py -3 -E tests/golden/gen_expected.py --data-dir <correctness> --output-dir <dir>
# e.g. --data-dir .lanes/data/correctness --output-dir .lanes/data/expected
```

- Use **`-E`, not `-I`**. `-I` also hides user site-packages, which is where `duckdb` is installed.
- Other flags: `--corpus` (default `tests/golden/corpus.json`), `--raw-view-sql` (default `tests/golden/raw_view.sql`), `--query <id>` for a single query (e.g. `q1-001`), and `--no-materialize` to use views instead of in-memory tables.
- The default `--data-dir` is the container path `/workspace/lume/.lanes/data/correctness`, so always pass it on the host.
- `raw_view.sql` defines the DuckDB TABLE MACROs `read_raw(...)` and `read_docs(...)`. The old `CREATE FUNCTION` form never parsed in DuckDB (fixed in `c88fb75`).
- Write generated outputs to `.lanes/data/` first. Then the owning agent (or the lead) commits the JSONs into `tests/golden/expected/`, which has been committed since `312f6a0`. Keep each output small (D32): if a query returns too much, narrow its window identically in both twins.

## 9. Dependency policy

- **Every new runtime dependency needs a line in the decisions log** ([spec/11](spec/11-risks-decisions.md), D8+) before it merges. This is a PR rule from [spec/10](spec/10-contracts.md).
- **All TI dependencies are optional and gated behind the `ti` feature.** The default `lume` build keeps its 4 direct dependencies.
- **No C dependencies, except as decided.** The spec allows C bindings only for `croaring`, and only after the M4 evaluation. Decisions so far (from the dependency survey, logged as D8–D15):

  | Dependency | Decision |
  |---|---|
  | DataFusion | `=55.1.0`, `default-features = false` (its defaults pull in C compression libs) |
  | arrow | `"59.2"`, resolves to 59.3.0 via `Cargo.lock` |
  | roaring | `0.11.5` for TI. The existing `MiniRoaring` stays in place for BM25 |
  | DuckDB | Out-of-process oracle only: the CLI or the `duckdb` Python package 1.5.6 (D14, amended in `8afa646`). Never a bundled Rust crate. See §8 |
  | Release builds | `cargo-zigbuild` for musl targets |

  The log also records pure-Rust Parquet codecs only, with no zstd (D11), `blake3` with `pure` (D12), and an early aarch64-musl smoke test for pgwire SCRAM, which pulls in `ring` (D13). **D27 amends D11:** `zstd-sys` (C) is accepted because DataFusion's `arrow-ipc` forces it in. That deviates from the spec's croaring-only C rule and is awaiting spec-owner confirmation. D18 (`serde`) and D19 (`toml`) cover the dependencies of `ti-contracts`. D23 is `proptest` (a `ti-core` dev-dependency). D24 (`bincode`) and D25 (`crc32fast`) are in `ti-store`. D22 (`ti-bench`: arrow/parquet 59.2 with pure-Rust codecs, chrono) is logged (`71b7fcd`). D26 (DataFusion) and D27 (`zstd-sys`) are logged. D28 (`tungstenite`) and D29 (`parquet`) are reserved for W3. D30 is the 1 s high-resolution store ([design/hi-res-store.md](design/hi-res-store.md)). D31 (`h3o` 0.11 + `geo` 0.33.1, exact-pinned) is logged for `ti-geo`. D32 keeps golden expected outputs small (target ≤ 256 KB per entry, a few MB in total; narrow the window in both twins rather than store huge results). D33 (fixed default aggregate profile per path), D34 (verify tolerance ±1 × 10^−scale), D35 (oracles round bucket aggregates to scale) and D36 (`match()` is lexical Lume BM25: OR by default, uppercase `AND` intersects, docs cover `[ts_start, ts_end)`) are behavioral, not dependencies. D37 records optional root pgwire =0.41.0 with server-api only and disabled defaults, existing async runtime adapters, and the tokio-postgres smoke-test dev-dependency. D38 is entity identity for generic Parquet: opaque `<kind>.urn:<id>` ids with shared validation, and existing `vessels.urn:` strings unchanged. D39 is benchmark-only `croaring =2.8.0` in the isolated `bench/croaring-eval` workspace (not a Lume dependency). D40 rejects CRoaring adoption for M4. D41 lets `ti-ingest` reuse root `ureq` 2.12 for Signal K Resources/logbook polling. D42 adds TI-optional `sha2`, `hmac`, `base64`, `rand`, `chrono` and pgwire's `pg-type-chrono` for verifier-only SCRAM, with no ring/aws-lc; TLS is still not offered. D43 approves optional lopdf 0.44.0, zip 8.6.0 (deflate only) and quick-xml 0.42.0 for offline PDF/EPUB indexing, behind pdf and included by ti. See [library setup and guards](design/library-index.md). D44 bundles stripped arm64 and x64 binaries in the Signal K plugin with no install-time scripts; see [packaging decision](decisions/D44-plugin-package.md) and [installation instructions](../plugins/signalk-lume-ti/README.md). The next free number is **D45**. Ask the lead before taking it.
- The contracts crate itself depends on `arrow-array`, `arrow-schema`, `roaring`, `serde` and `toml` only, not DataFusion.

## 10. How the lead merges

The lead (Industrial Pike) is the only one who writes to `plan/lume-ti`.

1. An agent reports a ready commit by mail.
2. The lead fetches the branch straight from the clone, for example `git fetch .lanes/w1 ti/w1-core`.
3. The lead reviews it and merges it into `plan/lume-ti` in the shared tree. Merge style (merge commit, squash or fast-forward) is the lead's call *(unconfirmed)*.
4. The docs keeper refreshes [STATUS.md](STATUS.md) and this file after each merge.
5. When `plan/lume-ti` is merged to `main` is not yet decided *(unconfirmed)*. The spec wants lanes integrated on `main` daily behind the `ti` flag ([spec/10](spec/10-contracts.md)).


## 11. Plugin-managed PostgreSQL / Grafana

See [plugin setup](../plugins/signalk-lume-ti/README.md#postgresql--grafana-on-the-halos-pi)
and [Grafana provisioning](../bench/grafana/lume-ti-datasource.yaml).
Use the plugin webapp's admin-only PostgreSQL form; never enter a plaintext
password in ordinary Signal K options. Default PG port is 5864, user grafana,
disabled until a SCRAM verifier is configured.

The lead's docker inspect of the live HaLOS Pi verified host networking for
Signal K/Lume, but Grafana shares InfluxDB's bridge namespace. Grafana uses
halos.local:5864, resolving to docker0 at 172.17.0.1. Set pgBind to that specific
IP, not 0.0.0.0. HTTP remains 127.0.0.1:5863. Defaults stay loopback.

Both serve and ti ingest --serve support:
- --pg <port> enables PG.
- --pg-bind <IP> changes only the PG listener, defaulting to --bind.
- --pg-auth-config <path> reads only the auth section of a private ti.toml.
  It takes precedence over store_root/ti.toml auth, replacing it entirely:
  **no merging**, including when the override has no users. Other sections
  are ignored and do not affect ingestion/query settings. Unix refuses
  group/world-accessible files (chmod 600). Credential changes require restart.

The plugin writes its data-dir ti.toml atomically with mode 0600 and passes
--pg-auth-config, keeping store/ti.toml intact. Provisioning uses
secureJsonData.password: $__env{LUME_PG_PASSWORD}; supply that environment
secret to Grafana with the same password entered in the plugin.
sslmode disable is fine over 172.17.0.1 (loopback/docker0 trust); use require everywhere else.

Run bash tests/pg_smoke.sh host:port grafana ti on the Pi with PGPASSWORD or
PGPASSFILE configured (`postgresql-client` is installed on the lead's Pi). First Pi
run, against a throwaway loopback instance: SCRAM login OK, 16/20 pass, stopped at
case 17 (raw 24 h series) on the old 500-row/64 KiB cap. pg-limits (`0798966`) fixes that; the rerun is pending.
The Python harness test for it (`bench/grafana/test_pg_smoke.py`) skips on Windows. It executes twenty Grafana/psql SQL cases, actual
\\d telemetry, and SCRAM rejection. The loopback Rust test independently
authenticates a Node-derived verifier and decodes typed timestamp/double rows.
Pi Save & Test and the actual psql run are deployment checks, not claimed by
container mocks.

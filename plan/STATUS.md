# Lume TI — Status board

Last updated: **2026-10-09** (docs keeper, after `d58a0a5`: **deploy/halos LF in gitattributes** (`d58a0a5`), **release pipeline publishes marine-lume deb** (`79f69fa`), **postinst plugin registration** (`34c33ea`), **marine-lume .deb for HaLOS** (`f0467a8`), **Skiff → Pi Signal K runbook** (`1c90e90`); earlier at `9334f18`: **D48 gap 2 contention on Pi** (`9334f18`), **plain serve loopback default & OPERATIONS.md** (`9e95173`, A18), **Pi docs use halos.local** (`fd80db8`), **nuts.services HTTP bearer auth** (`f8d95ec`, A17, D51 ACCEPTED), **gap 4 closed on Pi** (`ffe648a`), **q6-004 pruning** (`f4422f6`, A15), **OTLP split commit queues** (`377eebe`, A16, D50), **live Ask tool calls & prompt fix** (`cd6a323`, `6ff0376`, B17); earlier at `51080e9`: **zero-tail recovery** (`51080e9`, A13b), **WAL checkpoint fix** (`38f7f12`, A14), **append-only DocStore** (`ad7a1d0`, A13), **OTLP reload & group commit** (`e8aa41f`, A10), **pgwire frame cap** (`9891f31`, A12), **HTTP bearer opt-in** (`b46482f`, A11), **hardening & D51** (`f754c0d`, `c70d617`, A9), **otlp-soak** (`2c53d3f`, A7), **PR #4 review** (`6e27791`, `e004349`, A8), **contention harness** (`3c5ab9d`, A6), **Pi gap 4 measured** (`a47ff21`), **CI dashboard SQL build** (`fa37d06`); earlier at `3633d9a`: HaLOS container .debs, Q6 bench PASS, Ollama HaLOS app, Grub image, library search, plugin TLS, M6 items 1-3; earlier: M2 item 3 on Pi, D46 pgwire TLS, release pipeline)

Integration branch `plan/lume-ti` is at `d58a0a5`. **PR #4** (`plan/lume-ti` to `main` on public GitHub DeepBlueDynamics/lume) is open; CI is green through `d58a0a5`. **`ti-contracts` is frozen** (`96ac45d`). Root tests: 46 at `8e87a11`; `cargo test --features ti` was 55 at `39c0096` (not recounted since). Plugin `npm test` 39/39 (at `a5be3f8` / `c769d23`) and `cargo test` `ti_http` 8/8 at `e09bb87`.
Workspace members: `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench`, `ti-geo`, `ti-sync`. Signal K plugin: `plugins/signalk-lume-ti/`. Next free decision: **D54** (D51 HTTP bearer auth & loopback default ACCEPTED, D52 append-only DocStore, D53 WAL checkpoints; earlier D50 OTLP, D49 D48 follow-ups). D45 (binary size) is merged and recorded in [decisions/D45-binary-size.md](decisions/D45-binary-size.md). D46 (pgwire TLS, Option 2) is merged (`45ae6ff`, Artificial Shark, `ti/pg-tls` `66e2bbb`). D47 (open-shard flush) is in [spec/11](spec/11-risks-decisions.md). All measured numbers: [docs/performance-comparisons.md](../docs/performance-comparisons.md).
Setup and workflow: [SETUP.md](SETUP.md).

## Handoff (2026-10-09, lead session)

`plan/lume-ti` is at `d58a0a5` and pushed. PR #4 is open; CI is green through `d58a0a5`.

**Done since `78c913d`:**
- **Self-telemetry:** `6e84992`, then its own store and table `telemetry_lume` (`ed02ff4`).
- **SQL fixes:**
  - Aggregates over shards without the field return no values, not "not found" (`b8bca61`).
  - DataFusion unicode and regex functions are enabled: SUBSTRING, LEFT/RIGHT and `regexp_*` (`b33532a`).
  - Query responses carry units for their own columns only (`0b01eff`).
- **CI fix:** `AuthConfig::new` tests and the live-serve test (`2282261`).
- **Release pipeline:** `release.yml` and `bump-version.yml` with installers (`2a456bb`).
- **M6 item 1 passed:** mid-shard outage resumes within shore session TTL (`3024cb9`, `5fe23f1`). 20% loss HTTP link (seed 7) with 3-chunk drop; resumes missing chunks within TTL; resent in full past TTL; byte-identical hashes.
- **M6 item 3 and D48 (approved 2026-10-08):** benchmark report against spec/13 and proposed D48 go/no-go (`83fcce7`). Warm p95 meets every measured edge class; proposed D48: GO for the single-boat pilot, NO-GO beyond it until scale/contention gates land.
- **Plugin TLS options:** D46 pgwire TLS options in the Signal K plugin (`pgRequireTls`, `pgTlsCert`, `pgTlsKey`, `pgAllowPlaintext`), Grafana sslmode docs, and W10 rules oracle 30 s hold text fix (`c769d23`).
- **Library search via server:** supervisor starts `lume ti ingest --serve` with `--docs-index`; `Library.search()` POSTs to `/ti/query` on loopback with fallback to `lume sql` (8 ms in server vs ~575 ms per query process start) (`a5be3f8`).
- **Grub published image:** HaLOS container app updated to use published `deepbluedynamics/grubcrawler:latest-lite` (v0.16.1, multi-arch) (`09f2c4f`).
- **Ollama HaLOS app:** container app for Ask tab, loopback `127.0.0.1:11434`, 4 GB cap, flash attention + q8 KV (`724fb0b`), `qwen3:1.7b` default (~1 GB) with 4.2 GB arm64 image and SD headroom notes (`d7ca3ef`, `3633d9a`).
- **Q6 bench passed:** Q6 in native query-class bench, recorded in `bench/results`, warm p95 78.4 ms PASS (`64929b9`, `5750e1d`).
- **HaLOS container .debs:** Grub and Ollama packaged as arm64 `.deb` packages via `container-packaging-tools` (`marine-grubcrawler-container_0.16.1-1_arm64.deb` 7,452 B, `marine-ollama-container_0.1.0-1_arm64.deb` 7,228 B); build script `scripts/build-halos-debs.sh` outputs to gitignored `dist/halos/` (`471f810`, `3633d9a`).
- **OTLP agent telemetry receiver & persistence (A1, A4):** loopback OTLP http/json receiver (`1947f88`, D50), `telemetry_agents` table and doc logs; monotonic counter totals persist across restarts via `otlp-counters.json` (`985008f`).
- **Atomic library index & clippy gate (A5):** atomic index writes (`bm25.json` and siblings via a same-directory temp file and rename) and whole-package clippy `-D warnings` on `lume` in CI (`36e57fc`).
- **Signal K plugin OTLP & Ask tab (B1, B9):** cloud-direct Ask tab via `ollama.com` and `glm-5.3:cloud` with `chatApiKeyFile` (`7aebf7c`); `otlpEnabled` and `otlpTokenFile` settings with supervisor `--otlp` argv forwarding and non-loopback token validation, loopback query server pinned (`eb270dd`, `1b02096`).
- **Grafana agent telemetry dashboard (B10, B14):** `bench/grafana/lume-agents-dashboard.json` on pgwire (tokens over time by model, active time, active buckets per agent, logbook docs search), test suite (`test_agents_dashboard.py`, `test_agents_dashboard_sql.py`), and CI builds `lume` with `--features ti` so live SQL tests run (`b6836a3`, `755562b`, `fa37d06`).
- **Documentation audit & changelog (B3, B5, B11, B12):** README and CHANGELOG Unreleased sections updated (`aaadb1b`, `7422cee`); stale-statement audit across SETUP, HaLOS, plugin, and performance docs (`6cf1458`).
- **Pi provisioning & bring-up automation (B4, B6, B7, B8, B13):** `scripts/provision-pi.sh` (`207919d`, `2cc4bbb`), CI deploy tests (`bbb59fc`, `a1a7790`), and `scripts/pi-bringup.sh` single-command orchestrator for provisioning, retiring Ollama, Grub image pull, and gap 4 store sync/bench (`ee64f93`, `67afe75`).
- **A6 contention harness:** `ti-query-bench contention` (`3c5ab9d`), for the gap 2 OpenCPN test.
- **A7 otlp-soak harness:** debug soak capacity rows (`2c53d3f`).
- **A8 PR #4 readiness fixes and review report:** PR #4 review fixes (`6e27791`) and readiness findings/D51 proposal report (`e004349`).
- **A9 hardening & proposed D51:** pgwire timeouts, sync TTL and chunk bounds, URL redaction, a 512 MiB query pool (`f754c0d`, `c70d617`); D51 PROPOSED.
- **A11 HTTP bearer token opt-in:** opt-in `--http-token-file` bearer on every HTTP route (`b46482f`).
- **A12 pgwire frame cap:** 1 MiB pgwire frame cap (`9891f31`).
- **A10 OTLP lazy reload and leader group commit:** `e8aa41f` (release, 10 agents: p95 33 ms; 50 agents: 386 ms, which misses the 250 ms target).
- **A13 (D52) append-only DocStore:** append-only CRC transaction log for DocStore, atomic compaction, migration with `.bak` (`ad7a1d0`).
- **A13b zero-tail recovery:** recover a zero-filled document log tail after power loss (`51080e9`).
- **A14 (D53) WAL checkpoint crash-recovery fix:** per-field WAL checkpoints and monotonic sequence fix crash recovery after flush-before-truncate (`38f7f12`); the Pi's old binary had this bug.
- **Lead fixes:**
  - linger close on early rejects so a 401/413/400 isn't lost to a TCP reset (`a806ffb`);
  - `deploy-pi.sh` rename install to avoid ETXTBSY on running plugin (`2a0bda1`);
  - `pi-bench-recipe.sh` opts store into `@last` (`72f22d7`);
  - cold/warm float check rounds floats to 12 digits (`80121fc`);
  - gap 4 results measured on the Pi 5 (`a47ff21`).
- **A15 q6-004 docs-range pruning & D48 gap 4 closed on Pi:** conservative docs-range pruning for docs-to-telemetry joins (`f4422f6`); on the Pi, Q6 p95 dropped from 308.53 ms to 13.35 ms (`ffe648a`), closing D48 gap 4 (all 8 edge query classes pass).
- **A16 OTLP split commit queues:** separate log and metric commit queues, metrics acked on a WAL fsync (`377eebe`, D50); 50 agents p95 logs 50.6 ms / metrics 30.4 ms.
- **B17 live Ask tab tool calls:** `lume chat --events` emits NDJSON tool calls, streamed to browser with keepalive over POST `/api/chat` (`cd6a323`); disconnect stops chat (`6ff0376`).
- **A17 nuts.services HTTP bearer auth (D51 ACCEPTED):** `f8d95ec`: nuts.services HTTP bearer auth for plain/TI serve and ingest serve; RS256 tokens verified offline against JWKS (`<store>/auth/jwks.json`), `ahp_` exchange form-POSTed to `/auth` with in-memory caching, mandatory `--nuts-allow` list, read/write scopes, and an unauthenticated non-loopback bind is refused.
- **A18 plain serve loopback default & docs/OPERATIONS.md:** `9e95173`: plain `lume serve` defaults to `127.0.0.1` (the user's decision on 2026-10-08); network exposure documented in `docs/OPERATIONS.md`.
- **D48 gap 2 contention test on the Pi:** measured on the Pi at `f4422f6` (`9334f18`). 10-minute back-to-back Q4 and Q8 query load against 90-day store while Signal K, InfluxDB, Grafana, OpenCPN and live ingest ran. Signal K gap p95 change was at most +2.7% against a 10% target (median +0.26%), latency was about 0%, and the drop rate didn't rise (6.65/s unloaded vs 6.44/s under load). The OpenCPN observation is pending from the user.
- **Pi naming:** docs refer to the Pi as `halos.local`, not its current IP (`fd80db8`).
- **Skiff → Pi Signal K feed runbook & token helper:** `docs/SKIFF-FEED.md` runbook and `scripts/skiff-token.ps1` helper (`1c90e90`, `353a999`, `8fc75fe`) to request readwrite device access from Signal K, store the token in a user-only file (mode 600, never printed to console), and stream Skiff boat telemetry into Signal K on `halos.local`.
- **HaLOS marine-lume .deb package:** `marine-lume` Debian package for HaLOS Pi (`f0467a8`, `d5c616c`) installs the `signalk-lume-ti` plugin and bundled `linux-arm64/lume` binary into `/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti`, enables the plugin with safe loopback defaults (`127.0.0.1:5863`, Ask off until key file configured), and restarts Signal K; `apt remove` disables plugin and keeps store, `apt purge` deletes store and config. Built via `scripts/build-halos-debs.sh --lume-bin` and tested in `scripts/test/build-halos-debs.test.sh`.
- **Plugin registration in postinst/postrm:** `marine-lume` registers `signalk-lume-ti` with Signal K in `postinst` by atomically adding `"signalk-lume-ti": "file:lume-plugin/signalk-lume-ti"` under `dependencies` in `data/package.json` and symlinking `data/node_modules/signalk-lume-ti -> ../lume-plugin/signalk-lume-ti` (1000:1000) before restarting Signal K; `postrm` remove/purge unregisters both before restart; rewrote control Description plainly (`34c33ea`, `6360e19`).
- **Release pipeline publishes marine-lume deb:** `.github/workflows/release.yml` builds and publishes `marine-lume_<version>_arm64.deb` with `SHA256SUMS` alongside the standalone binary on GitHub Releases; `docs/HALOS.md` documents installation (`79f69fa`, `36014a6`).
- **deploy/halos LF in gitattributes:** `.gitattributes` enforces `eol=lf` for `deploy/halos/**` so Windows checkouts/builds preserve LF line endings in Debian maintainer scripts and control files (`d58a0a5`).

**What runs on the Pi (2026-10-08):**
- reachable as `halos.local` (mDNS); firstrun.sh also set 192.168.68.61 static on wlan0, but scripts and docs use the name, not the IP;
- deploy key added;
- Ollama retired (free space went from 4.5 GB to 8.4 GB);
- Grub on the published image;
- running the `f4422f6` build (D52 and D53 migrated; A15 pruning);
- gap 4: all 8 classes pass at `f4422f6` (Q6 13.35 ms p95, `ffe648a`);
- gap 2 contention test passed on Signal K (gap p95 max +2.7%, latency ~0%, no drop increase at `f4422f6` / `9334f18`); user saw no OpenCPN stutter: gap 2 CLOSED;
- A fan is fitted: the Pi runs at 62–66 °C with no throttling.

**In progress:**
- gap 2 OpenCPN observation (user observation pending).

**Waiting on the user:**
- nothing blocking; gap 2 closed (no OpenCPN stutter, user 2026-10-08).

**Agents:**
- Codex (Inland Tarantula, `.lanes/w4`): A15 (q6-004) merged (`f4422f6`, closing D48 gap 4 at `ffe648a`).
- Grok 4.7 (Burning Gerbil, `.lanes/w5`): A10+A13 soak profile (`ef68b89`) and A16 OTLP commit queues (`377eebe`) merged.
- Antigravity (Panicky Parrot, `.lanes/w3`): HaLOS `marine-lume` package (`f0467a8`, `34c33ea`), release pipeline (`79f69fa`), and STATUS refresh through `d58a0a5` (`docs/status-0109`).

## Next phase (approved 2026-10-08)

The plan is [next-phase-2026-10-08.md](next-phase-2026-10-08.md). Pane names are as of the 2026-10-08 Hyperia restart; the lead is Annual Echidna.
- **Codex (Inland Tarantula, `.lanes/w4`):**
  - **Done:** A1 `ti/otlp` (`1947f88`, D50), A4 `ti/otlp-persist` (`985008f`), A6 contention harness (`3c5ab9d`: `ti-query-bench contention` for the gap 2 OpenCPN test), A8 PR #4 readiness fixes and review report (`6e27791`, `e004349`), A9 hardening (`f754c0d`, `c70d617`: pgwire timeouts, sync TTL and chunk bounds, URL redaction, a 512 MiB query pool; D51 PROPOSED), A14 WAL checkpoint crash-recovery fix (`38f7f12`, D53; the Pi's old binary had this bug), A15 q6-004 docs-range pruning (`f4422f6`: conservative docs-range pruning for docs-to-telemetry joins, q6-004 p95 66.6 -> 4.6 ms native, closing D48 gap 4 on the Pi at `ffe648a`).
  - **Now:** Standby.
- **Grok 4.7 (Burning Gerbil, `.lanes/w5`, joined 2026-10-08):**
  - **Done:** A2 `ti/pi-bench` (`f51e9d4`, D48 gap 4), A3 `ti/lint-lib` (`d8d38b1`, `e0b6b1d`), A5 `ti/index-atomic` (`36e57fc`), A7 otlp-soak harness (`2c53d3f`: debug soak capacity rows), A11 opt-in `--http-token-file` bearer (`b46482f`), A12 1 MiB pgwire frame cap (`9891f31`), A10 OTLP lazy reload and leader group commit (`e8aa41f`: release, 10 agents p95 33 ms; 50 agents 386 ms, missing 250 ms target), A13 append-only DocStore (`ad7a1d0`, D52), A13b zero-tail recovery (`51080e9`), A10+A13 OTLP soak profile (`ef68b89`), A16 OTLP split commit queues (`377eebe`: separate log and metric commit queues, metrics acked on WAL fsync; 50 agents p95 logs 50.6 ms / metrics 30.4 ms, D50).
  - **Now:** Standby.
- **Antigravity (Panicky Parrot, `.lanes/w3`):**
  - **Done:** B1 `ti/plugin-cloud`, merged as `7aebf7c`. The Ask tab defaults to `https://ollama.com` and `glm-5.3:cloud`. The `chatApiKeyFile` key is passed in the child env only.
  - **Done:** B2. Both HaLOS `.deb`s were rebuilt with the Maintainer `kord@deepbluedynamics.com` and the auto memory hooks.
  - **Done:** B3 docs, merged as `0f22550`. SETUP §13 covers the Ask tab with ollama.com (the key is written from a hidden prompt, `dd318b5`), and §14 is an OTLP placeholder.
  - **Done:** B4 `ti/pi-deploy`: `scripts/deploy-pi.sh` and `scripts/pi-retire-ollama.sh`, both with `--dry-run`.
  - **Done:** B5, SETUP §14 OTLP exporter configs for Claude Code, Codex and Gemini, corrected in `f56d0f3`.
  - **Done:** B6 `ti/deploy-test`, merged as `2365406`. It tests the deploy scripts against a throwaway local sshd container.
  - **Done:** B7 `ti/provision-pi`, merged as `207919d`. `scripts/provision-pi.sh <host> [--dry-run] [--lume-bin P] [--debs DIR] [--with-ollama]` re-provisions a new or reflashed Pi: memcg, a UUID report, the .debs without Ollama, the plugin deploy and the key-file instructions. Lead fix `2cc4bbb`: `--lume-bin` always redeploys, and the test runs without a host `dpkg-deb`.
  - **Done:** B8 `ti/ci-deploy-test`, merged as `bbb59fc` / `a1a7790`, running the deploy-script integration test and shellcheck in CI.
  - **Done:** B9 `ti/plugin-otlp`, merged as `eb270dd` / `1b02096`: Signal K plugin OTLP receiver configuration (`otlpEnabled`, `otlpTokenFile`), supervisor argv handling, validation, tests, and pinned loopback query server.
  - **Done:** B10 `ti/agents-dashboard`, merged as `b6836a3` / `755562b`: Grafana dashboard (`lume-agents-dashboard.json`) on pgwire for agent telemetry (tokens over time by model, active time, active buckets per agent, logbook docs table with text search), unittest suite (`test_agents_dashboard.py`, `test_agents_dashboard_sql.py`), and README import instructions.
  - **Done:** B11 `ti/readme-changelog`, merged as `aaadb1b` / `7422cee`: updated top-level README.md and CHANGELOG.md Unreleased section covering TI features, OTLP ingestion, Grafana dashboard, cloud Ask tab, HaLOS .debs, deployment scripts, and plugin updates.
  - **Done:** B12 `ti/docs-audit`, merged as `6cf1458`: stale-statement audit across SETUP.md, HaLOS container READMEs, plugin README, Grafana README, and performance comparisons.
  - **Done:** B13 `ti/pi-bringup`, merged as `ee64f93` / `67afe75`: `scripts/pi-bringup.sh <ssh-host>` chaining provisioning, retiring Ollama, Grub image pull, gap 4 store sync/bench, and summary.
  - **Done:** B14 `ti/ci-dashboard-sql`, merged as `fa37d06`: CI builds `lume` with `--features ti` before running Grafana unittests, and `test_agents_dashboard_sql.py` fails rather than skips when `LUME_BIN` is set but unusable.
  - **Done:** B15 `ti/status-refresh`: refresh `plan/STATUS.md` "Next phase" and Handoff sections through `fa37d06` (`631f5cd`, corrections `5f54592`).
  - **Done:** B16 `ti/status-refresh-2`: refresh `plan/STATUS.md` through `51080e9` (`0430f11`, `a3d25a2`).
  - **Done:** B17 `ti/ask-tool-calls`, merged as `cd6a323` / `6ff0376`: live Ask tool calls (`lume chat --events`, NDJSON stream with keepalive) and corrected match()/sections system prompt; disconnect stops chat (`6ff0376`).
  - **Done:** B18 `ti/halos-deb` & `ti/halos-deb-register`: packaged `marine-lume` arm64 Debian package for HaLOS Pi (`f0467a8`, `34c33ea`), registering plugin with Signal K in `postinst` and cleaning up on `postrm` remove/purge, tested 6/6; release workflow publishes `.deb` with `SHA256SUMS` (`79f69fa`); `.gitattributes` keeps LF (`d58a0a5`).
  - **Now:** Standby.
- **Pi (2026-10-08):**
  - reachable as `halos.local` (mDNS); firstrun.sh also set 192.168.68.61 static on wlan0, but scripts and docs use the name, not the IP;
  - deploy key added;
  - Ollama retired (free space went from 4.5 GB to 8.4 GB);
  - Grub on the published image;
  - running the `f4422f6` build (D52 and D53 migrated; A15 pruning);
  - gap 4: all 8 classes pass at `f4422f6` (Q6 13.35 ms p95, `ffe648a`);
  - gap 2 contention test passed on Signal K (gap p95 max +2.7%, latency ~0%, no drop increase at `f4422f6` / `9334f18`); user saw no OpenCPN stutter: gap 2 CLOSED.
- **Waiting on the user:**
  - nothing blocking; gap 2 closed (no OpenCPN stutter, user 2026-10-08).
- **Lead:**
  - gap 4 on the Pi measured (`a47ff21`) and closed (`ffe648a`);
  - gap 2 contention test on the Pi measured (`9334f18`);
  - support the user's gap 2 OpenCPN observation;
  - merge.
- **Pi state:**
  - memory cgroups are enabled (`cgroup_enable=memory`, with `cmdline.txt.bak-pre-memcg` kept as the backup);
  - the auto memory caps are Grub 1611 MiB, Ollama retired and QuestDB 768 MiB.

## Critical path right now

- **🚢 Deploying to the user's Raspberry Pi 5** (HaLOS Marine RPI, no HAT, Signal K v2.31.1 in a container, Ubuntu 24.04 with glibc 2.39).
  - KIP and Freeboard-SK are installed. Grafana, QuestDB and OpenCPN are installed. Grafana shares the `influxdb` container's network namespace and reaches the host as `halos.local` = docker0 `172.17.0.1`.
  - `signalk-to-influxdb2` 2.3.0 writes InfluxDB bucket `marine` at 1 s resolution (self vessel only).
  - **Deployed:** the thin-LTO native arm64 binary at **`6d7f5c1`** (query cache, History `:last` fallback, bucket-gap fix, pg-limits, D43; `CARGO_PROFILE_RELEASE_LTO=thin`, `CODEGEN_UNITS=16`, `BUILD_JOBS=2`; fat LTO OOMs) runs in the plugin (glibc 2.39 OK), with `[query] sealed_cache_bytes = 64 MiB`. RSS 99 MB after restart. Ingest drop counters were all zero on `7081006`. `postgresql-client` is installed on the Pi.
  - **Lume is the default History provider.** The plugin now defaults the store to `opt_in = ["last"]`, and History `:last` falls back to `@mean` with `method_used` for older buckets without `@last` (`c592a17`, deployed).
  - **Signal K login** is OIDC (HaLOS SSO), bound to the host name: use `https://halos.local:4430/admin/` → Login → HaLOS SSO, then open apps from the same host. Admin needs the HaLOS `admins` group ([SETUP §3](SETUP.md)).
  - **Vessel UUID pinned.** Signal K on HaLOS regenerated its self UUID on every restart, which split history in both Lume and Influx. The lead pinned it in `data/baseDeltas.json` (`urn:mrn:signalk:uuid:0eb191d0-1f5a-42da-979e-ead792d676ee`). Now a deployment step ([SETUP §3](SETUP.md)).
  - **pg smoke on the Pi** (throwaway loopback instance): SCRAM login OK, **16/20 pass**. It stopped at case 17 (raw 24 h series) on the 500-row / 64 KiB cap. Fixed by pg-limits (`0798966`); **rerun pending**.
  - Plugin UI fixes deployed with the webapp: login hint, Log in link, not-logged-in banner; JSON requests (`3aec284`; the console had never shown results because the TI server answers Arrow by default).
  - **Access request still pending the user's approval.** Live ingest isn't blocked (`allow_readonly` is true), but the notes/logbook poller needs the token.
  - **Pi 1-hour live run** (`7407378`, [docs/bench/pi5-ingest-1h-2026-10-07.md](../docs/bench/pi5-ingest-1h-2026-10-07.md)): 46.6 values/s (input-bound replay), RSS 119 MiB, CPU 8.4 %, 77 °C, 0 blocked, 0 failures, throttle flags `0xe0000`. **Stability only, not the M2 item 3 gate** (≥ 20,000 values/s, ≤ 25 % of one core, ≤ 400 MB RSS).
  - **Load-run findings (first M2 item 3 attempt):** `7ea727d` fixed two bugs. The retained-window admission cap (64 windows / 64 MiB) is now charged once per accumulator, so a healthy 20k values/s stream no longer reports INGEST BLOCKED. `ti-bench sk-feed` subscriptions are additive and the feed handles socket backpressure (`35950de`). Backfill after the fix: **150,510 rows/s**, shards byte-identical, corpus 61/0/1 (`bc92c4f`).
  - **D47 flush** (`80c7f7e`): the load run then stalled. Each open-shard flush rewrote 4,704 files with 2 fsyncs each, about 140 s per flush on the SD card. A flush now writes only the changed fields behind one `syncfs` on Linux, then renames and syncs each directory once. Seal uses the same path. Ingest keeps its 5 s freshness flush, capped at about 10 % duty, with a hard flush past 2M records.
  - **✅ M2 item 3 PASSED** (`78c913d`, [docs/bench/pi5-load-20k-2026-10-07.md](../docs/bench/pi5-load-20k-2026-10-07.md), [performance-comparisons §1b](../docs/performance-comparisons.md)): 20k values/s for 60 min at **19,916 values/s mean**, **14.2 % / 24.2 %** CPU of one core, **65.6 MB** peak RSS. Ramp: 25k gives 25,659; 30k gives 30,317; 40k gives 38,508 at 25.1 % CPU and 75.3 MB. 100.4 M values in total; 0 rejected, blocked or failed.
  - **Bugs fixed on the way:** `50d8b15` closes staged flush files (fixes EMFILE). `6f94543` holds only a failing sink to 64 retained windows; a healthy 21-vessel fleet keeps about 126 open.
  - **Next:** rerun the pg smoke and Grafana Save & Test; re-verify Q6 (per-path counts) for the bucket gap; the user approves the access request.
  - **The Pi runs hot without an Active Cooler** (82–86 °C under load).
- **📊 Pi benchmark, 50-min window** (`bench/influx_vs_lume.py`, 23:00–23:50Z, 20 runs, warm p50, InfluxDB vs Lume). Replaces the earlier 17-min preliminary table:

  | Query | InfluxDB | Lume | Result |
  |---|---|---|---|
  | Raw depth | 42.9 ms | 43.7 ms | |
  | Hourly max | 21.4 ms | 6.1 ms | PASS |
  | Minute mean SOG | 15.2 ms | 9.3 ms | FIDELITY 0.44 % |
  | Multi-condition | 53.2 ms | 9.5 ms | 40/40 minutes; minute minimums differ (Influx drops samples) |
  | Min depth + position | 156.8 ms | 5.6 ms | Same depth; Influx's first occurrence is 10 min later (drops) |
  | Per-path counts | 4,080 ms | 57 ms | One-bucket gap on 3 constant battery paths (290 Influx vs 289 Lume). **Fixed** in `3896e5c`; Pi re-verification pending |

  Caveat: the HaLOS Influx writer's 1 s resolution drops samples, so raw bucket means and minimums differ from Lume's.
- **✅ Signal K History API provider** (`e09bb87`, merge of `64cbc0d`). `plugins/signalk-lume-ti` registers as a Signal K v2.31 History API provider backed by Lume's loopback HTTP. Plugin tests 17/17, `ti_http` 8/8. `first`/`last` need the `@last` aggregate retained. **It must be selected as the server's default history provider**, because `signalk-to-influxdb2` also registers one.
- **✅ M6 item 1 PASSED** (`3024cb9`, lead): 20% chunk loss over HTTP (seed 7) plus a mid-shard link outage. The outage is time-compressed at the real ratio: a 30-minute outage against the 60-minute shore session TTL becomes 60 ms against 120 ms. Within the TTL the client resumes and sends only the missing chunks. Past it, the shard is resent in full. Both end with byte-identical hashes. The earlier test only toggled the link between shards.
- **✅ M6 item 2 PASSED** on the host (`4400327`): 50 vessels synced and verified in **67.32 s** (1.35 s/vessel, release). `fleet_sync_m6` now defaults to 5 vessels in debug and 50 in release. HTTP sync merged in `ddd6398`. The lossy-HTTP results still go to the lead.
- **✅ Bucket-gap fix** (`3896e5c` + `65b84a3`). Root cause of the Pi's silent one-bucket loss: `let _ = advance_watermark` swallowed apply errors. Windows are now retained until the sink acknowledges, retried at 1/2/4/8/16/30 s, capped at 64 windows / 64 MiB per store, after which ingest reports `ingest_blocked`. Six drop counters appear in `ingest_status.json` and `/ti/status`. Window granularity is vessel-wide per bucket. **Pi Q6 re-verification pending deployment.**
- **✅ M4 item 1 PASSED: corpus 61/0/1** on the host (`a264ed2` + `981fe15` + `7d1bc5a`). `q2-001`'s TI SQL is now scoped to the primary vessel like its unchanged oracle, plus a two-vessel notes isolation test. The `count_paths` stores were rebuilt, the oracle rerun exited 0, and 65 hashes match. Only `qx-003` stays excluded.
- **✅ M5 item 2 PASSED** (`02dc763`, `7f048e6`, `d1f6a4b`; harness `02f826a`). With only Lume's read-only MCP `ti_*` tools: **`glm-5.3` 17/20** (15/20 before the MCP fixes) and `qwen2.5:7b` 5/20 (0/20 before). The grader matches rows by value and treats constant columns (the named vessel) as optional. **Deviation:** the harness runtime (`bench/agent_mcp_run.py`) stood in for a nemesis8 agent. `d1f6a4b` fixed a semantic merge break (`chat_sql` vs the new `ti_mcp::definitions(width)` signature). Details in `docs/performance-comparisons.md`.
- **✅ M5 item 1 PASSED** (`8d7cdbf` + `b9d3b1a`). `ti_resolve` top-3 **100/100** on the 100-phrase live MCP eval (holdout 30/30), up from a 65/100 baseline. The lead's independent blind 20 phrases: 19/20 top-3, 18/20 top-1. Adds nautical vocabulary, typo and unit handling, sailor idioms, depth weighting, `$source` exclusion and a never-empty fallback.
- **✅ Sealed-shard query cache** (`f17885d` + `ca04235`, [design/query-cache.md](design/query-cache.md)). `[query] sealed_cache_bytes` (default 256 MiB, 0 disables). Native 20-query warm p50, off → on: Q1 99 → 0.5, Q2 210 → 1.5, Q3 553 → 1.3, Q4 1003 → 3.1, Q5 4275 → 296, Q7 159 → 3.3, Q8 555 → 13.5 ms. 64 MiB is nearly identical. Corpus 61/0/1, all fingerprints equal.
- **✅ `IS [NOT] DISTINCT FROM` pushdown** (`1668313` profile, `cebf5ea`). Exact bitmap pushdown (two-valued under `NOT`). Q5-002: 300 → 8.45 ms native release, 0 rows materialized (was 1.5 M). **All query classes now meet their p95 targets.** Corpus 61/0/1.
- **✅ MCP ergonomics** (`899898b`, `2d5e681`). `ti_schema` lists columns and counts and explains unmatched prefixes. Tool descriptions carry a data-model guide with the live bucket width. Errors teach (available tables, omit `width_seconds`). Unknown arguments are ignored with a note. `store: ""` means the served store.
- **✅ `lume chat` + plugin Ask tab** (`492f12b`, Artificial Shark). `lume chat --ti-store <store> [--docs-index <index>] [--json]` writes and runs SQL with `ti_schema`/`ti_query`/`ti_explain`/`lume_sql`, schema-first, with 3 SQL retries. Logic in `src/chat_sql.rs`. Plugin options `chatOllamaUrl`/`chatModel`. 3 chat tests skip on Windows.
- **✅ `count_paths` merged** (`7bf038d`, Long Horse). Host oracle: 65 empty-list shard hashes MATCH; backfill 95.9 M rows in 744.5 s (128,847 rows/s); index 731 MB (0.39× raw). Corpus was 60/1/1 at this merge; `q1-007` and `q6-006` pass; `q2-001` fixed in `a264ed2`.
- **✅ pg-limits** (`0798966` + `1db0cce`). `[query] pg_max_rows` / `pg_max_bytes` (default 100,000 / 16 MiB); HTTP and MCP stay 500 rows / 64 KiB. True batch streaming with flushes, portal suspension for `max_rows > 0`, `BEGIN`/`COMMIT` as no-ops.
- **✅ Cruiser library** (`0ad839c`, `48db92e`, `c1bc8f0`, D43 `ac8c6d8` + `4bad710`). `lume crawl --list <csv>` fetches a reading list (Grub when reachable, otherwise direct; `library.json` manifest; `--max-mb 128`). `docs/cruiser_library.csv` has 471 rows. Plugin **Library** tab: 7 default picks, an admin Index button, search, alert references (`library/alert_references.json`). D43: pure-Rust PDF (lopdf) and EPUB extraction behind the `pdf` feature (included in `ti`), in an isolated worker with 128 MiB / 120 s / 512 MiB RSS limits. `--docs-index` on `serve` and `ingest --serve`, hot-reloaded. Release binary 112,273,408 → 114,342,400 bytes (+1.84 %, an upper bound that includes `count_paths`). Host timing, 7 default PDFs (8.9 MB): fetch 8.4 s, uv extraction 6.35 s, index build 51 ms for 356 sections, search 83–121 ms including process start.
- **✅ D44 plugin package** (`af79926`). One npm tarball with stripped linux-arm64 (133.9 MB, glibc 2.39) and linux-x64 (100.8 MB, glibc 2.35) binaries, no install scripts. 82.77 MB gzip / 235 MB unpacked. **The measured tarball mixes revisions; rebuild both binaries from one revision before publishing.** Size-reduction options are listed in [decisions/D44](decisions/D44-plugin-package.md) but not implemented.
- **✅ Grafana-compatible pgwire + SCRAM** (`7c4cb23`, post-merge fixes `2bbcc4b`). Typed results, extended protocol, `pg_catalog`, Grafana macros, and verifier-only SCRAM-SHA-256 (**D42**). TLS arrived with D46 (`45ae6ff`). **Plugin-managed** since `1bbbac2` (see Merged surfaces).
- **✅ Strict TI CI job** (`ec48673`, fmt fix `329d8a1`). Rust 1.96 pinned; fmt on 8 crates plus the root TI files; clippy `-D warnings` on the TI crates; `cargo test --features ti`; plugin npm on Node 20; bench Python. **First GitHub run is green** (PR #4, all 4 checks).
- **✅ W10 alerts ACCEPTED** (`a8f32bd` + `6d51df5`). Signal K notifications become `alerts` documents (`d656dd4`). Bitmap alert rules run on closed buckets, with history, hold, caps and live document refresh. Host DuckDB oracle: **118/118/118** ranges, and `match(alerts, 'battery')` covers 357 buckets.
- **✅ W9 generic Parquet ACCEPTED** (`167f391` + `dd2db2f`). Mapped long/wide Parquet and document import, common backfill drivers, D38 entity identity, and a robot-fleet golden set. Robot DuckDB oracle: **14/14** non-empty and matching. `lume ti verify` on robots: **14/0/0**. Boat regression on the new backfill path: **65/65 seal hashes identical**, corpus **58/0/4**. Backfill ran at 174,769 rows/s.
- **✅ Sealed-data repair fixed** (`b23514a`). An append to a sealed shard used to lose data. Now the shard is restored and resealed as a new version, and identical content keeps the same hash. The host suite passes, including the 1,000-run kill -9 test.
- **Merged surfaces:**
  - pgwire (`--pg`, D37, `451bfc7`; extended protocol and SCRAM in `7c4cb23`, D42). The 20 scripted fleet questions pass as a regression (20/20). The M5 item 2 agent run uses a new question set (see above).
  - `lume sql`: read-only SQL over ordinary Lume indexes, the `lume_sql` MCP tool, and `--docs-index` in TI sessions (`c130bc1`).
  - Signal K notes and optional logbook polled into owned documents (D41), chart pins in the plugin (`db2e777`).
  - Pi 5 fixes (`042d681`): live serve reopens read-only on flush/seal (5 s debounce), so it sees new data; applied-timestamp ingest status; plugin UUID access request.
  - `bench/influx_vs_lume.py` (`e9d95fc`, `9e823ea`, `0c4cdf6`): stdlib-only paired Influx-vs-Lume harness, 19 unit tests, statuses PASS/FIDELITY/MISMATCH/EMPTY.
  - Plugin-managed SCRAM pgwire (`1bbbac2` + `495d836`): plugin options `enablePg`, `pgPort` (5864), `pgUser`, `pgBind`; an admin-only webapp form stores only the verifier. `--pg-bind` and `--pg-auth-config` (auth section only, replaces the store's users, mode 0600 required on Unix). Grafana provisioning `bench/grafana/lume-ti-datasource.yaml` (`halos.local:5864`, password from `$__env{LUME_PG_PASSWORD}`). 20-query `tests/pg_smoke.sh`; its Python harness test skips on Windows.
  - The notes/logbook poller treats 404 or anonymous 401 on optional sources as absent; it logs once, then hourly (`0c4cdf6`).
  - Top-level `lume --help` lists `ti query|repl|ingest|status` (`b3cce8b`; only in `--features ti` builds).
  - `lume ti ingest` live service (`75a1a4f`).
  - `lume ti backfill --parquet|--signalk`.
  - `lume ti rules list|test`.
  - `lume ti repl` (`f1bb15a`).
  - The `signalk-lume-ti` plugin (`31841c3`).
- **In flight:**
  - Better Platypus (Codex): `ti/cache-warm` in `.lanes/w4`.
  - Compact Echidna (Antigravity): `ti/docs-refresh` in `.lanes/w3`.
  - Lead: PR #4 review and merge, Pi pg smoke and Grafana rerun.
  - **Hyperia:** mail notices and `pane_send` to n8 (Docker) panes are fixed in Hyperia PR #311, not built yet. Until then a human types "run msg_check".
  - `.gitattributes` (`a1b631d`) now keeps `*.sh` LF; CRLF checkouts had broken bash on the Pi.
- **🟡 User decisions:** item 16, the CC-BY-SA licence on `src/ti_resolve/signalk_paths.json`, is still needed before publishing.
- **Disk policy (lead):**
  - Keep at least **25 GB free** on the host.
  - Keep all build caches together at **25 GB or less**, and each agent's `target/` at **8 GB or less**.
  - Run `cargo clean` after each handoff.
  - Delete verification stores immediately.
  - See [SETUP §8](SETUP.md).

## Pane changes (about 06:00 UTC)

| Role | Now | Formerly |
|---|---|---|
| W4 cache warm | **Better Platypus** (Codex) in `.lanes/w4` on `ti/cache-warm` | Long Horse `888bff45` / Rigid Roadrunner `d58ca1b1` |
| W3 ingest, W8 sync, HaLOS debs, plugin | **Compact Echidna** (Antigravity) in `.lanes/w3` | Artificial Shark `fd91f4b1` / Romantic Pike `90fc608c` |
| Corpus and generator | Lead took it over and merged it (`c65e515`) | Zygomorphic Prawn `eccaf836`, n8-noble-toad: **retired** (permanently offline, per the user) |
| Host build pane | **Compact Echidna** `6914c38e` | Regular Pheasant `364a3fc7` (gone) |

Commit messages, the decisions log and older docs use the former names. Re-check pane ids with Hyperia `terminal_status` before mailing.

## Recent merges into `plan/lume-ti`

| Commit | What |
|---|---|
| `d58a0a5` | gitattributes: keep `deploy/halos` LF so Windows-built .debs have working maintainer scripts |
| `79f69fa` | **release pipeline publishes marine-lume deb** (`36014a6`): release workflow builds and attaches `marine-lume` arm64 `.deb` with `SHA256SUMS`; `docs/HALOS.md` install instructions |
| `34c33ea` | **plugin registration in postinst/postrm** (`6360e19`): `marine-lume` registers plugin with Signal K (`package.json` dep + `node_modules` link) and unregisters on remove/purge; control description reworded |
| `f0467a8` | **marine-lume .deb for HaLOS** (`d5c616c`): plugin + arm64 binary into Signal K data dir, safe loopback defaults, remove keeps history, purge deletes it |
| `1c90e90` | **Skiff → Pi Signal K feed** (`353a999`, `8fc75fe`): `docs/SKIFF-FEED.md` runbook and `scripts/skiff-token.ps1` token helper (readwrite access request, user-only token file, never printed) |
| `9334f18` | bench: D48 gap 2 contention on the Pi at `f4422f6`; Signal K gap p95 +2.7% max, latency ~0%, no drop increase; OpenCPN observation pending |
| `9e95173` | **A18 plain serve loopback default** (`e34c7ed`): plain `lume serve` defaults to `127.0.0.1` (the user's decision on 2026-10-08); `docs/OPERATIONS.md` network exposure (D51) |
| `fd80db8` | docs: refer to the Pi as `halos.local`, not its current IP |
| `f8d95ec` | **A17 nuts.services HTTP auth** (`ab90a71`): nuts.services bearer auth for HTTP (D51 ACCEPTED); RS256 offline JWKS verify, `ahp_` exchange, mandatory allowlist, read/write scopes; off-loopback unauthenticated bind refused |
| `ffe648a` | bench: D48 gap 4 closed on the Pi; every edge-target class passes at `f4422f6` (Q6 308.53 -> 13.35 ms p95, A15) |
| `f4422f6` | **A15 q6-004 docs-range pruning** (`a27e458`, Codex / Inland Tarantula): conservative docs-range pruning for docs-to-telemetry joins, registered only for docs joins; q6-004 p95 66.6 -> 4.6 ms native |
| `377eebe` | **A16 OTLP split commit queues** (`119c296`, Grok 4.7 / Burning Gerbil): separate log and metric commit queues, metrics acked on a WAL fsync; 50 agents p95 logs 50.6 ms / metrics 30.4 ms (D50) |
| `6ff0376` | fix(plugin): stop lume chat when the Ask client disconnects mid-stream (B17) |
| `cd6a323` | **B17 live Ask tool calls** (`b89d05b`, Compact Echidna / Antigravity): live Ask tool calls (`lume chat --events`, NDJSON stream with keepalive) and corrected match()/sections system prompt |
| `2515300` | ci: Discord notifications for PRs, issues and releases via the bot API (ported from Hyperia) |
| `1c1d0e5` | docs(readme): Ollama setup (local models, ollama.com cloud with OLLAMA_API_KEY, failover URLs, Pi Ask tab key script) |
| `c30f902` | feat(deploy): `scripts/pi-set-ollama-key.sh` installs the Ask tab key on a Pi in one command |
| `12fe5f5` | fix(ci): dashboard SQL test uses `lume serve --ti-store --otlp`; standalone `ti otlp` is ingestion-only |
| `ef68b89` | **A10+A13 OTLP soak profile** (`eecb22e`, Grok 4.7 / Burning Gerbil): A10+A13 release soak rows and 50-agent flush-phase profile (log-aware docs bytes) |
| `0430f11` | **B16 status refresh** (`a3d25a2`, Compact Echidna / Antigravity): STATUS through `51080e9`, Pi bring-up and A6-A14 |
| `51080e9` | **A13b zero-tail recovery** (`af2a448`, Grok 4.7 / Burning Gerbil): recover a zero-filled document log tail after power loss, keep non-zero garbage fatal (D52) |
| `38f7f12` | **A14 WAL checkpoint crash-recovery fix** (`fa70570`, Codex / Inland Tarantula): per-field WAL checkpoints and monotonic sequence fix crash recovery after flush-before-truncate (D53; fixes bug present in Pi's old binary) |
| `2a0bda1` | fix(deploy): install the lume binary by rename so a running plugin doesn't fail the deploy (ETXTBSY) |
| `a47ff21` | bench: D48 gap 4 measured on the Pi 5, 7 of 8 classes pass edge p95; Q6 misses on q6-004 (308.53 ms against 200 ms) |
| `ad7a1d0` | **A13 append-only DocStore** (`de9fcbb`, Grok 4.7 / Burning Gerbil): append-only CRC transaction log for DocStore, atomic compaction, migration with `.bak` (D52) |
| `a806ffb` | fix(http): lingering close on early rejects so a 401/413/400 isn't lost to a TCP reset |
| `e8aa41f` | **A10 OTLP lazy reload & leader group commit** (`5c8b2aa`, Grok 4.7 / Burning Gerbil): lazy engine reload and leader group commit for OTLP; 10 agents p95 33 ms, 50 agents 386 ms (missing 250 ms target) (D50) |
| `9891f31` | **A12 pgwire frame cap** (`c7596ed`, Grok 4.7 / Burning Gerbil): 1 MiB pgwire frontend frame cap before buffering, fatal 08P01 on decode errors, zero default-build warnings |
| `b46482f` | **A11 HTTP bearer token opt-in** (`535a8fa`, Grok 4.7 / Burning Gerbil): opt-in `--http-token-file` bearer on every HTTP route, route-specific sync/OTLP tokens, defaults unchanged (D51 part) |
| `80121fc` | fix(bench): cold/warm self-check rounds floats to 12 digits; reported fingerprint stays exact |
| `72f22d7` | fix(bench): pi-bench-recipe opts the store into `@last`, as the CI bench gate does (A2) |
| `c70d617`, `f754c0d` | **A9 hardening & proposed D51** (`920ba8f`, `a476453`, Codex / Inland Tarantula): pgwire idle/write deadlines, sync session TTL and chunk bounds, URL redaction, 512 MiB query memory pool; D51 PROPOSED; Q1-Q8 correctness fingerprints |
| `2c53d3f` | **A7 otlp-soak harness** (`10fa09c`, Grok 4.7 / Burning Gerbil): ti-bench otlp-soak harness and debug soak capacity rows |
| `e004349`, `6e27791` | **A8 PR #4 readiness fixes & review report** (`d4aad0d`, `aab48f5`, Codex / Inland Tarantula): PR #4 readiness fixes and report with D51 proposal |
| `3c5ab9d` | **A6 contention harness** (`7fab796`, Codex / Inland Tarantula): `ti-query-bench contention` for the D48 gap 2 OpenCPN test, live vessel/window overrides |
| `5f54592`, `631f5cd` | status: STATUS refresh through B14 (B15) and lead corrections (real branch commits, Ollama retirement status, bm25.json, dashboard metrics, CI state) |
| `fa37d06` | **CI dashboard SQL build** (`d008957`, Compact Echidna / Antigravity): CI builds `lume` with `--features ti` before running Grafana unittests so `test_agents_dashboard_sql.py` runs live; fails when `LUME_BIN` is invalid (B14) |
| `67afe75`, `ee64f93` | **Pi bringup script** (`958036b`, `447547a`, Compact Echidna / Antigravity): `scripts/pi-bringup.sh` single-command orchestrator (provision Pi, retire Ollama, pull Grub image, gap 4 store sync and bench, system summary); clears partial remote store before `scp -r` (B13) |
| `6cf1458` | **Docs audit** (`b75de73`, Compact Echidna / Antigravity): stale-statement audit across SETUP, HaLOS container READMEs, plugin README, Grafana README, and performance comparisons (B12) |
| `7422cee`, `aaadb1b` | **README & CHANGELOG** (`5c3129a`, Compact Echidna / Antigravity): update README.md and CHANGELOG.md Unreleased section for TI, OTLP ingestion, Grafana dashboard, cloud Ask tab, HaLOS .debs, deployment scripts; fix `otlp-counters.json` and self-telemetry switch (B11) |
| `36e57fc` | **Atomic index & clippy gate** (`197f14a`, Grok 4.7 / Burning Gerbil): atomic library index writes (`bm25.json` and siblings go through a unique same-directory temp file, then a rename), and whole-package clippy `-D warnings` enforcement on `lume` in CI (A5) |
| `755562b`, `b6836a3` | **Agents dashboard** (`f5c4135`, `1203408`, Compact Echidna / Antigravity): Grafana dashboard `lume-agents-dashboard.json` on pgwire for agent telemetry (tokens over time, active time, active buckets per agent, logbook docs search), unittest suite (`test_agents_dashboard.py`, `test_agents_dashboard_sql.py`), and README instructions (B10) |
| `985008f` | **OTLP counter persistence** (`199b4ae`, Codex / Inland Tarantula): monotonic counter totals survive receiver restarts via `stores/agents/otlp-counters.json` (A4, D50) |
| `1b02096`, `c852b6d` | **Plugin OTLP settings** (`eb270dd`, Compact Echidna / Antigravity): Signal K plugin `otlpEnabled` / `otlpTokenFile` settings, supervisor `--otlp` argv forwarding and non-loopback validation; query server pinned to loopback (B9) |
| `e0b6b1d`, `d8d38b1` | **Lint lib** (`a312e75`, Grok 4.7 / Burning Gerbil): clear strict clippy across lume lib, bin and tests (A3) |
| `a1a7790`, `bbb59fc` | **CI deploy test** (`5513fe9`, Compact Echidna / Antigravity): CI deploy-script integration test and shellcheck run unconditionally (B8) |
| `1947f88` | **OTLP agent telemetry receiver** (`856524a`, Codex / Inland Tarantula): OTLP http/json receiver on loopback for agent telemetry (Hyperia and n8), `telemetry_agents` table and doc logs (A1, D50) |
| `2cc4bbb`, `207919d` | **Provision Pi** (`cf57077`, Compact Echidna / Antigravity): `scripts/provision-pi.sh` for new or reflashed HaLOS Pis (memcg, UUID report, container .debs, plugin deploy) (B7) |
| `2365406` | **Deploy test** (`4dfcfd8`, Compact Echidna / Antigravity): deploy-script integration test against a throwaway local sshd container (B6) |
| `f51e9d4` | **Pi query bench** (`c9735d4`, Grok 4.7 / Burning Gerbil): arm64 `ti-query-bench` variant, `scripts/pi-bench-recipe.sh store`, and `ti-query-bench bench --pi` (A2, D48 gap 4) |
| `f56d0f3`, `e4a5b82` | **OTLP docs** (`3d6a2f4`, Compact Echidna / Antigravity): SETUP §14 OTLP receiver and exporter configuration for Claude Code, Codex, and Gemini (B5) |
| `6bc62d6` | **Pi deploy scripts** (`3ef3824`, Compact Echidna / Antigravity): `scripts/deploy-pi.sh` and `scripts/pi-retire-ollama.sh` (B4) |
| `dd318b5`, `0f22550` | **Docs cloud Ask tab** (`c7d42cf`, Compact Echidna / Antigravity): SETUP §13 Ask tab with ollama.com, hidden prompt key entry, optional Ollama app (B3) |
| `7aebf7c` | **Plugin cloud Ask tab & debs** (`ea48cb3`, Compact Echidna / Antigravity): Ask tab cloud-direct with ollama.com and glm-5.3:cloud, rebuilt HaLOS debs with auto memory (B1, B2) |
| `3633d9a` | deploy: Ollama config.yml and .deb install notes default to qwen3:1.7b like metadata.yaml |
| `471f810` | **HaLOS container .debs** (`138f974`, Compact Echidna / Antigravity): Grub and Ollama as arm64 HaLOS container .debs via `container-packaging-tools`; package layout matches `marine-questdb-container`; build script `scripts/build-halos-debs.sh` outputs to gitignored `dist/halos/` |
| `d7ca3ef` | deploy: Ollama app defaults to qwen3:1.7b; README records the 4.2 GB arm64 image and SD headroom |
| `5750e1d` | **Q6 bench PASS** (`64929b9`, Better Platypus / Codex): Q6 in the native query-class bench; results in `bench/results`; Q6 warm p95 78.4 ms PASS |
| `724fb0b` | **Ollama HaLOS app**: local LLM container app for Ask tab, loopback 127.0.0.1:11434, 4 GB cap, flash attention + q8 KV, qwen3:4b default (1.7b in d7ca3ef), documented 3-way failover |
| `09f2c4f` | **Grub published image**: uses published `deepbluedynamics/grubcrawler:latest-lite` (v0.16.1, multi-arch) |
| `c769d23` | **Plugin TLS options** (`4f8ec3d`, Compact Echidna / Antigravity): D46 pgwire TLS options in Signal K plugin (`pgRequireTls`, `pgTlsCert`, `pgTlsKey`, `pgAllowPlaintext`), Grafana sslmode docs; tests/golden/README.md 30 s hold text fix |
| `a5be3f8` | **Library search via server**: supervisor passes `--docs-index` to `lume ti ingest --serve`, `Library.search()` POSTs to `/ti/query` loopback, fallback to `lume sql` (8 ms vs 575 ms per query) |
| `83fcce7` | **M6 item 3 benchmark report & proposed D48**: benchmark report against spec/13; proposed D48 go/no-go (single-boat pilot GO, multi-boat NO-GO until scale/contention gates land) |
| `5fe23f1`, `3024cb9` | **M6 item 1 passed** (lead): mid-shard outage resumes within shore session TTL (20% loss HTTP seed 7, 30-min outage compressed to 60 ms vs 120 ms resumes missing chunks, past TTL resent in full; byte-identical hashes) |
| `78c913d` | **M2 item 3 passed** on the Pi: 20k values/s for 60 min at 19,916 mean, 14.2 % / 24.2 % CPU, 65.6 MB peak RSS; ramp to 38,508 values/s at 25.1 % CPU; 100.4 M values, 0 rejected/blocked/failed |
| `45ae6ff` | **D46 pgwire TLS** (`66e2bbb`, `72d39e6`, Artificial Shark): Option 2, tokio-rustls with permissive licences, rcgen auto self-signed `<store>/pg_cert.pem`, TLS required off loopback, docker0 trust narrowed to exactly `172.17.0.1`. Flags `--pg-require-tls[=bool]`, `--pg-tls-cert`, `--pg-tls-key`, `--pg-allow-plaintext`, `[bind] pg_require_tls`. `pg_tls`, `pg_limits`, `ti_http` pass on the host |
| `2a456bb` | **Release pipeline**: `release.yml` (5 targets, `--features ti`, glibc <= 2.39 gate, plugin .tgz, SHA256SUMS, `install.sh`, `install.ps1`) and one-click `bump-version.yml`; plugin version synced to 0.12.0 |
| `6f94543`, `50d8b15` | Load-run bugs: only a failing sink is held to 64 retained windows (healthy 21-vessel fleet keeps about 126 open); staged flush files closed (EMFILE) |
| `80c7f7e` | **D47** open-shard flush: changed fields only, one `syncfs` on Linux, renames and directory syncs once; seal uses the same path; 5 s freshness flush capped at about 10 % duty, hard flush past 2M records |
| `bc92c4f`, `7ea727d`, `35950de` | Pi load-run fixes: retained-window admission cap charged once per accumulator (no false INGEST BLOCKED at 20k values/s); `sk-feed` additive subscriptions and socket backpressure; backfill 150,510 rows/s, byte-identical shards, corpus 61/0/1 |
| `8e7fcfe` | **D45 release profile**: fat LTO, 1 CGU, opt-level 3, unwind, `strip = symbols` (D45 itself `39bfe72`, merged `b84e6aa`) |
| `eba3ce8`, `92b211e`, `2df24b5` | **`ti-bench sk-feed`** (`205231a`, Artificial Shark): synthetic Signal K WebSocket stream with rate ramp, vessels and batching for the Pi 20k values/s gate; UTF-8 summary output on Windows |
| `cebf5ea` | **`IS [NOT] DISTINCT FROM` pushdown** (`ad627f0`): exact bitmap pushdown; Q5-002 300 → 8.45 ms, no rows materialized. All classes meet p95 |
| `6d7f5c1` | `docs/bench/pi5-ingest-1h-2026-10-07.md`: Pi 1-hour live ingest (46.6 values/s input-bound, 119 MiB RSS, 8.4 % CPU, 0 blocked/failures) |
| `b9d3b1a` | **`ti_resolve` idioms** (`d0b4cce`): sailor idioms, depth weighting, `$source` exclusion, never-empty fallback; blind 20: 19/20 top-3, 18/20 top-1 |
| `1668313` | Q5 profile (`446d578`): `IS DISTINCT FROM` was a residual filter materializing 1.5 M rows (14× slower than bitmap runs) |
| `8d7cdbf` | **`ti_resolve` eval** (`e5a7c8b`): nautical vocabulary, typo and unit handling; 100-phrase live MCP eval top-3 100/100 (holdout 30/30), from 65/100. **M5 item 1** |
| `ca04235` | Native cache timings; class-only benchmark reports |
| `a1b631d` | `.gitattributes`: `*.sh` always LF |
| `7407378` | **Pi ingest sampler** (`6aeb276`): read-only `bench/pi_ingest_run.sh` (RSS, CPU, status counters, WAL/shard sizes, temperature, throttling) and `bench/summarize_ingest.py` |
| `f17885d` | **Sealed-shard query cache** (`4c790fe`, Long Horse): bounded LRU of decoded fields shared across snapshots, `[query] sealed_cache_bytes` (256 MiB default) |
| `c592a17` | **History `:last`** (`3458c2e`): plugin defaults the store to `opt_in = [last]`; `:last` falls back to `@mean` with `method_used` |
| `d1f6a4b` | Plan build fix: `chat_sql` uses the shared `ti_mcp::definitions(width)` |
| `02dc763`, `7f048e6` | Grader: constant expected columns optional, DATE equals midnight bucket start. **M5 item 2 results**: `glm-5.3` 17/20, `qwen2.5:7b` 5/20 |
| `492f12b` | **`lume chat`** (`fe6e251`, Artificial Shark): `ti_schema`/`ti_query`/`ti_explain`/`lume_sql`, schema-first, 3 SQL retries, `--json`; plugin Ask tab |
| `899898b` | **MCP ergonomics** (`9264447`): schema with columns and counts, data-model guide in tool descriptions with live bucket width, teaching errors, unknown arguments ignored with a note |
| `9d90cf9`, `78b326e`, `472d6e6`, `48696a6`, `3aec284` | Plugin UI: login hint to the Signal K host name and HaLOS SSO, Log in link, not-logged-in banner; wind preset uses `speedApparent`, trimmed column-dump errors; webapp requests JSON (console had never shown results) |
| `29090d7` | `docs/performance-comparisons.md`: all measured numbers (Pi Influx vs Lume, ingest, library indexing, fleet sync, binary size) |
| `02f826a`, `6080b14`, `d8dbfc2`, `5c02e19`, `2d5e681` | **M5 item 2 harness** (`4356748`): `bench/agent_mcp_run.py` (tool-only, `--allow-tools`), `tests/golden/agent_questions.json` (20 questions, hidden DuckDB oracles), host-only `bench/agent_mcp_grade.py` (row values, not column names); MCP `store: ""` = served store |
| `a264ed2` + `981fe15`, `7d1bc5a` | **`q2-001`** (`4b94802`, Long Horse): scoped to the primary vessel like its oracle; two-vessel notes isolation test. **Host corpus 61/0/1**, 65 hashes match |
| `af79926` | **D44 plugin package** (`56119cd`): offline npm tarball with validated stripped arm64/x64 binaries, no install scripts, HaLOS/OpenPlotter install steps, `scripts/package-plugin.sh` |
| `0798966` + `1db0cce` | **pg-limits** (`e7976c7`, Artificial Shark): `[query] pg_max_rows`/`pg_max_bytes` (100k / 16 MiB), batch streaming, portal suspension, transaction no-ops. Fixes Pi smoke case 17 |
| `3896e5c` + `65b84a3` | **Bucket-gap fix** (`9f21aa6`): retain windows until sink ack, bounded retry, blocked admission, six drop counters in status. Root cause: `let _ = advance_watermark` swallowed apply errors |
| `7bf038d` | **`count_paths`** (`9bc2c73`, Long Horse): per-bucket list snapshot, retained value aggregates, skipped-magnitude reporting, verify dispatch fix. Corpus 60/1/1 |
| `ac8c6d8` + `4bad710` | **D43** library extraction (`4f02460`): lopdf, zip, quick-xml behind `pdf`; hot-reloaded `--docs-index` tables on `serve` and `ingest --serve` |
| `4400327`, `925fa8c` | `fleet_sync_m6`: 5 vessels in debug, 50 in release (`TI_FLEET_VESSELS` overrides); TOML literal string for the Windows `token_file` path. **M6 item 2 passed** (50 vessels, 67.32 s release) |
| `0ad839c`, `48db92e`, `c1bc8f0` | `lume crawl --list` reading-list fetch (`docs/cruiser_library.csv`, 471 rows); plugin Library tab; `docs/alert-reference-searches.md` |
| `ddd6398` | **M6** (`c99b66a`, Artificial Shark): `ti-sync` HTTP transport, shore endpoints `/ti/manifest` and `/ti/shards/...` with bearer auth, `lume ti sync --to`, `[sync]` in `ti.toml`, lossy-HTTP two-node test, fleet equality test (`TI_FLEET_VESSELS`). Host fmt, clippy and sync tests green |
| `ec48673` + `329d8a1` | Strict TI CI job (`8c603dc`), legacy jobs unchanged; `329d8a1` rustfmts `src/ti_parquet.rs` for the new root-format check |
| `1bbbac2` + `495d836` | **Plugin-managed SCRAM pgwire** (`7aa42b5`, Long Horse): `--pg-bind`, `--pg-auth-config`, admin-only verifier form, HaLOS Grafana provisioning, 20-query `tests/pg_smoke.sh`, `tests/golden/grafana-smoke.json`. `495d836`: rustfmt root TI files; pg_smoke harness test skips on Windows |
| `ebd96a4` | rustfmt `ti-ingest` after the pi-polish merge |
| `0c4cdf6` | `ti/pi-polish` (`3788614`): benchmark threshold-edge and path-type classification; the notes/logbook poller treats 404 or anonymous 401 on optional sources as absent, logs once then hourly |
| `9e823ea` | `ti/bench-influx` (`21546f1`): bucket-aligned windows, lat/lon position, FIDELITY status, path-set diff |
| `e9d95fc` | **`bench/influx_vs_lume.py`** (`bd465ad`, Long Horse): stdlib-only paired InfluxDB-vs-Lume Signal K query benchmark (19 unit tests by `0c4cdf6`) |
| `b3cce8b` | Top-level `lume --help` lists the TI subcommands (`ti query`, `repl`, `ingest`, `status`); `--features ti` builds only |
| `e09bb87` | **Signal K v2.31 History API provider** (`64cbc0d`, Long Horse) in `plugins/signalk-lume-ti`, backed by Lume loopback HTTP. npm 17/17, `ti_http` 8/8. Needs `@last` retained for `first`/`last`; must be the server's default history provider |
| `7c4cb23` + `2bbcc4b` | **Typed Grafana-compatible pgwire** (`2cc5597`): extended protocol, `pg_catalog`, Grafana macros, verifier-only SCRAM (**D42**). `2bbcc4b`: rustfmt of the pgwire files; `main` runs on a 64 MB thread (the debug build overflowed the 1 MB Windows main stack); webapp shows a Signal K login hint on 401 |
| `530f6b1` | Webapp `apiBase` is `/plugins/signalk-lume-ti`, presets fixed, display name "Lume TI" |
| `0c169c2` | Tests bind loopback only (no `0.0.0.0`, so no Windows Firewall prompts); Windows-only race in the Signal K resources mock fixed |
| `db2e777` | Signal K notes and optional logbook into documents (**D41**), plugin chart pins and owned-resource cleanup, portable plugin tests, live-server document freshness test |
| `042d681` | `ti/pi5-fixes` (`c79a91d`): live serve read-only reopen on flush/seal (5 s debounce), empty-store width, applied-timestamp ingest status, plugin UUID access request |
| `f38ecb4` + `b173296` | Backfill profiling (no regression) and the **CRoaring evaluation**: D39 bench-only `bench/croaring-eval`, **D40 reject for M4** ([design/croaring-eval.md](design/croaring-eval.md)) |
| `c130bc1` + `ff20ec4` | **`lume sql`** (`f711cfc`): read-only SQL over Lume indexes, `lume_sql` MCP tool, `--docs-index` in TI sessions |
| `167f391` + `dd2db2f` | **W9 final** (`ebdb848`, Long Horse): mapped document import (`lume ti import-docs --parquet`), common backfill drivers, the robot-fleet generator (`ti-bench gen --profile robots`) and a 14-query golden set (`tests/golden/robots/`) with a host DuckDB oracle. **Accepted** (see Critical path). `dd2db2f`: host fmt and a run-time `CARGO_TARGET_TMPDIR` fix |
| `c4e3a63` | **D38** entity identity (opaque `<kind>.urn:<id>`) and bounded per-entity watermark backfill with a scratch journal (`70d047d`). Host D38 regression: 65/65 seal hashes identical, corpus 58/0/4 |
| `31841c3` + `08836bb` | **`signalk-lume-ti` plugin** (`ec3d43b`, Artificial Shark): supervision, Signal K access-request auth, loopback proxy, SQL webapp. Also `ti-bench --hz` with per-path override and the W8 bench harness (`2159fa6`) |
| `dfce248` + `b6c6ee0` | **W9 first slice** (`857d777`): mapped long/wide Parquet reader, units/scales, `lume ti backfill --parquet`. Host fmt and strict clippy |
| `b23514a` + `6297cf2` | **Sealed-data repair** (`67b44ca`): appends to a sealed shard restore it and reseal as a new version; identical content keeps the hash. Host suite incl. the 1,000-run kill -9 test passes |
| `75a1a4f` + `cdd9628` | **`lume ti ingest` live service** (`4971810`, `e6b604a`): reconnect, flush/seal/retention timers, SIGTERM WAL flush, `ingest_status.json`, `--serve` on loopback by default. `cdd9628` gates the shutdown-signal handler to `cfg(unix)` |
| `a8f32bd` + `6d51df5` | **W10 rules** (`1760742`): bitmap alert rules with history, hold, caps and live document refresh. `6d51df5` fixes the rules oracle (raw path encoding; 30 s hold, because the 5 min hold gave zero runs and passed vacuously). **W10 accepted** (118/118/118) |
| `8d232ca` + `0344835` | Live `receive_time` fix (live timestamps were clamped to EPOCH) and non-fatal notification errors (`3c192b0`) |
| `6bd99a4`, `d656dd4`, `a1c65e9` | W10 step 1: `ClosedBucketObserver` hook (`1afb7f3`), and Signal K notifications kept as `alerts` documents in live ingest and backfill (`cb61ff7`). Host fmt/clippy |
| `13c58c0` + `12cad6a` | **ti-sync** (`0f2f642`, Artificial Shark): manifest diff, resumable chunked shipping, shore import, lossy two-node test (20 % loss, 30 min outage); D30 follow-ups. Host fixes |
| `451bfc7` | **pgwire** (`7118ef8`, Long Horse): read-only simple-query Postgres listener (`--pg`, **D37**) and the 20 scripted fleet questions (regression 20/20) |
| `41858ed`, `e72298b` | W10 lane file (notifications as alerts; rules that write their own alerts) and W9 lane file (generic time-series Parquet) |
| `f1bb15a`, `1d362d0` | `lume ti repl` and `backfill_store` `TI_WIDTH`. README: documents and time series in one store |
| `ef9148a` | **D30 end to end** (`4a90c96`, Artificial Shark): multi-store backfill and stream, the `telemetry_hr` table, retention drops only sealed shards |
| `d3ce0f0` | Docs refresh after W7 HTTP and `ti_resolve` |

Earlier: `39c0096` `ti_resolve` 93/100 + `--bind`, `0b3a17a` fmt, `400557c` README TI section, `bce7779` **HTTP `/ti`**, `4626591` `open_timing`, `b95f890` docs, `26e535a` host fmt, `20117c4` **M2 idempotence + D30 fan-out**, `31fd76a` **W7 CLI/MCP**, `cb39a4d` **full corpus 58/0/4**, `3354bd0` line-tables-only, `b0ba6b0` docs, `cd9bb3e`/`11d961a`/`ad05f94`/`c1d4941` **W5 text**, `6dc19c5` docs, `711d2c4` **closes M3**, `f6c47b4`, `b07ec05`, `f5806c0` D34, `bfd7373` D33, `8f776da`/`82a8aa5`/`829c771` backfill speed, `5632aad`, `b4a1879` **Q4 72.4×**, `be6ffdb`, `312f6a0` **closes M0**, `8931877` **W6 geo**, `cf98c61` W2 (**M1 complete**), `f7faf5f` W1, `96ac45d` contracts freeze, `06dd5c5` workspace. Full list: `git log --oneline --first-parent plan/lume-ti`.

## Agents

| Agent (pane name) | Pane | Lane/scope | Branch | Clone | Last known state |
|---|---|---|---|---|---|
| Industrial Pike | `ee764a09` | Lead and integrator. Host runs and oracles (DuckDB, `lume ti verify`, rules and robots oracles, D38 regression), host fmt/clippy at merge, disk policy. **Pi 5 deployment**, the Influx-vs-Lume benchmark and the M2 item 3 load run | `plan/lume-ti` | shared tree | `2df24b5` |
| Long Horse (formerly Rigid Roadrunner) | `888bff45` | **M6 items 1 and 3** (lossy-sync gate, benchmark report, D48 go/no-go draft). Delivered **D45 binary size** (`39bfe72`, merged `b84e6aa`), the query cache, History `:last` fallback, `IS DISTINCT FROM` pushdown, `ti_resolve` idioms, `q2-001`, the M5 agent harness, MCP ergonomics, `count_paths`, D43, plugin pgwire, the Influx-vs-Lume bench, pgwire + SCRAM, the History API provider, `lume sql`, the CRoaring evaluation, W9 and W10 | `ti/m6-report` | `.lanes/w4` | Assigned, not started |
| Artificial Shark (formerly Romantic Pike) | `fd91f4b1` | Next: **`ti/plugin-tls`** (plugin options for D46, `tests/golden/README.md` 30 s hold fix). Delivered D46 pgwire TLS (`66e2bbb`), `ti-bench sk-feed`, the Pi ingest sampler, `lume chat` and the Ask tab, pg-limits, M6 HTTP sync, ti-sync, the live ingest service, the plugin, `--hz` and the bench harness | `ti/plugin-tls` | `.lanes/w3` | Its plugin edits once leaked into the shared tree (rescued as a patch) |
| Zygomorphic Prawn | `eccaf836` | — | — | `.lanes/corpus` (retired) | **Retired.** Its `target/` has been deleted |
| Compact Echidna (formerly Regular Pheasant) | `6914c38e` | Host build pane (PowerShell 7, rustc 1.96.1). Not an agent | — | — | Bulk data and DuckDB jobs run here. **Check free disk before big builds** |

How to refresh the last column: `git -C .lanes/<x> log --oneline -5` and `git -C .lanes/<x> status --short`.

## Milestones

Week numbers count from kickoff. A milestone closes only when every gate test passes in CI ([spec/12](spec/12-milestones.md)).

| Milestone | Lanes | Weeks | Gate status |
|---|---|---|---|
| M0 Contracts | W0 | 1 | **✅ Complete** (`312f6a0`) |
| M1 Core and store | W1, W2 | 2–4 | **Complete** (`cf98c61`). Sealed-data repair hardened in `b23514a` |
| M2 Ingest | W3 | 2–5 | **✅ Passed on the Pi** (`78c913d`). Items 1 and 2 passed; item 3 passed: 20k values/s for 60 min at 14.2 % CPU and 65.6 MB peak RSS, ramp to 38.5k |
| M3 SQL and pushdown | W4 | 2–6 | **✅ Closed 2026-10-06** (`711d2c4`) |
| M4 Text, geo, intervals | W4, W5, W6 | 6–8 | **All three items met on the host** (CI not run on this branch). Item 1: corpus 61/0/1 (`a264ed2`). Item 2: 72.4×. Item 3: CRoaring rejected (D40, `f38ecb4`) |
| M5 Agent surface | W7 | 7–9 | **In progress. Items 1 and 2 passed** (`ti_resolve` 100/100; agent `glm-5.3` 17/20 with the harness standing in for nemesis8). Item 3: plugin, History API provider (the Pi's default) and plugin-managed SCRAM pgwire deployed on the Pi (token pending). Pi pg smoke 16/20 before pg-limits, rerun pending. Store-based installs, Pi 4 and OpenPlotter not done |
| M6 Fleet and benchmarks | W8, integrator | 9–12 | **In progress.** Item 1 **passed** (`3024cb9`, mid-shard outage resume). Item 2 **passed** (50 vessels, 67.32 s release). Item 3: [benchmark report](bench/benchmark-report.md) written; D48 gap 4 closed (`ffe648a`), gap 2 contention measured on Pi at `f4422f6` (`9334f18`: Signal K gap p95 max +2.7%, latency ~0%, no drop increase; OpenCPN observation pending from user); Influx-vs-Lume Pi benchmark run (50 min) |

Also landed outside the original milestones: W9 generic Parquet (accepted), W10 alerts (accepted), the cruiser library (`lume crawl --list`, D43), the D44 plugin package, `lume chat`, the query cache and `IS DISTINCT FROM` pushdown. Remaining: M5 item 3, M6 item 3, and the Pi 5 deployment.

### M2 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Replaying a recorded 24 h delta log yields `BucketRecord`s equal to oracle bucketing | **Passing on real data**: 460,112/460,112 (`8931877`) |
| 2 | Parquet backfill of the correctness set is idempotent | **✅ Passed** (`11c0edc`): 65 sealed shards identical on rerun. Still holds on the W9 backfill path (65/65, `167f391`) |
| 3 | Pi 5 sustains 20,000 values/s for 1 h within the CPU and RSS budget (≤ 25 % of one core, ≤ 400 MB RSS) | **✅ Passed** (`78c913d`, [report](../docs/bench/pi5-load-20k-2026-10-07.md)): 19,916 values/s mean for 60 min, CPU 14.2 % mean / 24.2 % peak of one core, 65.6 MB peak RSS. Ramp: 25k gives 25,659; 30k gives 30,317; 40k gives 38,508 at 25.1 % CPU and 75.3 MB. 100.4 M values, 0 rejected/blocked/failed. Fixes on the way: `7ea727d`, D47 (`80c7f7e`), `50d8b15` (EMFILE), `6f94543`. Host x86: 181,783 values/s. Earlier 1-hour live run (`6d7f5c1`): 46.6 values/s input-bound, stability only. The Pi reached 82–86 °C under load without an Active Cooler |

### M4 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Full golden corpus green, including `match()`, `in_bbox`, `within_nm` and `intervals()` | **✅ Passed on the host: 61 passed, 0 failed, 1 excluded** (`a264ed2`). `q1-007` and `q6-006` pass since `count_paths` (`7bf038d`); `q2-001` since its SQL was scoped to the primary vessel (oracle unchanged). `qx-003` stays excluded (DataFusion 55 limit, covered by `qx-013`) |
| 2 | `BitmapAggregateExec` ≥ 10× faster than the materializing path on Q4 at shore scale | **Met in release**: 72.4× (`b4a1879`) |
| 3 | `croaring` frozen-view evaluation written up in the decisions log, adopt or reject | **✅ Met: rejected** (D40, `f38ecb4`; [design/croaring-eval.md](design/croaring-eval.md)). A Portable-view prototype is a follow-up needing its own C-dependency approval |

### M5 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | MCP tools live in `lume serve`, and `ti_resolve` returns the right column in the top 3 for ≥ 90 % of a 100-phrase test set | **✅ Passed** (`8d7cdbf` + `b9d3b1a`): 100/100 top-3 on the live MCP eval (holdout 30/30), from a 65/100 baseline; the lead's blind 20: 19/20 top-3, 18/20 top-1. (Was 93/100 fixture accuracy at `39c0096`) |
| 2 | A nemesis8 agent with only Lume MCP answers 20 scripted fleet questions; integrator grades against oracle results | **✅ Passed, with a deviation** (`7f048e6`): `glm-5.3` 17/20 (15/20 before the MCP fixes), `qwen2.5:7b` 5/20 (0/20 before), on `tests/golden/agent_questions.json` with hidden DuckDB answers, graded by `bench/agent_mcp_grade.py` (rows by value, constant columns optional). **Deviation:** the `bench/agent_mcp_run.py` runtime, offering only the read-only `ti_*` tools, stood in for a nemesis8 agent |
| 3 | Signal K plugin installs on a HALPI2 from the HaLOS Marine container store, and on OpenPlotter from the Signal K App Store (Pi 4 4 GB and Pi 5), obtains a token and supervises ingest. psql and Grafana pass a 20-query smoke set | **Open.** Plugin merged (`31841c3`) with access-request auth and supervision, plus the History API provider (`e09bb87`). pgwire merged (`451bfc7`), Grafana-compatible with verifier-only SCRAM since `7c4cb23` (D42; TLS since D46, `45ae6ff`), plugin-managed with Grafana provisioning since `1bbbac2`. **Installed on the Pi 5** by hand (binary at `1bbbac2`); the token is still pending the user's approval. **psql smoke on the Pi: 16/20**, stopped at case 17 on the 500-row/64 KiB cap (`ti/pg-limits`). Grafana Save & Test, store-based installs, Pi 4 and OpenPlotter are not done |

### M6 gate detail

| # | Gate item | State |
|---|---|---|
| 1 | Two-node sync converges with 20 % chunk loss and a 30-minute link outage | **In progress.** Passes in process (`13c58c0`). The HTTP version (`two_node_sync_http.rs`, bearer auth) merged in `ddd6398`; the lossy-HTTP results go to the lead |
| 2 | Shore node answers fleet queries across 50 synthetic vessels | **✅ Passed on the host** (`4400327`): 50 vessels synced and verified in 67.32 s release (1.35 s/vessel). Debug default is 5 vessels (107 s); `TI_FLEET_VESSELS` overrides. Gate wants CI; CI hasn't run on this branch |
| 3 | Benchmark report against every target; go/no-go in the decisions log | **In progress.** [Benchmark report](bench/benchmark-report.md) written. D48 gap 4 closed on Pi (`ffe648a`). D48 gap 2 contention measured on the Pi at `f4422f6` (`9334f18`): Signal K gap p95 was at most +2.7% against a 10% target, latency was about 0%, drop rate didn't rise; OpenCPN observation pending from user. Influx-vs-Lume 50-min Pi run exists |

M0, M1 and M3 gate details are unchanged since they closed; see `git show d3ce0f0:plan/STATUS.md`.

## Open decisions waiting on the user

From [spec/11-risks-decisions.md](spec/11-risks-decisions.md) (open questions):

1. ~~Default bucket width: 10 s, or 1 s with 1-min rollups?~~ **Resolved by D30**: keep 10 s, and add a 1 s `telemetry_hr` store for the navigation, wind and depth allow-list.
2. Per-source values as first-class columns in v1, or only `$source` sets?
3. AIS contacts as a second `contacts` table, or out of scope?
4. Shore storage: local NVMe only, or sealed shards in object storage with a read cache?
5. Should `ti_resolve` also index Signal K spec descriptions for paths a vessel has never reported?
6. Does anything besides sealed shards move to shore? (p. 7 says only sealed shards. p. 20 ships open-shard WAL tails every 5 min)
7. PV-1 Done criterion: HALPI install from the Signal K App Store (p. 2), or from the HaLOS Marine container store (p. 19)?
8. Licensing check: borrow ideas only from FeatureBase (Apache-2.0), copy no code, keep Lume BSD-3-clean.

From [spec/02-pilot-vessel.md](spec/02-pilot-vessel.md) (owner assumptions to confirm):

9. Boat computer: exact HALPI model, CM5 RAM size, OS image (HaLOS Marine or desktop + OpenCPN), and whether OpenCPN runs on the same HALPI. *(The test device now is a Raspberry Pi 5 on HaLOS Marine RPI with OpenCPN installed.)*
10. Final NMEA 2000 equipment list: MFD brand, instrument vendor, engine gateway.
11. Whether Victron stays on 2.0, and whether a Cerbo GX or Venus OS device is aboard.
12. Consent to record 90 days of data for the benchmark dataset, and what may be shared publicly.
13. Satellite link (Starlink or Viasat) and the data budget `ti-sync` may use.

Spec deviations and proposals needing the user:

14. **D27: accept `zstd-sys` (C)**, forced in by DataFusion 55.1.0's `arrow-ipc`. Amends D11 and deviates from spec/10 (C bindings only for `croaring`). Awaiting spec-owner confirmation.
15. **Pinned `rust-toolchain.toml`**, proposed because the host (rustc 1.96.1) and the containers (1.99) report different clippy lints.
16. **Licence of `src/ti_resolve/signalk_paths.json`** (150 KB, `39c0096`). Extracted from SignalK/specification 1.8.4 @ `fb628fb4` under **CC-BY-SA 2.0**, with its own `LICENSE` and `README.md` in `src/ti_resolve/`. Lume is BSD-3. Decide before publishing.
17. **Pi 5 cooling:** the user's Pi 5 reaches 82–86 °C under load without an Active Cooler. Fit one before the M2 item 3 one-hour run (suggestion; unconfirmed whether planned).

## Open items (team, not user)

- [ ] **Pi 5 deployment** (lead): binary at `6d7f5c1` (cache 64 MiB), plugin JS, vessel UUID pin, Lume as default History provider with `:last` fallback are done. Still to do: the user approves the plugin's access request (needed by the notes/logbook poller).
- [ ] **PR #4** (`plan/lume-ti` to `main`): open, CI green; awaiting merge.
- [ ] **Pi pg smoke rerun** (16/20 before pg-limits) and Grafana Save & Test for M5 item 3, after the D43 build is deployed.
- [ ] **Pi Q6 re-verification** of the bucket-gap fix (`3896e5c`) after deployment.
- [x] **M2 item 3 load run** passed (`78c913d`): 20k values/s for 60 min, 14.2 % CPU, 65.6 MB; ramp to 38.5k.
- [x] Query cache (`f17885d`), History `:last` fallback (`c592a17`), `IS DISTINCT FROM` pushdown (`cebf5ea`; all classes meet p95), Pi 1-hour stability run (`6d7f5c1`).
- [x] `q2-001` (`a264ed2`, corpus 61/0/1, M4 item 1). `lume chat` + Ask tab (`492f12b`). MCP ergonomics (`899898b`). Plugin UI login and JSON fixes.
- [ ] **D44 package:** rebuild arm64 and x64 from one revision before publishing; size-reduction options not implemented.
- [x] pg-limits (`0798966`). Bucket-gap fix (`3896e5c`). `count_paths` (`7bf038d`). M6 item 2 (50 vessels, 67.32 s). Cruiser library and D43.
- [x] Influx-vs-Lume 50-min Pi run (20 runs; see Critical path).
- [x] Plugin-managed SCRAM pgwire + Grafana provisioning (`1bbbac2`).
- [x] **M5 item 2** (`7f048e6`): `glm-5.3` 17/20; deviation: harness runtime instead of nemesis8. **M5 item 1** (`8d7cdbf`): `ti_resolve` 100/100.
- [x] **D46 pgwire TLS** (Artificial Shark, `45ae6ff`).
- [x] **D45 binary size** (Long Horse): merged (`b84e6aa`). Shipped release profile (`8e7fcfe`): fat LTO, 1 codegen unit, opt-level 3, `panic = "unwind"`, `strip = "symbols"`. The Pi native build overrides to thin LTO with 1 codegen unit: lume 88.3 MB, ti-bench 81.1 MB, peak rustc RSS 2.01 GiB, 54 minutes.
- [ ] **M6** (Long Horse, `ti/m6-report`, not started): lossy-sync gate (item 1), benchmark report and D48 go/no-go draft (item 3). Item 2 passed.
- [x] `lume sql` and `--docs-index` (`c130bc1`).
- [x] `croaring` evaluation (M4 item 3): rejected, D40.
- [x] **`count_paths` host acceptance** (`7bf038d`): 65 empty-list shard hashes MATCH; stores rebuilt and oracle rerun exited 0 after `a264ed2`. See [the contract](design/count-paths.md) and `tests/golden/count_paths_oracle.py`.
- [ ] **`qx-003`**: keep the join-form `qx-013`, or revisit on a DataFusion upgrade.
- [x] pgwire TLS: **D46, Option 2** delivered by Artificial Shark and merged (`45ae6ff`); plugin options follow on `ti/plugin-tls`. SCRAM landed (D42, `7c4cb23`); D13's aarch64 smoke: SCRAM login works on the Pi (`tests/pg_smoke.sh`, 16/20 before pg-limits).
- [ ] **User licence decision** on `signalk_paths.json` (user item 16).
- [ ] Meridian VHF transcripts. Not done. (Notes and logbook polling landed in `db2e777`, D41.)
- [x] Pi 5 throughput/RSS for M2 item 3 (`78c913d`). Sampler `bench/pi_ingest_run.sh` merged (`7407378`).
- [ ] `tests/golden/README.md` still describes the W10 rule as `for 5m`, but the oracle and test now use a 30 s hold (`6d51df5`). Owner: Artificial Shark, in `ti/plugin-tls` (outside docs-keeper scope).
- [x] W10 alerts accepted (118/118/118; `match(alerts,'battery')` 357 buckets).
- [x] W9 accepted (robots 14/14 DuckDB, verify 14/0/0; boat 65/65 hashes, 58/0/4; 174,769 rows/s).
- [x] Sealed-data repair (`b23514a`); D38 regression 65/65, 58/0/4 (`c4e3a63`).
- [x] Live ingest service, plugin, pgwire, ti-sync, D30 end to end.
- [x] Grafana-compatible pgwire + SCRAM (D42), History API provider (`e09bb87`), loopback-only tests (`0c169c2`).
- [ ] Verify [signalk-formats §1.3](design/signalk-formats.md) against a **real** signalk-parquet `.parquet` file with DuckDB `DESCRIBE`.
- [x] Strict TI CI job (`ec48673`) ran on GitHub for the first time on PR #4: green on all 4 checks. Containers lacking rustfmt or clippy still rely on the lead's host run.
- [ ] Toolchain drift between the host and the containers (item 15).

Also undecided: the repo-fit items in [repo-fit.md](repo-fit.md), such as the W7a/W7b split (§6). These are lead decisions unless escalated (unconfirmed).

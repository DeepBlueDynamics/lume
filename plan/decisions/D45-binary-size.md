# D45 — release binary size evaluation

Proposed profiles; release defaults are unchanged. Local measurements are complete; the selected-profile host correctness gate remains pending. Resumed on cebf5ea after the exact distinct pushdown.
The lead authorized this measurement after D44's mixed-revision two-architecture
npm tarball measured 82,767,412 bytes gzip.

## Method

All local measurements use x64 Linux, rustc 1.99.0 (b940084d7 2026-09-28),
Cargo 1.99.0, the same Cargo.lock and source revision. The integrator's
correctness build uses Windows rustc 1.96.0; those results are identified
separately. Cross-toolchain/platform absolute sizes or timings are not A/B pairs.

Every binary is built with features ti, CARGO_INCREMENTAL=0 and strip=symbols.
Gzip sizes use level 9, empty filename and mtime=0. They measure one binary,
not a two-architecture npm tarball. Artifacts, raw feature trees, build logs and
timing reports stay in .lanes/data/binary-size-cebf5ea outside git.
The pre-pause thin/1 binary at 02f826a is retained as historical evidence but is
excluded from the resumed A/B, because the classifier and query cache changed.

The repository release profile already ships fat LTO (lto=true), opt-level=3,
codegen-units=1 and unwind. D44's native container package used a thin-LTO
override. Both are measured. codegen-units=1 is therefore already in the
shipping profile; thin/16 versus thin/1 isolates the codegen-units option.

The per-package size experiment applies opt-level=s to pgwire, tokio, chrono,
zip, lopdf, quick-xml and ureq. Root lume stays at 3 because it contains Lume
BM25/search, not just CLI/agent glue. This follows the lead's approved exclusion;
no root-s performance claim is made. DataFusion, Arrow, roaring and ti-* stay at 3.

Builds are sequential, target is capped at 8 GB and cleaned between variants.
bench/binary_size_build.py records flags, elapsed build time and target peak,
and preserves both lume and ti-bench. It records cumulative maximum child RSS
from Linux getrusage as a build-memory indicator, not total concurrent memory
or a guarantee that an arm64 build will fit the Pi. It does not edit release defaults.

## Performance and correctness gates

Twenty fixed non-text golden queries span Q1, Q2, Q3, Q4, Q5, Q7 and Q8.
bench/binary_size.py corpus records the exact subset without changing its SQL.
The shard_cache harness runs them over the same read-only store-full, with
--cache-mode on and --cache-bytes 268435456: once cold then 31 warm
repetitions. Cold means cleared decoded application cache; OS cache is
uncontrolled. Each row reports warm p50. All 20 row counts and complete answer
fingerprints must agree across profiles.
The benchmark process runs in deep lane scratch so its optional DuckDB helper is
not found and git provenance remains the frozen lane revision; DuckDB is host-only. No model or network server is involved.

Text queries are excluded from timing because existing ti-bench opens TiEngine
without the root LumeText factory. The root-s optimization is excluded too.
The full host 61 passed / 0 failed / 1 excluded corpus gate covers text and count
semantics using the rebuilt count-paths-enabled store. The integrator offered
to build and verify the winning profile on the host; no not-yet-run gate is
claimed passed.

Each candidate is followed by a fat/1 control using the preserved baseline
runner, with no intervening build and the same 31 warm repetitions. These
adjacent controls reduce timing-drift ambiguity; the original baseline and all
runs are retained, with no best-run selection. Every control must match the
original baseline's 20 fingerprints before its timings are used.

The lead clarified acceptance during measurement: aggregate sum-of-query-p50 regression must be ≤5%, and class p50 regressions must be assessed with an absolute 1 ms noise floor. Here class p50 is explicitly the median of its constituent query p50s, not a pooled sample percentile. A class or individual query exceeding both 5% and 1 ms is flagged. Individual flags are reported separately, not used as automatic rejection; the lead explicitly accepted thin/1's +4.63% aggregate result for the Pi. The full host 61/0/1 gate remains required. All per-query measurements are retained.

## Recommended profiles and remaining gates

**Shipped server:** fat LTO, CGU=1, opt-level=3, panic=unwind, strip=symbols. This is the measured baseline. Retain the seven dependencies at opt-level 3: their size experiment saves only 187,648 raw / 55,294 gzip bytes (0.20% / 0.16%), too little to justify extra profile overrides. It passes the clarified timing gate (+1.43% aggregate, no material class/query regressions), but is not recommended because the measured size gain is negligible. Do not combine unmeasured options.

The reviewable Cargo.toml diff is bench/binary-size-winner.patch. It makes existing opt-level/unwind defaults explicit and adds strip=symbols, without applying it to this lane. On this checkout, check/apply with git apply --ignore-space-change because Cargo.toml uses CRLF. The lead will build the proposed profile with Rust 1.96 and run the rebuilt count-paths corpus gate (61/0/1); that specific profile verification is pending. The pre-existing cebf5ea native corpus acceptance is recorded separately in q5-profile.md and is not relabeled as a D45 winner run.

**Pi native build:** thin LTO, CGU=1, opt-level=3, panic=unwind, strip=symbols, one build job, incremental disabled. This uses the independently measured thin/1 profile, not an unmeasured thin-plus-abort or thin-plus-size combination.

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
CARGO_PROFILE_RELEASE_LTO=thin \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
CARGO_PROFILE_RELEASE_OPT_LEVEL=3 \
CARGO_PROFILE_RELEASE_PANIC=unwind \
CARGO_PROFILE_RELEASE_STRIP=symbols \
cargo build --locked --release --features ti --bin lume
```

The thin/1 production maximum single-child RSS is 1,819,734,016 bytes (1.69 GiB), measured on x64 Rust 1.99 with two build jobs; thin/16 is 1.38 GiB and fat/1 is 7.62 GiB. The Pi has about 3.5 GB available with Signal K running (lead-reported). **Inference:** thin/1 is a plausible fit, especially with one job; x64 maximum child RSS is neither an arm64 measurement nor total concurrent memory, so native peak RSS must be confirmed before claiming it fits. Run the native command under /usr/bin/time -v and monitor total memory/target (≤8 GB); clean afterwards.

Thin/1 passes the clarified aggregate/class timing gate versus its adjacent fat/1 control (+4.63% aggregate). The separately run thin/16 and thin/1 rows sum to 147.008 and 137.422 ms across the 20 query p50s respectively: thin/1 is 6.52% lower. Those are sequential profile runs, not a new interleaved A/B; adjacent fat controls remain the acceptance references. Thin/1 is also 30.91% smaller raw / 28.18% smaller gzip than thin/16. Native arm64 size and timings were not measured here. D44's 133,878,064-byte stripped arm64 thin/16 artifact is a different revision/toolchain; its size cannot be treated as a paired comparison to this x64 experiment. No predicted new two-architecture npm tarball size is claimed.

## Feature audit

Verified commands:

```sh
cargo tree --locked -e features -p ti-sql
cargo tree --locked -e normal -p ti-sql --format '{p} features=[{f}]' --prefix none
cargo metadata --locked --features ti --format-version 1
```

DataFusion 55.1.0 has top-level defaults disabled and explicitly enables
sql, parquet, nested_expressions, datetime_expressions, math_expressions,
string_expressions. Transitive features are additive.

| Package | Actually enabled | Decision |
|---|---|---|
| datafusion | sql/sqlparser/datafusion-sql; parquet/datafusion-datasource-parquet; nested_expressions/datafusion-functions-nested; datetime/math/string | SQL and Parquet are compiled API dependencies; datetime is used by date_trunc golden queries; nested registers array_has for source predicates |
| datafusion-functions | default, datetime, encoding, math, regex, string, unicode; base64, chrono-tz, hex, regex, uuid | Defaults are enabled inside DataFusion; removing our redundant top-level math/string flags cannot subtract these functions |
| datafusion-sql | default, unicode_expressions, unparser | Defaults are enabled by DataFusion; no caller-level subtraction |
| arrow 59.3.0 | default, csv/arrow-csv, json/arrow-json, ipc/arrow-ipc, prettyprint, chrono-tz, canonical_extension_types | DataFusion unconditionally enables default Arrow and prettyprint/timezones; no caller-level subtraction |
| arrow-ipc | default, lz4/lz4_flex, zstd | DataFusion dependencies enable compression; retain IPC for HTTP query output |
| parquet | default, arrow, async, object_store, brotli, flate2/rust_backend/zlib-rs, lz4/lz4_flex, snap, zstd, related Arrow/futures/tokio/base64 features | Keep every codec: W9 accepts arbitrary generic Parquet, so these are real inputs, not dead features |

Crypto expressions, DataFusion's file compression feature, Avro, Parquet
encryption, recursive protection/backtrace and Arrow PyArrow/FFI are absent.
Disabling default features on our own Arrow/Parquet references cannot override
defaults selected by upstream dependents. Upstream patches or new feature gates
would be required; none are proposed here. Codec removal is explicitly excluded.

No golden TI SQL directly calls array/struct/list/unnest functions, but the TI
source-equality/IN rewriter creates array_has expressions (ti-sql/src/rewrite.rs
and classifier.rs; the source column is List<Utf8>). Retain nested_expressions:
dropping automatic registration would break source predicates even if this
particular corpus passed. No currently enabled feature can safely be subtracted
by a caller-level Cargo toggle without either losing an existing API or retaining
the same upstream-additive feature. Selective function registration would be a
separate implementation and compatibility investigation, not a profile change.

## Panic/unwind audit

Static audit at cebf5ea: searched all root src/ and TI crates for catch_unwind, resume_unwind, thread spawning/joining, scoped threads, spawn_blocking and JoinSet. Read the matching production call sites and the locked Tokio 1.53.2, DataFusion 55.1.0 and pgwire 0.41.0 sources. This is a source audit, not a panic-injection test or a claim that unwind perfectly restores service state.

| Unwind dependency / boundary | Verified behavior with unwind | Effect of abort / coverage |
|---|---|---|
| src/main.rs:19–21 main thread join/resume_unwind | Joins the 64 MiB main-stack thread and rethrows its panic | Main-process failure already fatal; no containment gain |
| src/main.rs:62 and src/document_extract.rs:261 catch_unwind | Only production caller of document_extract::worker is the private __extract-document entrypoint; extract() starts current_exe as a separate bounded child (document_extract.rs:59–60). Tests/pdf_extract.rs calls it directly in disposable test processes | Parent isolation survives child abort, but a decoder panic loses all pages in that file instead of skipping one page. Both catches cease working under abort |
| src/document_extract.rs:149,202 reader thread and ignored join result | Parent detects a disconnected capture channel; joins reader. Child output is bounded and worker exit is checked | The reader itself is in the parent, not the disposable parser process; reader panic would now kill parent |
| src/agent.rs:841 HTTP/MCP connection threads | A panicking handler terminates its thread, leaving listener/ingest running. No join/catch around the handler; active-connection decrement is not guarded on panic | Uncovered shared-process boundary: abort kills ingest and every connection. Unwind containment is imperfect (slot can leak), but abort broadens the failure |
| src/main.rs:473 integrated query-server thread | Detached query-server thread shares the live ingest process | Server-thread panic becomes ingest-process termination under abort; not disposable |
| src/ti_pg.rs:114–117 blocking describe task | spawn_blocking JoinError (including panic) becomes PostgreSQL XX000 | Cannot produce JoinError after abort; whole process exits |
| src/ti_pg.rs:813–837 client JoinSet | Every process_socket runs in a Tokio task. Completed join results are discarded; listener continues even after a task panic | Abort defeats Tokio task containment. This is not an explicit pgwire catch; pgwire's src/ has no catch_unwind/resume_unwind/is_panic occurrence in the searched version |
| src/ti_pg.rs:779–785 listener Drop | Stops and joins listener thread; ignores thread panic result | Listener-thread panic becomes shared-process abort; no disposal boundary |
| ti-ingest/src/resources.rs:391–436 document poller | Separate thread, ignored join result on Drop; panic can stop docs polling while telemetry continues | Poller is not a child process; abort also stops telemetry |
| src/hybrid.rs:496 and src/main.rs:1495 scoped workers | std::thread::scope implicitly joins workers and propagates unhandled worker panics. These are indexing/Grub CLI paths, not a promised recovery mechanism | Abort skips orderly scope unwinding/cleanup; CLI process still ends |
| Tokio runtime/task/harness.rs:302,338,373,502,523,548 | Runtime guards task poll/drop/output with catch_unwind and stores panic JoinError; Lume's runtimes do not opt into a shutdown-on-panic policy | Abort bypasses every task panic boundary, including detached query execution. Additional internal unwind guards exist in Tokio sync/watch, signal/reusable_box and sync/task/atomic_waker |
| DataFusion physical-plan/buffer.rs:420–433 | BufferExec catches panics while polling input and sends an internal stream error, avoiding apparent clean EOF | Abort prevents conversion into an error and terminates the entire server |
| DataFusion physical-plan/stream.rs:134–148, execution_plan.rs:1817–1822, common-runtime/common.rs:87–91 | JoinError panic payloads are resumed in the consuming task/thread; they are not all converted to DataFusionError | Requires Tokio/request-thread unwind containment above; abort terminates ingest instead |
| TiServer shared query state (src/ti_http.rs:249,269,286,345,654) | Mutex poison is mapped to string errors; runtime.block_on executes request futures in-process | No process isolation or recovery proof for all state. Even unwind may leave a query gate poisoned, but ingest can continue; abort cannot |

**Decision: retain panic=unwind for both shipped server and Pi native profile.** The measured 13.56% binary / 16.31% gzip saving from abort is available only after hardening or moving every risky handler/query/poller into disposable processes and defining recovery/cleanup behavior. Supervisor restart is not equivalent to preserving live telemetry and other connections. The two extractor catches being in a child is insufficient to cover the other shared-process boundaries. No panic hardening is implemented by this measurement branch.

## Measurements

All rows use rustc 1.99.0 x64 Linux, source cebf5ea, strip=symbols, opt-level 3 except the seven dependencies in the s row. Size and gzip are bytes. Query medians below are medians of 20 individual warm p50s, not pooled percentiles; aggregate change uses their sum against the adjacent fat control. All variants and controls match all 20 fingerprints and row counts.

| Profile | Toolchain / arch | Binary bytes | Gzip bytes | Median query p50 ms | Aggregate change | Build max child RSS GiB | Recommendation |
|---|---|---:|---:|---:|---:|---:|---|
| Fat/1 unwind | 1.99.0 / x64 | 92,796,472 | 33,944,786 | 1.935 | +0.00% | 7.62 | shipped baseline |
| Thin/16 unwind | 1.99.0 / x64 | 146,331,296 | 48,565,453 | 2.107 | +10.65% | 1.38 | fails timing gate |
| Thin/1 unwind | 1.99.0 / x64 | 101,098,296 | 34,879,670 | 1.967 | +4.63% | 1.69 | Pi candidate; native memory confirmation |
| Fat/1 abort | 1.99.0 / x64 | 80,213,144 | 28,407,481 | 1.870 | +0.49% | 6.51 | timing passes; unwind audit rejects |
| Fat/1 dependency s | 1.99.0 / x64 | 92,608,824 | 33,889,492 | 1.826 | +1.43% | 7.65 | timing passes; negligible size gain |

Maximum target size across all builds was 3.50 GB (thin/16), below the 8 GB cap. The driver cleaned target between variants and after each completed build. Production RSS is the stage-0 cumulative maximum child metric; it is not a sum of simultaneously resident compiler processes.

CGU=16→1 with thin LTO saves 45,233,000 raw bytes (30.91%) and 13,685,783 gzip bytes (28.18%). The sum of query p50s is 6.52% lower in the separate thin/1 run. Fat/1 abort saves 12,583,328 raw bytes (13.56%) and 5,537,305 gzip bytes (16.31%), but is not safe to deploy under this audit. Dependency s saves 187,648 raw / 55,294 gzip bytes (0.20% / 0.16%).


### Twenty-query warm p50 matrix

Rust 1.99.0 x64, milliseconds; 31 warm repetitions, decoded cache ON (256 MiB). Column labels identify the separately built profile; adjacent fat controls used for the acceptance ratios are recorded below and in comparison.json.

| Query | Fat/1 | Thin/16 | Thin/1 | Fat/1 abort | Fat/1 dependency s |
|---|---:|---:|---:|---:|---:|
| q1-001 | 0.456 | 0.564 | 0.515 | 0.425 | 0.464 |
| q1-002 | 0.457 | 0.557 | 0.526 | 0.414 | 0.463 |
| q1-004 | 0.460 | 0.578 | 0.540 | 0.449 | 0.556 |
| q2-002 | 0.827 | 0.930 | 0.883 | 0.774 | 0.778 |
| q2-003 | 1.246 | 1.318 | 1.267 | 1.206 | 1.275 |
| q2-005 | 1.807 | 2.025 | 1.825 | 1.728 | 1.755 |
| q3-001 | 0.933 | 1.267 | 0.983 | 0.894 | 0.964 |
| q3-002 | 1.165 | 1.380 | 1.240 | 1.116 | 1.167 |
| q3-004 | 0.672 | 0.823 | 0.743 | 0.655 | 0.660 |
| q4-001 | 1.931 | 2.188 | 2.076 | 2.011 | 1.897 |
| q4-002 | 3.999 | 4.119 | 3.905 | 3.934 | 3.706 |
| q4-005 | 2.283 | 2.469 | 2.447 | 2.399 | 2.282 |
| q5-001 | 7.060 | 8.395 | 6.404 | 6.524 | 6.437 |
| q5-002 | 8.025 | 8.513 | 9.204 | 8.085 | 7.330 |
| q7-001 | 1.939 | 1.956 | 1.857 | 1.727 | 1.680 |
| q7-002 | 3.143 | 3.348 | 3.067 | 3.004 | 2.892 |
| q7-006 | 55.384 | 58.959 | 56.186 | 55.836 | 55.601 |
| q8-001 | 17.609 | 20.054 | 18.098 | 17.629 | 17.136 |
| q8-002 | 12.573 | 13.763 | 12.770 | 12.590 | 12.342 |
| q8-006 | 12.997 | 13.802 | 12.884 | 12.332 | 12.474 |

### Paired class medians

Each cell is adjacent fat control → candidate, milliseconds (percent change). Class p50 is the median of its constituent query p50s; individual flags follow. A material class regression exceeds both 5% and 1 ms. Only thin/16 has material class regressions (Q5 and Q8).

| Class | Thin/16 | Thin/1 | Abort | Dependency s |
|---|---|---|---|---|
| Q1 | 0.457→0.564 (+23.39%) | 0.439→0.526 (+19.80%) | 0.463→0.425 (-8.12%) | 0.451→0.464 (+2.92%) |
| Q2 | 1.236→1.318 (+6.61%) | 1.342→1.267 (-5.55%) | 1.239→1.206 (-2.66%) | 1.209→1.275 (+5.43%) |
| Q3 | 0.933→1.267 (+35.79%) | 0.958→0.983 (+2.53%) | 0.925→0.894 (-3.27%) | 0.940→0.964 (+2.52%) |
| Q4 | 2.334→2.469 (+5.75%) | 2.467→2.447 (-0.83%) | 2.336→2.399 (+2.71%) | 2.264→2.282 (+0.81%) |
| Q5 | 7.285→8.454 (+16.05%) | 7.094→7.804 (+10.01%) | 7.122→7.305 (+2.56%) | 6.912→6.883 (-0.42%) |
| Q7 | 2.956→3.348 (+13.28%) | 2.994→3.067 (+2.44%) | 3.052→3.004 (-1.58%) | 2.975→2.892 (-2.79%) |
| Q8 | 12.672→13.802 (+8.92%) | 12.345→12.884 (+4.36%) | 12.677→12.590 (-0.68%) | 12.300→12.474 (+1.42%) |

### Individual material flags

These queries exceed both 5% and 1 ms versus their adjacent controls. They remain visible even when the aggregate/class gate accepts thin/1. Abort and dependency s have no individual material flags.

| Profile | Query | Control ms | Candidate ms | Increase ms | Increase % |
|---|---|---:|---:|---:|---:|
| Thin/1 unwind | q5-002 | 7.569 | 9.204 | 1.635 | +21.60% |
| Thin/1 unwind | q8-001 | 16.982 | 18.098 | 1.116 | +6.57% |
| Thin/16 unwind | q5-001 | 6.326 | 8.395 | 2.068 | +32.69% |
| Thin/16 unwind | q7-006 | 54.914 | 58.959 | 4.045 | +7.37% |
| Thin/16 unwind | q8-001 | 17.153 | 20.054 | 2.901 | +16.91% |
| Thin/16 unwind | q8-002 | 12.672 | 13.763 | 1.091 | +8.61% |
| Thin/16 unwind | q8-006 | 12.360 | 13.802 | 1.442 | +11.67% |

### Validation and artifacts

- Five root-only stripped release binaries and five matched-profile ti-bench runners built successfully, sequentially, on Rust 1.99.0.
- All 20 fixed golden answer fingerprints/row counts match across five variants and every control. Each run uses 31 warm iterations and 256 MiB decoded cache; cold is recorded but not a native-host latency claim.
- python3 -m unittest discover -s bench -p 'test_*.py': 56 passed (TMPDIR and CARGO_TARGET_TMPDIR in lane scratch). This includes six comparison tests and five cache-helper tests. The separate bench/grafana discovery command was not rerun; no Grafana files changed.
- git diff --check and git apply --check --ignore-space-change bench/binary-size-winner.patch passed. Cargo.toml/Cargo.lock and Rust sources are unchanged in this branch.
- Raw trees, build commands/logs/peak sizes, size.json, deterministic gzip, timing reports and comparison.json are under .lanes/data/binary-size-cebf5ea; no generated binaries or raw reports are committed. The older 7-repeat fat pilot is retained in fat/timing-pilot7 and excluded from the 31-repeat comparison.
- Proposed-profile Rust 1.96 host corpus gate and native Pi thin/1 memory/size measurement are pending, not claimed passed. The lead-reported DISTINCT gates at cebf5ea are recorded in docs/performance-comparisons.md §7 and plan/design/q5-profile.md.

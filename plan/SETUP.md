# Lume TI — Developer and agent setup

Read this first if you are joining the Lume TI build. Current assignments are in [STATUS.md](STATUS.md).
The plan itself is in [README.md](README.md).

Items marked *(unconfirmed)* are conventions the docs keeper has not verified. Ask the lead before relying on them.

## 1. The repo in one paragraph

Lume is a Rust crate (`lume` 0.12.0, edition 2021): `src/lib.rs` plus a CLI in `src/main.rs`.
By default it has four direct dependencies (`tantivy-fst`, `ureq`, `serde`, `serde_json`) and a committed `Cargo.lock`.
The repo is now a Cargo workspace. The root `lume` package stays at `.`, and Lume TI (the telemetry index) lives in members under `crates/ti-*` (merged so far: `crates/ti-contracts`, `crates/ti-core`, `crates/ti-store`, `crates/ti-sql`, `crates/ti-ingest`, `crates/ti-bench` and `crates/ti-geo`). TI is compiled only behind the `ti` cargo feature, which is off by default.

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
- There is no `rust-toolchain` file yet. The host build pane runs rustc **1.96.1** and the containers run **1.99**, and their clippy lints differ. A pinned `rust-toolchain.toml` has been proposed to the user (unconfirmed until accepted).
- CI runs only on pushes and PRs to `main`. Lane branches and `plan/lume-ti` get **no CI**, so run the commands above yourself before you report a commit.
- `release.yml` builds release binaries on `v*` tags for five gnu/darwin/msvc targets. It has no musl targets and no `--features ti` build yet ([repo-fit §4](repo-fit.md)).

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

**Golden corpus verify (full store).** This closed M3 (`711d2c4`) and now covers the full corpus, including geo and `intervals()` (`cb39a4d`: **58 passed, 0 failed, 4 excluded**). Build the store once, then verify the golden corpus against it:

```sh
TI_OPT_IN=last cargo run --release -p ti-ingest --example backfill_store -- .lanes/data/correctness/tier=raw .lanes/data/store-full
cargo build --release --features ti --bin lume
target/release/lume.exe ti verify --store .lanes/data/store-full --corpus tests/golden
```

- In PowerShell, set `$env:TI_OPT_IN = 'last'` first. `TI_OPT_IN=last` adds the `@last` opt-in aggregate the corpus needs. `backfill_store` also pre-registers vessels from `catalog/vessels` and path scales from `catalog/paths`, then reports raw vs index bytes. On the host: 436.5 s backfill (219,779 rows/s), 65 shards sealed in 11.6 s, index 729.3 MB vs 1,892.7 MB raw (0.39×). The store needs about 0.7 GB of disk.
- **The `ti` feature is required.** Without `--features ti`, `lume` reports `Unknown subcommand: ti`.
- Verify tolerance is ±1 × 10^−scale (D34). A corpus entry with an `exclude` reason in `tests/golden/corpus.json` is skipped and counted as excluded. Geo and `intervals()` always run. Text entries are skipped only when the store has no document index.
- On the host, opening `store-full` is fast: `Store::open` 0.07 s, `session_from_store` 1.78 s, `lume ti status` 2.17 s wall. Through a container bind mount it can take about 70 s. The `ti-sql` example `open_timing` prints per-stage timings.
- **Text (`match()`, `docs`):** `backfill_store` now imports the docs automatically from `<ancestor>/docs`. To load the 1,610 golden documents into an existing store, run `cargo run --release -p ti-ingest --example import_docs -- .lanes/data/correctness/docs .lanes/data/store-full`. They are stored in `<store>/docs/documents.json`. `lume ti import-docs` (below) does the same thing.
- Regenerate the golden expected outputs (DuckDB, testing only) with `py -3 -E tests/golden/gen_expected.py --data-dir .lanes/data/correctness`. See §8 for its other flags. The oracles round per-bucket aggregates to the path scale before filtering (D35), and text oracles cover `[ts_start, ts_end)` (D36).

**`lume ti` CLI** (`31fd76a`; needs `--features ti`, like `verify`):

```sh
lume ti query "<sql>" --store <root> [--json] [--width <seconds>]
lume ti explain "<sql>" --store <root> [--json] [--width <seconds>]
lume ti status --store <root> [--width <seconds>]
lume ti import-docs <docs_dir> --store <root> [--width <seconds>]
```

- `--store` is required. By default the bucket width is read from the store. `--width` overrides it.
- The same engine backs the MCP tools `ti_query`, `ti_schema`, `ti_explain`, `ti_status` and `ti_resolve` (`src/ti_mcp.rs`). `ti_query` is read-only and capped at 500 rows and 64 KiB.

**`lume serve` with TI** (`bce7779`, `39c0096`; needs `--features ti`):

```sh
lume serve --ti-store <store> [--bind <IP>] [--port <PORT>]
```

- With `--ti-store`, the server binds to **loopback `127.0.0.1` by default**. Pass `--bind <IP>` to expose it. Plain `lume serve` (no TI) still binds `0.0.0.0`. `/ti` and the TI server's `/mcp` send no wildcard CORS.
- One shared engine serves both MCP and HTTP:

  | Endpoint | Does |
  |---|---|
  | `POST /ti/query` | Read-only SQL. Returns Arrow IPC (`application/vnd.apache.arrow.stream`) or JSON, chosen by the `Accept` header |
  | `GET /ti/schema` | Tables and columns |
  | `POST /ti/explain` | Query plan and pushdown |
  | `GET /ti/status` | Store status |
  | `GET /ti/resolve?q=<phrase>` | `ti_resolve`: phrase to column (93/100 top-3 on `tests/golden/resolve.json`) |

- Read-only pgwire (`--pg-port`, off by default, loopback) is in progress and not merged.

A plain `cargo test` only tests the root crate, because `default-members = ["."]`.

**Strict checks, scoped to each TI crate.** Run both for every `crates/ti-*` crate you touch (currently `ti-contracts`, `ti-core`, `ti-store`, `ti-sql`, `ti-ingest`, `ti-bench` and `ti-geo`):

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
The repo has no `.gitattributes`.

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
- Since `3354bd0` the dev profile uses `debug = "line-tables-only"` and no debug info for dependencies. Full DataFusion debug info had grown the caches to about 81 GB across `target/` and the clones. **Keep each agent's `target/` under 15 GB.** At the last check (2026-10-06), the host had 42.9 GB free and root `target/` was 8.7 GB.
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

  The log also records pure-Rust Parquet codecs only, with no zstd (D11), `blake3` with `pure` (D12), and an early aarch64-musl smoke test for pgwire SCRAM, which pulls in `ring` (D13). **D27 amends D11:** `zstd-sys` (C) is accepted because DataFusion's `arrow-ipc` forces it in. That deviates from the spec's croaring-only C rule and is awaiting spec-owner confirmation. D18 (`serde`) and D19 (`toml`) cover the dependencies of `ti-contracts`. D23 is `proptest` (a `ti-core` dev-dependency). D24 (`bincode`) and D25 (`crc32fast`) are in `ti-store`. D22 (`ti-bench`: arrow/parquet 59.2 with pure-Rust codecs, chrono) is logged (`71b7fcd`). D26 (DataFusion) and D27 (`zstd-sys`) are logged. D28 (`tungstenite`) and D29 (`parquet`) are reserved for W3. D30 is the 1 s high-resolution store ([design/hi-res-store.md](design/hi-res-store.md)). D31 (`h3o` 0.11 + `geo` 0.33.1, exact-pinned) is logged for `ti-geo`. D32 keeps golden expected outputs small (target ≤ 256 KB per entry, a few MB in total; narrow the window in both twins rather than store huge results). D33 (fixed default aggregate profile per path), D34 (verify tolerance ±1 × 10^−scale), D35 (oracles round bucket aggregates to scale) and D36 (`match()` is lexical Lume BM25: OR by default, uppercase `AND` intersects, docs cover `[ts_start, ts_end)`) are behavioral, not dependencies. The next free number is **D37**. Ask the lead before taking it.
- The contracts crate itself depends on `arrow-array`, `arrow-schema`, `roaring`, `serde` and `toml` only, not DataFusion.

## 10. How the lead merges

The lead (Industrial Pike) is the only one who writes to `plan/lume-ti`.

1. An agent reports a ready commit by mail.
2. The lead fetches the branch straight from the clone, for example `git fetch .lanes/w1 ti/w1-core`.
3. The lead reviews it and merges it into `plan/lume-ti` in the shared tree. Merge style (merge commit, squash or fast-forward) is the lead's call *(unconfirmed)*.
4. The docs keeper refreshes [STATUS.md](STATUS.md) and this file after each merge.
5. When `plan/lume-ti` is merged to `main` is not yet decided *(unconfirmed)*. The spec wants lanes integrated on `main` daily behind the `ti` flag ([spec/10](spec/10-contracts.md)).

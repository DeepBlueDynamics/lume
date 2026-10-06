# Repo fit — spec vs the current Lume codebase

Checked against `main` @ 867c8db (lume 0.12.0) on 2026-10-05. These are
decisions to make before or during M0; none are in the spec.

## 1. Single crate → workspace

Today: one crate, `src/lib.rs` + `src/main.rs` (2,427 lines, hand-rolled arg parsing).
Spec: crates under `crates/ti-*` in "the Lume workspace", behind a `ti` feature.

- Need a root `[workspace]` in `Cargo.toml`. Either keep `lume` as the root package
  plus members, or move it to `crates/lume`. The first is less churn for README links
  and CI.
- `lume ti …` subcommands get dispatched from `src/main.rs`'s `match` on the first arg.

## 2. Roaring implementation

Today: `src/fast_retrieval.rs::MiniRoaring` — hand-rolled, `HashMap<u16, Container>`
with Array and Bitmap containers only. No run containers, no portable
serialization, no `RoaringTreemap`. Serialized via serde JSON.
Spec assumes: `RoaringBitmap` / `RoaringTreemap` types, roaring **portable format**
`.rbm` files, run containers (used by `intervals()`), and a later `croaring` evaluation.

Recommendation: adopt the `roaring` crate for TI (decisions log D8) and leave
`MiniRoaring` in place for BM25. Spec decision D2's "BM25 postings already
roaring-native" is only loosely true: the postings are roaring-*shaped*, not
format-compatible.

## 3. Lume search as a library

Today: MCP tools in `src/agent.rs` run searches by **shelling out to the `lume` CLI**
(`run_lume_cli`), not by calling functions. `Bm25Index::search` (`src/bm25.rs:445`) and
`query_semantic_search` (`src/hybrid.rs:637`) exist, but the hybrid + SKG pipeline
lives in `handle_search` in `main.rs`.
Spec needs: in-process Lume search for `ti-text` (`match()`) and `ti_resolve`.

→ Before W5/W7, pull a `lume::search(...)` library entry point out of `main.rs`.
This is pre-work for W5 and W7, and it should start in week 1.
Design proposal: [design/search-api.md](design/search-api.md).

## 4. Dependency footprint

Today: 4 dependencies; `Cargo.toml` describes Lume as "zero-dependency".
Spec adds: DataFusion + arrow + parquet, roaring, h3o, pgwire, blake3, bincode,
crc32, a WebSocket client, proptest, DuckDB (dev/oracle), plus an async runtime
(DataFusion and pgwire need tokio).

- Keep all of it behind the `ti` feature so the default `lume` build stays small.
- Update the crate description.
- The release workflow already cross-compiles `aarch64-unknown-linux-gnu` with a gcc
  cross linker (`.github/workflows/release.yml:29–31,54–64`). What's missing is
  **static musl** targets (`aarch64`/`x86_64-unknown-linux-musl`) built with `--features ti`,
  plus packaging. DataFusion makes those builds slow and large.

## 5. Server model

Today: `lume serve` is a blocking `std::net::TcpListener` with thread-per-connection,
hand-written HTTP, and a 503 above `MAX_CONCURRENT_CONNECTIONS`. It binds `0.0.0.0`.
Spec: MCP + `/ti/*` HTTP (Arrow IPC streaming) + pgwire on the same process,
LAN-only bind on the boat, NUTS auth on shore.

→ Either host an async runtime beside the existing loop, or move `serve` to an async
HTTP stack when `ti` is enabled. The `0.0.0.0` bind (`src/agent.rs:718`) is fine
inside a container whose ports are published only to the LAN (the HaLOS path). For bare
OpenPlotter installs, define the exposure boundary: a configurable bind address with a
LAN default. Don't switch to loopback, because the plugin webapp and psql clients are on the LAN.

## 6. Lane sizing

W7 covers Rust MCP/HTTP/pgwire/CLI, a Node Signal K plugin, a webapp, a HaLOS
container package, Grafana provisioning, and systemd/thermal governance. That is
several lanes' worth of work in a 3-week window. Proposal: split it into
**W7a `ti-serve`** (Rust surfaces) and **W7b packaging** (plugin, webapp, container,
Grafana).

## 7. CI

Today: clippy runs as informational only (`|| true`), with about 10 existing warnings,
and there is no `fmt` check.
Spec: `clippy -D warnings` and `fmt --check` on every PR.

→ Either clean the existing crate first, or scope the strict checks to `crates/ti-*`.
The crash test (kill -9) and the Pi benchmarks need a Linux job and real hardware.

## 8. Things the spec references but doesn't define

- `AggOp`, `AggPartial`, `ShardManifestEntry`, and the `Result` error type (W0 to define)
- How the golden queries split across Q1–Q8
- The Meridian VHF transcript format
- ~~Whether `match()` is pure BM25 or hybrid~~. The spec already says BM25 for `match()`
  (p. 10) and hybrid for `ti_resolve` (p. 17). Making `match()` hybrid would be a deviation
  needing a decision. `ti_resolve` on the boat still needs a no-Shivvr fallback
  (`SearchMode::LexicalOnly` in [design/search-api.md](design/search-api.md)).

## 9. Contradictions inside the spec

Recorded as open questions in [spec/11-risks-decisions.md](spec/11-risks-decisions.md):

- "Sealed shards are the only thing that moves to shore" (p. 7) vs open-shard WAL-tail
  shipping every 5 min (p. 20).
- The Done criterion says the HALPI install comes "from the Signal K App Store" (p. 2), but
  the HaLOS install path is the container store (p. 19).

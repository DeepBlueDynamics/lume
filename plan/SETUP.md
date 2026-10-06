# Lume TI — Developer and agent setup

Read this first if you are joining the Lume TI build. Current assignments are in [STATUS.md](STATUS.md).
The plan itself is in [README.md](README.md).

Items marked *(unconfirmed)* are conventions the docs keeper has not verified. Ask the lead before relying on them.

## 1. The repo in one paragraph

Lume is a Rust crate (`lume` 0.12.0, edition 2021): `src/lib.rs` plus a CLI in `src/main.rs`.
By default it has four direct dependencies (`tantivy-fst`, `ureq`, `serde`, `serde_json`) and a committed `Cargo.lock`.
The repo is now a Cargo workspace. The root `lume` package stays at `.`, and Lume TI (the telemetry index) lives in members under `crates/ti-*` (so far only `crates/ti-contracts`). TI is compiled only behind the `ti` cargo feature, which is off by default.
The golden SQL corpus is in `tests/golden/` (see its `README.md`).
The work happens on branch `plan/lume-ti`, not `main`.

## 2. Build and test the existing crate

These are the commands CI runs (`.github/workflows/ci.yml`, on Ubuntu, macOS and Windows, stable toolchain):

```sh
cargo build --locked --verbose
cargo test --locked --verbose
cargo clippy --all-targets || true   # informational only; the existing crate has warnings
```

- Always pass `--locked`. CI fails if `Cargo.lock` would change.
- There is no `rust-toolchain` file, so use current stable.
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
```

A plain `cargo test` only tests the root crate, because `default-members = ["."]`.

**Strict checks, scoped to each TI crate.** Run both for every `crates/ti-*` crate you touch (currently `ti-contracts`; `ti-bench` is coming in corpus part 2):

```sh
cargo clippy -p <ti crate> -- -D warnings
cargo fmt -p <ti crate> --check
```

A root-wide `cargo fmt --check` is **not** required. The baseline `src/` isn't rustfmt-clean ([repo-fit §7](repo-fit.md)), so don't reformat `src/` as a side effect of TI work.
These checks are not in `ci.yml` yet, so run them yourself.

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
4. **Work and commit only inside your clone.** Never edit the shared working tree at the repo root. Never touch another agent's clone.
5. **Don't push.** The lead fetches your branch straight from your clone.
6. **Stay current when asked.** The clone's `origin/plan/lume-ti` is a snapshot from when it was cloned. `git fetch origin` refreshes it. Ask the lead before you merge or rebase onto a newer `plan/lume-ti` *(unconfirmed convention)*.
7. **Before reporting a commit,** run the section 2 commands, plus the section 3 commands for any TI crate you touched, in your clone, and check line endings (section 6).

## 5. Reporting protocol

- Report to the lead (**Industrial Pike**, pane `ee764a09`) by Hyperia mail. Agents may also mail each other directly.
- A report should give your branch, the commit hash(es) ready to merge, what changed, which tests you ran, and anything blocked or decided. The field list is a suggestion *(unconfirmed)*.
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
- For subcommands (mail, pane access), run `hyperia --help`. The exact CLI syntax is not documented here *(unconfirmed)*.
- Agents running as Claude Code on the host (such as the lead) use the Hyperia MCP tools instead.

## 8. The host build pane

**Regular Pheasant** (pane `364a3fc7`) is a PowerShell 7 pane on the Windows host with the Windows Rust toolchain.

- Use it when your container has no cargo, or when you need a Windows build.
- Each agent may **split it once**. Run your builds in your split, not in the original pane.
- Build inside your own clone, e.g. `cd C:\Users\kordl\Code\DeepBlueDynamics\lume\.lanes\<name>`. Each clone has its own `target/`, so lanes don't overwrite each other's builds.
- Use PowerShell syntax there (`$env:VAR = 'x'`, not `VAR=x`).
- Some containers also have cargo locally. Either is fine; say which one you used in your report.

## 9. Dependency policy

- **Every new runtime dependency needs a line in the decisions log** ([spec/11](spec/11-risks-decisions.md), D8+) before it merges. This is a PR rule from [spec/10](spec/10-contracts.md).
- **All TI dependencies are optional and gated behind the `ti` feature.** The default `lume` build keeps its 4 direct dependencies.
- **No C dependencies, except as decided.** The spec allows C bindings only for `croaring`, and only after the M4 evaluation. Decisions so far (from the dependency survey, logged as D8–D15):

  | Dependency | Decision |
  |---|---|
  | DataFusion | `=55.1.0`, `default-features = false` (its defaults pull in C compression libs) |
  | arrow | `"59.2"`, resolves to 59.3.0 via `Cargo.lock` |
  | roaring | `0.11.5` for TI. The existing `MiniRoaring` stays in place for BM25 |
  | DuckDB | Used as an oracle through the **CLI**, not as a bundled Rust crate |
  | Release builds | `cargo-zigbuild` for musl targets |

  The log also records pure-Rust Parquet codecs only, with no zstd (D11), `blake3` with `pure` (D12), and an early aarch64-musl smoke test for pgwire SCRAM, which pulls in `ring` (D13). The next free number is D18.
- The contracts crate itself depends on `arrow-array`, `arrow-schema` and `roaring` only, not DataFusion.

## 10. How the lead merges

The lead (Industrial Pike) is the only one who writes to `plan/lume-ti`.

1. An agent reports a ready commit by mail.
2. The lead fetches the branch straight from the clone, for example `git fetch .lanes/w0 ti/w0-contracts`.
3. The lead reviews it and merges it into `plan/lume-ti` in the shared tree. Merge style (merge commit, squash or fast-forward) is the lead's call *(unconfirmed)*.
4. The docs keeper refreshes [STATUS.md](STATUS.md) and this file after each merge.
5. When `plan/lume-ti` is merged to `main` is not yet decided *(unconfirmed)*. The spec wants lanes integrated on `main` daily behind the `ti` flag ([spec/10](spec/10-contracts.md)).

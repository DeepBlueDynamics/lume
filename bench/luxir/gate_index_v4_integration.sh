#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
case "$(rustc --version)" in
  "rustc 1.96."*) ;;
  *) echo "Stage 1 integration gate requires rustc 1.96" >&2; exit 1 ;;
esac
rustup component add rustfmt clippy
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export TMPDIR=/tmp
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
trap 'cargo clean' EXIT
# agent.rs has pre-existing unformatted code; only this slice's formatter hunks were applied.
rustfmt --edition 2021 --config skip_children=true --check src/main.rs src/search.rs src/resident_index.rs src/ti_docs_index.rs src/index_binary.rs src/index_binary/*.rs tests/index_v4_core.rs tests/index_v4_generation.rs tests/index_v4_snapshot.rs
cargo test --locked --features ti --test index_v4_core --test index_v4_generation --test index_v4_snapshot
cargo test --locked --features ti --lib
cargo clippy --locked --features ti --all-targets -- -D warnings
du -sh "$CARGO_TARGET_DIR"

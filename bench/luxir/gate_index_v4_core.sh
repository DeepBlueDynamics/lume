#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
case "$(rustc --version)" in
  "rustc 1.96."*) ;;
  *) echo "Stage 1 gate requires rustc 1.96" >&2; exit 1 ;;
esac
rustup component add rustfmt clippy
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export TMPDIR=/tmp
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
trap 'cargo clean' EXIT
rustfmt --edition 2021 --config skip_children=true --check src/bm25.rs src/index_binary.rs src/index_binary/*.rs tests/index_v4_core.rs
cargo test --locked --features ti --test index_v4_core
cargo test --locked --features ti --lib
cargo clippy --locked --features ti --all-targets -- -D warnings
du -sh "$CARGO_TARGET_DIR"

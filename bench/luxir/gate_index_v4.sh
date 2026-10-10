#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
# The combined H2/meta gate includes atomic-index and every integration test.
bash bench/luxir/gate_h2_local.sh
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export TMPDIR=/tmp
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
trap 'cargo clean' EXIT
rustfmt --edition 2021 --config skip_children=true --check src/index_timing.rs src/bm25.rs src/meta.rs tests/index_timing.rs tests/index_checkpoint.rs tests/lume_sql.rs
cargo build --release --locked --features ti --bin lume
binary_output="${LUME_INDEX_V4_BINARY:-/bench/bin/lume-index-stage0}"
install -m 755 "$CARGO_TARGET_DIR/release/lume" "$binary_output"
sha256sum "$binary_output"
du -sh "$CARGO_TARGET_DIR"

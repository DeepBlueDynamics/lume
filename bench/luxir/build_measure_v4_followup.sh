#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
case "$(rustc --version)" in
  "rustc 1.96."*) ;;
  *) echo "Requires rustc 1.96" >&2; exit 1 ;;
esac
rustup component add rustfmt clippy
apt-get update
apt-get install -y --no-install-recommends python3 nodejs
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export TMPDIR=/tmp
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
trap 'cargo clean' EXIT
check_target() {
  local target_bytes
  target_bytes=$(du -sb "$CARGO_TARGET_DIR" | cut -f1)
  if (( target_bytes > 8 * 1024 * 1024 * 1024 )); then
    echo "Target exceeds the 8 GiB budget" >&2
    exit 1
  fi
}
rustfmt --edition 2021 --config skip_children=true --check src/bm25.rs src/index_timing.rs src/index_binary/bm25_codec.rs src/index_binary/snapshot.rs
cargo test --locked
check_target
cargo clippy --locked --all-targets -- -D warnings
check_target
cargo clean
cargo test --locked --features ti
check_target
cargo clippy --locked --features ti --all-targets -- -D warnings
check_target
cargo clean
cargo build --release --locked --features ti --bin lume
check_target
binary_output="${LUME_V4_BINARY_OUTPUT:-/bench/bin/lume-index-v4-followup}"
install -m 755 "$CARGO_TARGET_DIR/release/lume" "$binary_output"
sha256sum "$binary_output"
python3 bench/luxir/measure_v4_snapshot.py --binary "$binary_output" \
  --source /indexes/trec-covid-files \
  --existing /indexes/hot-path/trec-covid/stemmed \
  --new-db /measuredb/trec-v4 --query-db /measuredb/query-index \
  --queries /bench/trec-covid/queries.tsv --output "${LUME_V4_MEASURE_OUTPUT:-/bench/index-v4-build-followup}"
du -sh "$CARGO_TARGET_DIR"

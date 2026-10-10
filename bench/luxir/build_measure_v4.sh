#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
case "$(rustc --version)" in
  "rustc 1.96."*) ;;
  *) echo "Measurement requires rustc 1.96" >&2; exit 1 ;;
esac
if ! command -v python3 >/dev/null; then
  apt-get update
  apt-get install -y --no-install-recommends python3
fi
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export TMPDIR=/tmp
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
trap 'cargo clean' EXIT
cargo build --release --locked --features ti --bin lume
target_bytes=$(du -sb "$CARGO_TARGET_DIR" | cut -f1)
if (( target_bytes > 8 * 1024 * 1024 * 1024 )); then
  echo "Target exceeds the 8 GiB budget" >&2
  exit 1
fi
binary_output=/bench/bin/lume-index-v4-stage1
install -m 755 "$CARGO_TARGET_DIR/release/lume" "$binary_output"
sha256sum "$binary_output"
python3 bench/luxir/measure_v4_snapshot.py --binary "$binary_output" \
  --source /indexes/trec-covid-files \
  --existing /indexes/hot-path/trec-covid/stemmed \
  --new-db /measuredb/trec-v4 --query-db /measuredb/query-index \
  --queries /bench/trec-covid/queries.tsv --output /bench/index-v4-stage1
du -sh "$CARGO_TARGET_DIR"

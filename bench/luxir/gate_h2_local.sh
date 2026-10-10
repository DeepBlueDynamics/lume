#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
export TMPDIR=/tmp
mkdir -p "$CARGO_TARGET_TMPDIR"
trap 'cargo clean' EXIT
rustc --version
rustc --version | awk '{ if ($2 !~ /^1\.96\./) exit 1 }'
rustup component add rustfmt clippy
if ! command -v node >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
    apt-get update
    apt-get install -y --no-install-recommends nodejs python3
fi
node -e 'if (Number(process.versions.node.split(".")[0]) < 18) process.exit(1)'
# Format copies only: preserve the source until its changed hunks are reviewed.
rustfmt --edition 2021 --config skip_children=true /bench/h2-local-format/src/*.rs /bench/h2-local-format/tests/*.rs
status=0
rustfmt --edition 2021 --config skip_children=true --check src/local_vectors.rs src/search.rs src/hybrid.rs src/resident_index.rs src/main.rs src/lib.rs tests/local_vectors_cli.rs || status=1
cargo clippy --locked -p lume --features lume/ti -p ti-contracts -p ti-core -p ti-store -p ti-ingest -p ti-sql -p ti-sync -p ti-bench -p ti-geo --all-targets -- -D warnings || status=1
cargo test --locked --no-fail-fast --features ti || status=1
du -sh "$CARGO_TARGET_DIR"
export LUME_BIN="$CARGO_TARGET_DIR/debug/lume"
python3 -m unittest discover -s bench -p 'test_*.py' || status=1
python3 -m unittest discover -s bench/grafana -p 'test_*.py' || status=1
python3 -m unittest discover -s bench/luxir -p 'test_*.py' || status=1
exit "$status"

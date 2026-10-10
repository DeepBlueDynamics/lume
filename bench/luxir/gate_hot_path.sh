#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
export TMPDIR=/tmp
mkdir -p "$CARGO_TARGET_TMPDIR"
trap 'cargo clean' EXIT
if ! command -v node >/dev/null 2>&1; then
    echo 'Full TI gates require Node.js >=18; install nodejs in the gate image before running.' >&2
    exit 1
fi
node -e 'if (Number(process.versions.node.split(".")[0]) < 18) { console.error("Full TI gates require Node.js >=18"); process.exit(1); }'
rustc --version
rustc --version | awk '{ if ($2 !~ /^1\.96\./) exit 1 }'
rustup component add rustfmt clippy
rustfmt --edition 2021 --check src/bm25.rs src/search.rs
cargo clippy --locked -p lume --features lume/ti -p ti-contracts -p ti-core -p ti-store -p ti-ingest -p ti-sql -p ti-sync -p ti-bench -p ti-geo --all-targets -- -D warnings
du -sh "$CARGO_TARGET_DIR"
cargo test --locked --features ti --no-fail-fast
du -sh "$CARGO_TARGET_DIR"

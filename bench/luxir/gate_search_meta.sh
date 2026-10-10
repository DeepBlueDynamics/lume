#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/target}"
export CARGO_TARGET_TMPDIR="${CARGO_TARGET_TMPDIR:-/tmp/cargo-tmp}"
export TMPDIR="${TMPDIR:-/tmp}"
export PYTHONDONTWRITEBYTECODE=1
mkdir -p "$CARGO_TARGET_DIR" "$CARGO_TARGET_TMPDIR"
cleanup() {
    local rc=$?
    cargo clean 2>/dev/null || rm -rf "${CARGO_TARGET_DIR:?}"/* 2>/dev/null || true
    exit "$rc"
}
trap cleanup EXIT

if ! command -v node >/dev/null 2>&1; then
    echo 'Full TI gates require Node.js >=18; install nodejs in the gate image before running.' >&2
    exit 1
fi
node -e 'if (Number(process.versions.node.split(".")[0]) < 18) { console.error("Full TI gates require Node.js >=18"); process.exit(1); }'
rustc --version
rustc --version | awk '{ if ($2 !~ /^1\.96\./) exit 1 }'
rustup component add rustfmt clippy

echo "=== 1. Checking format on touched files ==="
rustfmt --edition 2021 --check \
    src/meta.rs \
    src/search.rs \
    src/sql.rs \
    tests/lume_sql.rs

echo "=== 2. Strict clippy ==="
cargo clippy --locked -p lume --features lume/ti -p ti-contracts -p ti-core -p ti-store -p ti-ingest -p ti-sql -p ti-sync -p ti-bench -p ti-geo --all-targets -- -D warnings

echo "=== 3. Running tests (pass 1) ==="
cargo test --locked --features ti --no-fail-fast

echo "=== 4. Running tests (pass 2) ==="
cargo test --locked --features ti --no-fail-fast

echo "=== 5. Running benchmark Python unit tests ==="
python3 -m unittest discover -s bench/luxir -p "test_*.py"

echo "=== search/meta gates complete ==="

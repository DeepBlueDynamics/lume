#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export TMPDIR=/tmp
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
mkdir -p "$CARGO_TARGET_TMPDIR" /bench/bin
trap 'cargo clean' EXIT
rustc --version
rustc --version | awk '{ if ($2 !~ /^1\.96\./) exit 1 }'
cargo build --release --locked --features ti --bin lume
install -m 755 "$CARGO_TARGET_DIR/release/lume" /bench/bin/lume-h2-local
sha256sum /bench/bin/lume-h2-local
du -sh "$CARGO_TARGET_DIR"

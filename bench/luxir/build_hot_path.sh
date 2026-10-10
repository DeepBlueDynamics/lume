#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
# Native Linux scratch is required for the atomic-rename regression.
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
export TMPDIR=/tmp
mkdir -p "$TMPDIR" /bench/bin
rustc --version
rustc --version | awk '{ if ($2 !~ /^1\.96\./) exit 1 }'
rustup component add rustfmt clippy
trap 'cargo clean' EXIT
rustfmt --edition 2021 --check src/bm25.rs src/search.rs
cargo clippy --locked -p lume --all-targets -- -D warnings
cargo test --locked -p lume --lib
cargo test --locked -p lume --lib
cargo clean
cargo build --release --locked --features ti --bin lume
install -m 755 "$CARGO_TARGET_DIR/release/lume" "/bench/bin/lume-hot-$1"
sha256sum "/bench/bin/lume-hot-$1"
du -sh "$CARGO_TARGET_DIR"

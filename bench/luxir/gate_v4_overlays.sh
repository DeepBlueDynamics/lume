#!/usr/bin/env bash
set -euo pipefail
rustc --version
case "$(rustc --version)" in "rustc 1.96."*) ;; *) exit 1 ;; esac
rustup component add rustfmt clippy
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export TMPDIR=/tmp
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
mkdir -p "$CARGO_TARGET_TMPDIR"
trap 'cargo clean' EXIT
check_size() {
  local bytes
  bytes=$(du -sb "$CARGO_TARGET_DIR" | cut -f1)
  test "$bytes" -lt 8589934592
}
rustfmt --edition 2021 --config skip_children=true --check src/index_binary.rs src/index_binary/generation.rs src/index_binary/snapshot.rs src/index_binary/overlays.rs tests/index_v4_generation.rs tests/index_v4_overlays.rs
cargo test --locked --test index_v4_overlays --test index_v4_generation --test index_v4_snapshot --test index_v4_core
cargo test --locked --lib
check_size
cargo clippy --locked --all-targets -- -D warnings
check_size
cargo clean
cargo test --locked --features ti --test index_v4_overlays --test index_v4_generation --test index_v4_snapshot --test index_v4_core
cargo test --locked --features ti --lib
check_size
cargo clippy --locked --features ti --all-targets -- -D warnings
check_size

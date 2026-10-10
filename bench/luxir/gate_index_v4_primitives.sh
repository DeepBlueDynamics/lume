#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
case "$(rustc --version)" in
  "rustc 1.96."*) ;;
  *) echo "Stage 1 gate requires rustc 1.96" >&2; exit 1 ;;
esac
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/workspace/lume/.build-cache/target
export TMPDIR=/tmp
export CARGO_TARGET_TMPDIR=/tmp/cargo-tmp
trap 'cargo clean' EXIT
rustfmt --edition 2021 --config skip_children=true --check src/bm25.rs src/index_binary.rs src/index_binary/codec.rs src/index_binary/csr.rs src/index_binary/postings.rs
cargo test --locked --features ti --lib compact_forward_and_postings_preserve_all_score_bits
cargo test --locked --features ti --lib index_binary::
cargo clippy --locked --features ti --lib -- -D warnings
du -sh "$CARGO_TARGET_DIR"

#!/usr/bin/env bash
set -euo pipefail
cd /workspace/lume
case "$(rustc --version)" in
  "rustc 1.96."*) ;;
  *) echo "Requires rustc 1.96" >&2; exit 1 ;;
esac
rustup component add rustfmt clippy
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
rustfmt --edition 2021 --config skip_children=true --check src/index_timing.rs src/index_binary/bm25_codec.rs src/index_binary/generation.rs src/index_binary/snapshot.rs
cargo test --locked --features ti --test index_v4_core --test index_v4_generation --test index_v4_snapshot --test index_v4_cli
cargo clippy --locked --features ti --all-targets -- -D warnings
# Delete the debug target before the release build to keep below the lane budget.
cargo clean
cargo build --release --locked --features ti --bin lume
install -m 755 "$CARGO_TARGET_DIR/release/lume" /bench/bin/lume-index-v4-profile
sha256sum /bench/bin/lume-index-v4-profile
python3 bench/luxir/profile_v4_open.py --binary /bench/bin/lume-index-v4-profile --output /bench/index-v4-profile
du -sh "$CARGO_TARGET_DIR"

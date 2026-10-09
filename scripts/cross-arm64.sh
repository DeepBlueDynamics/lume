#!/bin/bash
# Cross-compile for aarch64-unknown-linux-gnu inside rust:bookworm.
#   scripts/cross-arm64.sh                  -> --bin lume (unchanged)
#   scripts/cross-arm64.sh ti-query-bench   -> --bin ti-query-bench only
set -euo pipefail
BIN="${1:-lume}"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross > /dev/null
rustup target add aarch64-unknown-linux-gnu > /dev/null
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
export CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++
export AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
export CARGO_PROFILE_RELEASE_LTO=thin
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=/target
cd /src
start=$(date +%s)
case "$BIN" in
  lume)
    cargo build --release --locked --features ti --bin lume --target aarch64-unknown-linux-gnu
    cp /target/aarch64-unknown-linux-gnu/release/lume /out/lume-aarch64
    OUT=/out/lume-aarch64
    ;;
  ti-query-bench)
    cargo build --release --locked -p ti-query-bench --bin ti-query-bench --target aarch64-unknown-linux-gnu
    cp /target/aarch64-unknown-linux-gnu/release/ti-query-bench /out/ti-query-bench-aarch64
    OUT=/out/ti-query-bench-aarch64
    ;;
  *)
    echo "usage: cross-arm64.sh [lume|ti-query-bench]" >&2
    exit 2
    ;;
esac
echo "elapsed_s=$(( $(date +%s) - start ))"
aarch64-linux-gnu-objdump -T "$OUT" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1
ls -la "$OUT"

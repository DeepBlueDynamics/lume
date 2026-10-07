#!/bin/bash
# Cross-compile lume (--features ti) for aarch64-unknown-linux-gnu inside rust:bookworm.
set -euo pipefail
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
cargo build --release --locked --features ti --bin lume --target aarch64-unknown-linux-gnu
echo "elapsed_s=$(( $(date +%s) - start ))"
cp /target/aarch64-unknown-linux-gnu/release/lume /out/lume-aarch64
aarch64-linux-gnu-objdump -T /out/lume-aarch64 | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1
ls -la /out/lume-aarch64

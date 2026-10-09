#!/bin/bash
# A2 store recipe: 1 vessel x 90 days, seed 42.
# Raw parquet is about 0.35 GB, inferred from the 1.8 GB correctness set
# (5 vessels x 92 days), so the store fits in about 1 GB on the Pi.
# This does not cross-build. The lead runs scripts/cross-arm64.sh ti-query-bench.
set -euo pipefail

usage() {
  cat <<'EOF'
Store (host). ROOT/parquet is the gen output. ROOT/store is what the harness opens.
  CARGO_INCREMENTAL=0 scripts/pi-bench-recipe.sh store ROOT

That runs:
  ti-bench gen --root ROOT/parquet --seed 42 --vessels 1 --days 90
  TI_OPT_IN=last backfill_store ROOT/parquet/tier=raw ROOT/store
  import_docs ROOT/parquet/docs ROOT/store

Cross-build (host, inside rust:bookworm, /src = this repo, /out mounted):
  scripts/cross-arm64.sh ti-query-bench
  # /out/ti-query-bench-aarch64

Harness (Pi). Writes bench/results/<UTC date>-pi-<sha>.json
with warm p95 for all 26 queries, grouped by class (class p95 = slowest query).
  SHA=$(git rev-parse --short=7 HEAD)
  ./ti-query-bench-aarch64 bench \
    --store ROOT/store \
    --parquet ROOT/store \
    --corpus tests/golden/corpus.json \
    --out-dir bench/results \
    --iterations 7 \
    --cache-bytes 268435456 \
    --sha "$SHA" \
    --pi
EOF
}

if [[ "${1:-}" != "store" || -z "${2:-}" ]]; then
  usage
  if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || -z "${1:-}" ]]; then
    exit 0
  fi
  exit 2
fi

ROOT=$2
export CARGO_INCREMENTAL=0
cargo run --locked --release -p ti-bench -- gen \
  --root "$ROOT/parquet" --seed 42 --vessels 1 --days 90
# The committed query set reads @last (Q1, Q8); the CI bench gate opts in the same way.
TI_OPT_IN=last cargo run --locked --release -p ti-ingest --example backfill_store -- \
  "$ROOT/parquet/tier=raw" "$ROOT/store"
cargo run --locked --release -p ti-ingest --example import_docs -- \
  "$ROOT/parquet/docs" "$ROOT/store"

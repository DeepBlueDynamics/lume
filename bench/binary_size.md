# D45 binary size measurements

Local D45 measurement is complete. The root release defaults are unchanged.
Recommended shipped profile: fat/1, unwind, opt-level 3, strip=symbols;
Pi candidate: thin/1 with the same settings and one build job.
bench/binary-size-winner.patch is the reviewable shipped-profile Cargo diff;
check with git apply --check --ignore-space-change before the host corpus gate. Panic-abort is measured but rejected
for the server because it defeats request/task containment; do not deploy that
measurement artifact. See plan/decisions/D45-binary-size.md
for the measurement table, query p50 comparisons and correctness status.

## Run

All output paths below are shared artifacts, never committed. Run from the lane.
Builds are sequential; the driver cleans target before and after each variant,
checks the 8 GB ceiling every 30 seconds, and saves stripped lume and ti-bench.
It builds the root-only production binary first and preserves it before building
the separate ti-bench runner with the same profile settings.

```sh
python3 bench/binary_size.py features --output /workspace/lume/.lanes/data/binary-size-cebf5ea
python3 bench/binary_size_build.py --variant thin --output /workspace/lume/.lanes/data/binary-size-cebf5ea
python3 bench/binary_size_build.py --variant fat --output /workspace/lume/.lanes/data/binary-size-cebf5ea
python3 bench/binary_size_build.py --variant abort --output /workspace/lume/.lanes/data/binary-size-cebf5ea
python3 bench/binary_size_build.py --variant small --output /workspace/lume/.lanes/data/binary-size-cebf5ea
python3 bench/binary_size_build.py --variant thin-cgu16 --output /workspace/lume/.lanes/data/binary-size-cebf5ea
```

The small variant overrides pgwire, tokio, chrono, zip, lopdf, quick-xml and ureq
through Cargo --config flags. Root stays at optimization 3 because it owns BM25.
Every other crate keeps optimization 3. The current shipped baseline is fat/1,
not thin/16; the last variant isolates the codegen-units comparison.

Only after build activity has stopped, time every variant:

```sh
python3 bench/binary_size.py benchmark \
  --binary /workspace/lume/.lanes/data/binary-size-cebf5ea/thin/ti-bench \
  --store /workspace/lume/.lanes/data/store-full \
  --output /workspace/lume/.lanes/data/binary-size-cebf5ea --label thin --iterations 31
```

Repeat for fat, abort, small and thin-cgu16, changing --binary and --label.
After each candidate, run the preserved fat ti-bench through shard_cache.py
with --cache-mode on, --cache-bytes 268435456 and --iterations 31, placing
--out under <candidate>/timing-fat-control. The comparison uses this adjacent
control's timings and checks its fingerprints against the initial fat baseline.
Both binaries run with no compiler active; retain every run.
The 20-query corpus is fixed in QUERY_IDS and its TI SQL is copied unchanged
from tests/golden/corpus.json. The existing TiEngine benchmark has no root
LumeText factory, so the selected queries are non-text. Full text/count
correctness remains the integrator's 61/0/1 gate over the enabled boat store.

```sh
python3 bench/binary_size.py compare \
  --output /workspace/lume/.lanes/data/binary-size-cebf5ea --baseline fat
```

Repeat comparison with --baseline fat when choosing a change to the shipped
profile. Row-count or answer-fingerprint changes fail comparison. Every timing uses
shard_cache.py --cache-mode on --cache-bytes 268435456; no cache-off run. Reports retain all 20 per-query
p50s, each relative change, maximum per-query regression and sum-of-p50 change.
The clarified D45 gate uses aggregate sum change ≤5%, and class medians with
an absolute 1 ms floor. Classes use the median of constituent query p50s.
Individual queries exceeding both 5% and 1 ms are flagged separately; they do
not automatically reject a profile. Raw per-query strict checks are retained
for transparency alongside passes_clarified_gate.

Artifacts include per-variant lume-build.log and ti-bench-build.log, build.json (flags and target peak),
size.json (toolchain, binary/gzip sizes, SHA), deterministic gzip, timing.log,
timing-command.json and the existing ti-bench timing JSON/Markdown.
The driver never edits Cargo.toml, installs dependencies, executes DuckDB or
starts servers. Gzip is streamed at level 9 with mtime=0 and no filename.

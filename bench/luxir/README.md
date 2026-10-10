# Lume vs Luxir benchmark

The shared input is BEIR's test split of SciFact and TREC-COVID, downloaded from
the [BEIR dataset host](https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/).
No retrieval parameters are tuned against relevance judgments.

From the lane root:

```sh
python3 bench/luxir/prepare.py --root /workspace/lume/.lanes/data/luxir-bench
TMPDIR=/workspace/lume/.lanes/w4/.test-tmp python3 -m unittest discover -s bench/luxir -p 'test_*.py'
python3 bench/luxir/score.py --root /workspace/lume/.lanes/data/luxir-bench
```

On the desktop, replace the root with the host's corresponding shared path.
Preparation is stdlib-only and reads individual ZIP members without extracting
archive paths. Each dataset has docs.jsonl (`id,text`), queries.tsv
(`qid<TAB>text`) and qrels.tsv (`qid<TAB>docid<TAB>grade`), without headers.
`prepare.py --with-meta` additionally creates docs_meta.jsonl with the same
id/text plus synthetic year (1990–2024), one of eight categories, n_cites
(0–1000), a UTC published_at within that year, and 1–3 unique tag_00–tag_19.
Seed 42 and the candidate lists are recorded in metadata.json; these fields
are generated capability fixtures, not factual paper metadata.
Only query IDs in test qrels are retained. Titles precede bodies with two
newlines. Archive SHA-256, URL and counts are recorded in prepare.json;
recorded hashes establish provenance, not independent publisher verification.

Put document-level runs in the shared runs directory:

- `<engine>-<mode>-<dataset>.trec`: `qid Q0 docid rank score tag`, up to
  100 unique documents per query, ranks starting at 1, descending finite scores.
  Collapse Lume sections by maximum document score before ranking documents.
- Matching `.lat.jsonl`: exactly one `{"qid": "...", "ms": ...}` per test
  query, the median of three timed passes after one complete warmup pass.
- `<engine>-<dataset>.build.json`: indexing wall time, index bytes, peak indexing
  RSS and idle loaded-index RSS. Use explicit units in field names.
- Optional matching `.throughput.json`: qps plus measured seconds,
  concurrency, successful requests and errors for 8 clients over 60 seconds.
  Use the shared `http_runner.py` transport: one persistent HTTP connection
  per worker, TCP_NODELAY, eight workers followed by sixteen. Record driver
  CPU time, errors and the 16/8 QPS gain; a gain indicates client saturation
  still needs investigation. Both engines and their drivers must use one
  user-defined Docker network `luxir-bench-net`, addressed by container names
  (`lume-engine:5863`, `luxir-bench:9400`). Lume's bearer check stays enabled
  for both released and resident variants.
  A missing throughput result is shown as “not run,” not zero.

The scorer macro-averages over all test qids, including zero scores for queries
without retrieved documents. nDCG@10 uses linear relevance gain and log2
discount. Recall@100 and MRR@10 treat positive judgments as relevant.
R_cap@100 divides relevant hits by min(100, relevant judgments), alongside
uncapped Recall@100. Unjudged documents receive zero gain. Latency percentiles use linear
interpolation over the per-query median latencies. Duplicate documents,
inconsistent ranks and incomplete latency coverage are rejected.

Results are written to bench/results/2026-10-09-luxir-vs-lume.json; that file
is only a benchmark result once actual engine runs exist. Build metadata and
per-query quality metrics are preserved in it. A matching `.invalid.json`
marker excludes an interrupted or contaminated run from scoring and records
its reason. The runner clears that marker only after a successful complete
rerun, including throughput. Use `--modes default` to rerun graph mode without
replacing the separately completed BM25 row.

## Fair execution and current Lume limitation

Use Linux Docker containers on the desktop with `--cpus 8 --memory 8g`.
Serve the Lume index from the named Linux Docker volume `lume-luxir-indexes`
(`/indexes/scifact`), never from the Windows bind mount. The SciFact copy was
SHA-256 checked against all five original files (57,804,471 bytes). Inputs
and output runs may stay on the bind mount. The initial bind-mount run was
stopped before completion and is excluded from results. Parrot confirmed Luxir's indexes live in the Linux named volume
`luxir-bench-data` at `/var/lib/luxir_data`; its Windows input mount is
read-only. Both latency harnesses measure client wall-clock HTTP requests,
including serialization, transport, search and JSON decoding, after one
full warmup and with three timed passes (per-query median).
Only one engine may build or time on the desktop at once. Exchange START
and DONE mail with the other benchmark agent and wait for its DONE before
starting; Rust builds must also stay outside timed runs.
Publish ports on 127.0.0.1 only. Pin and checksum the released v0.12.2 Lume
Linux x64 binary; do not substitute a compiled lane binary. Run SciFact end
to end before TREC-COVID. Phase 1 is lexical: Lume alpha=0, graph=0 and
graph=0.4; record all deviations from defaults. Hybrid alpha=0.5 with Shivvr
is phase 2, after phase 1.

Source inspection at 7183552 (also confirmed by the lead at release tag v0.12.2, 5d88268) found that MCP `lume_search` calls
`LoadedIndex::open` per request (src/agent.rs), which deserializes the index
JSON files every time (src/search.rs). A long-running process therefore
does not provide a resident-index comparison. The approved comparison has two separately labeled Lume rows: released
v0.12.2 including per-request loads, and a resident-index fix with identical
rankings. Warm released means a warmed process/filesystem, not a retained index.
Released Lume closes the connection after each response; the client reconnects
per request, included in latency. The shared client uses an explicit
close-after-response setting for this verified server behavior, so it does
not time a doomed reuse attempt. Unexpected drops are errors.
The fixed-source base is 5e71ed8; release tag v0.12.2 is 5d88268.
Plain .txt indexing splits files into consecutive 25-line sections titled
`Lines <start>-<end>` and preserves the source filename (src/main.rs). The
benchmark must map `<BEIR id>.txt` back to that document ID and take its maximum
section score. No engine comparison results have been produced by this harness yet.

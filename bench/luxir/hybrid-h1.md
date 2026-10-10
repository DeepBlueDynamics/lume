# Existing Lume hybrid: SciFact

Quality over all 300 SciFact test queries, using the resident MCP path, stemming and coordination floor 1, graph off, GTR-T5 768-d through Shivvr. Binary is the Step 3 release built from `58ec10e` (rustc 1.96.1, thin LTO/CGU1). Engine: 8 CPUs/8 GiB; driver outside those limits.

| Blend | Alpha | nDCG@10 | R_cap@100 | MRR@10 |
|---|---:|---:|---:|---:|
| Multiplicative | 0.3 | 0.6934 | 0.9076 | 0.6580 |
| Multiplicative | 0.5 | 0.6997 | 0.9153 | 0.6627 |
| Multiplicative | 0.7 | 0.7038 | 0.9187 | 0.6655 |
| Normalized | 0.3 | 0.6976 | 0.9287 | 0.6624 |
| Normalized | 0.5 | 0.7057 | 0.9337 | 0.6706 |
| Normalized | 0.7 | 0.7097 | 0.9353 | 0.6740 |

Luxir's prior GTR-T5 + RRF k=60 reference, reported by the lead, has nDCG@10 0.6943, R_cap@100 0.9563 and MRR@10 0.6637. Lume's best tested normalized row has higher nDCG/MRR and lower recall. This compares existing engine configurations using the same model, **not identical vectors**: Lume's 5,183 sections produced 8,417 remote chunks, and fusion/depth differ. H3 must use the shared document/query vector cache for strict vector-input parity. Do not infer a general search-quality win from one corpus or from tuning alpha on this test set.

The fresh semantic index took **1,541.88 seconds**, including embedding. Build exit 0, semantic session present, 8,417 chunks created, no logged build errors. Semantic depth is hard-coded at 60. The session is reused; no sweep re-embedding.

The first alpha=0.3 pass observed 300 Shivvr completions in each blend sweep after the lead cleared the semantic cache. Later alpha sweeps reused semantic query results (zero calls). Multiplicative cached passes had zero calls. Normalized alpha=0.3 cached pass observed one call for q770; later normalized passes had zero calls. One normalized first-pass upstream 502 was recorded. Completion-based proxy attribution places its 65-second event alongside a 164-ms q903 request, so per-query attribution is unreliable for that event; the source only retries session-expired 404, and these logs do not prove automatic recovery from 502.

First/cached aggregate quality metrics match. Raw outputs are byte-identical only for multiplicative alpha=0.7 and normalized alpha=0.5. Other differences include equal-score document swaps at rank 100. Normalized alpha=0.3 also changes all q770 score magnitudes between passes, from a lexical-scale score to normalized hybrid scores, although aggregate quality remains equal. The report preserves this rather than claiming complete ranking parity.

Initial sweep attempts traversed the Windows-mounted corpus on every request and were stopped. The lead copied the 5,183 files, preserving mtimes, into Linux volume `lume-scifact-files`, readonly at `/bench/scifact/files`; fingerprint stayed `(7783564, 1791574938)`, so no re-ingestion. Lead probes reported uncached 382 ms/cached 46 ms after the move; these are diagnostics, not a matched timing benchmark. H1 is quality-only.

Known issues for H2: hybrid ignores explicit `shivvr_url` (H1 uses `SHIVVR_BASE_URL`); walks/stats the entire corpus on every search; fixes depth at 60; has no RRF mode; tie behavior at the retrieval cutoff is nondeterministic. The alpha trend warrants testing 0.8–1.0, but needs an independent validation set before changing defaults.

Full metrics and caveats: `hybrid-h1.json`. Raw runs: `.lanes/data/luxir-bench/runs/lume-h1-{mult,norm}-a*-{first,cached}-hybrid-scifact.*`; logs: `hot-path/h1-*.log`.

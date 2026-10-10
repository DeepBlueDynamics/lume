# Opt-in resident local vectors and deterministic hybrid fusion

Hybrid searches fingerprint the loaded index snapshot instead of walking the source directory on each request. Explicit Shivvr URLs now reach embedding, session, query and inversion calls; endpoint-specific caches cannot silently reuse another server's results.

This change adds optional resident section vectors using exact cosine scoring. Indexing can import shared document and query caches without service calls, or embed missing sections through /embed with explicit document/query tasks. Imports validate model, dimensions, IDs, coverage and finite nonzero vectors. Requests reject more than 256 texts or texts over 32 KiB. Resident indexes refresh when the vector file changes.

Three fusion modes are opt-in through LUME_BLEND: rrf, normalized-v2, and vector. RRF uses deterministic ranks with LUME_RRF_K (default 60); normalized-v2 min-max scales lexical and cosine scores; vector uses cosine alone. LUME_LOCAL_VECTOR_DEPTH accepts a count or all (default 100). Existing blend behavior and defaults remain unchanged. These experimental modes fuse lexical/vector lists; graph scores are not included.

Validation at cbd9012, reported by the host runner: fmt, strict whole-package/eight-crate clippy, locked TI tests, and Python suites passed. Tests cover zero-service imports/searches, malformed caches, task metadata, input bounds, exact cosine, deterministic RRF/ties, normalization, and legacy score formulas. Release measurements use rustc 1.96, thin LTO, CGU 1.

On SciFact using the same EmbeddingGemma2 document/query vectors, normalized-v2 alpha 2.0/depth 100 scored nDCG@10 0.8533, Recall@100 0.9800, MRR@10 0.8291; Luxir scored 0.7794/0.9867/0.7523. All 16 fusion runs made zero upstream calls. Alpha was tuned on SciFact; these are quality-only results, not latency claims. The fixed-alpha NFCorpus held-out check is pending. No new defaults are proposed by this PR.

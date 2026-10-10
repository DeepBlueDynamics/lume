# Local-vector hybrid search

Lume can keep document vectors in its search index and combine exact cosine scores with BM25. This is opt-in; existing search defaults stay unchanged. The implementation is in PR [#14](https://github.com/DeepBlueDynamics/lume/pull/14), stacked on #12; use a build containing it.

## Embed your documents

Use a Shivvr service that supports /embed and your chosen model:

```bash
lume index ./documents --db ./library-index \
  --embed-model embeddinggemma-2 --embed-dimensions 768 \
  --shivvr-url http://127.0.0.1:8085

LUME_BLEND=normalized-v2 LUME_LOCAL_VECTOR_DEPTH=100 \
  lume search --db ./library-index --alpha 2 --graph 0 \
  --shivvr-url http://127.0.0.1:8085 "bilge pump maintenance"
```

Embedding uses explicit document and query tasks. Uncached queries need the service; query vectors are cached in memory. Document vectors persist in local-vectors.json and load with the resident index. Index updates reuse unchanged section vectors.

Requests are limited to 256 texts and 32 KiB per text. Oversize sections are rejected, not truncated. Model, dimension and vector checks reject incompatible inputs. Use NUTS_SERVICES_TOKEN if the service requires bearer authentication.

## Import existing vectors

For a corpus with one section per file, use document IDs matching filename stems: doc123 maps to doc123.txt. JSONL rows have this shape:

```json
{"id":"doc123","vector":[0.1,0.2]}
```

Use filenames <model>-<dimensions>-<dataset>-docs.jsonl and the corresponding -queries.jsonl. The vectors must have the declared dimensions and use the same model/task convention. Optional query imports also need a headerless TSV of query ID, tab, exact query text.

```bash
lume index ./scifact-files --db ./scifact-index \
  --embed-model embeddinggemma-2 --embed-dimensions 768 \
  --embed-docs ./embeddinggemma-2-768-scifact-docs.jsonl \
  --embed-queries ./embeddinggemma-2-768-scifact-queries.jsonl \
  --embed-query-texts ./queries.tsv
```

Complete imports make no embedding calls. Imported query vectors are saved in the index and reused for matching query text. Other queries still need /embed. Document IDs must cover every section; multiple sections sharing one filename stem are rejected. There is a 1,024-entry query-vector cache limit.

## Choose a blend

Set these variables in the search or server process:

| Setting | Use |
|---|---|
| LUME_BLEND=normalized-v2 | Min-max scales BM25 and cosine, then adds lexical + alpha × cosine. Try when dense retrieval is strong. |
| LUME_BLEND=rrf | Adds reciprocal ranks from both lists; less sensitive to score scales. LUME_RRF_K defaults to 60. |
| LUME_BLEND=vector | Cosine only; useful for checking the dense baseline. |
| No LUME_BLEND | Existing blend behavior. LUME_BLEND_NORM=1 selects the older BM25/max + alpha × raw cosine formula. |

Use a positive --alpha for hybrid search; --alpha 0 selects BM25. For the new modes, use --graph 0: they combine lexical/vector lists, without graph fusion. Ties use section order.

LUME_LOCAL_VECTOR_DEPTH defaults to 100. Set a larger positive count or all to admit more cosine candidates. More candidates did not consistently improve quality below; measure on your own corpus.

The service URL precedence is explicit shivvr_url/--shivvr-url, then SHIVVR_BASE_URL, then http://localhost:8085. Session/result caches distinguish service URLs.

## Quality checks

Same EmbeddingGemma2 768-dimensional document/query vectors; exact cosine; graph off; depth 100. All runs made zero upstream calls. These are quality-only checks, with no speed claims.

SciFact, 300 queries:

| Method | nDCG@10 | Recall@100 | MRR@10 |
|---|---:|---:|---:|
| Lume normalized-v2, alpha 2 | 0.8533 | 0.9800 | 0.8291 |
| Lume vector only | 0.8451 | 0.9733 | 0.8218 |
| Lume RRF, k=20 | 0.7781 | 0.9833 | 0.7419 |
| Luxir hybrid | 0.7794 | 0.9867 | 0.7523 |

Alpha was tuned on SciFact. At alpha 2, depth all reduced normalized-v2 nDCG to 0.8119.

NFCorpus, 323 held-out queries, alpha fixed at 2 before running:

| Method | nDCG@10 | Recall@100 | MRR@10 |
|---|---:|---:|---:|
| Lume normalized-v2, alpha 2 | 0.3730 | 0.3434 | 0.5763 |
| Lume RRF, k=20 | 0.3622 | 0.3458 | 0.5706 |
| Lume vector only | 0.3533 | 0.3466 | 0.5549 |
| Lume BM25 | 0.3143 | 0.2379 | 0.5275 |
| Luxir hybrid | 0.3700 | 0.3435 | 0.5768 |

NFCorpus results are comparable, rather than evidence of a general Lume advantage. No significance test was run. Recall is ordinary Recall@100, not capped recall. Lume artifacts: [SciFact](../bench/luxir/hybrid-h3-fusion.json), [NFCorpus](../bench/luxir/hybrid-h3-nfcorpus.json). Luxir reference numbers were supplied by the lead's matched-cache runs. Build: rustc 1.96, thin LTO, one codegen unit.

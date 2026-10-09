# Lexical Pipeline & Relevance Audit: Luxir vs. Lume

This audit compares the lexical ranking pipelines of **Luxir (v0.1.0)** and **Lume (v0.12.2)** at the source code level to explain the **+0.0346 nDCG@10 gap** observed on BEIR's SciFact benchmark (Luxir BM25: **0.6793** vs. Lume Released BM25: **0.6447**).

Both engines evaluated identical test queries and document texts (`title + "\n\n" + text`). Luxir is licensed under Apache-2.0. This audit isolates mathematical, structural, and algorithmic differences to provide actionable ablations for Lume.

---

## 1. Step-by-Step Pipeline Comparison

### 1.1 Tokenizer
- **Luxir** (`src/luxir/analysis/Analyzer.cpp`, `deps/uni-algo`):
  - Uses `unicode_word`, implementing **Unicode Standard Annex #29 (UAX#29)** word boundaries.
  - Correctly segments multi-lingual text, preserves numbers joined with letters, handles apostrophes within words without splitting, and treats punctuation according to Unicode word break properties.
- **Lume** (`src/lib.rs#L642-L669`):
  ```rust
  // src/lib.rs:646-663
  for fc in folded {
      if fc.ch.is_ascii_alphanumeric() {
          // append to current token
      } else if let Some(t) = cur.take() {
          tokens.push(t);
      }
  }
  ```
  - Splits strictly on any non-ASCII alphanumeric byte (`!fc.ch.is_ascii_alphanumeric()`).
  - **SciFact Impact**: Biomedical literature is saturated with chemical and genetic names (e.g., `IL-6`, `p53`, `SARS-CoV-2`, `1,25-dihydroxyvitamin`). Lume fragments `SARS-CoV-2` into three separate tokens `["sars", "cov", "2"]`, losing compound identity and inflating term frequencies for generic single digits.

---

### 1.2 Normalization & Case Folding
- **Luxir** (`docs/guide/schema.md#L303`, `src/luxir/analysis/Analyzer.h`):
  - Uses `nfkc_cf` (Unicode Normalization Form KC with Case Folding, calculated to a fixpoint), followed by `fold` (accent and diacritic folding).
  - Ensures composed and decomposed Unicode representations, ligatures, and Greek/scientific symbols compare identical.
- **Lume** (`src/lib.rs#L595-L640`):
  - Applies `fold_text` with `fold_latin`, a custom lookup table mapping accented Latin characters to basic ASCII (e.g., `é` -> `e`), followed by ASCII lowercasing.
  - Lacks NFKC normalization; non-Latin Unicode characters or decomposed sequences can fail to match.

---

### 1.3 Stemming & Stopwords
- **Luxir** (`src/luxir/analysis/KStemmer.cpp#L1-L1140`, `KStemData.h`, `KStemmer.h`):
  - Implements **Bob Krovetz's KStem algorithm** using a 35,000-word embedded lexicon (`KStemData.h`).
  - KStem is conservative: it conflates plurals, past tense, and common suffixes while preserving distinct dictionary words (e.g. `biomaterials` -> `biomaterial`, `inhibits` -> `inhibit`, `cells` -> `cell`).
  - Does not discard stopwords by default in full-text schema `_t`.
- **Lume** (`src/bm25.rs#L6-L110`):
  - **No stemmer exists in Lume**. Search across the codebase reveals zero stemming logic (neither Porter, Snowball, nor KStem).
  - Queries are filtered by `filter_query_stopwords`, removing 174 hardcoded English stopwords.
  - **SciFact Impact**: In scientific claims, query terms are frequently inflected differently than the abstract text. For example, if a query mentions `"biomaterials show enhanced uptake"` and the document abstract states `"the biomaterial shows enhanced uptake"`, Lume fails to match `biomaterials` to `biomaterial`. This is the single largest structural factor limiting Lume's recall.

---

### 1.4 BM25 Formula & Length Normalization
- **Luxir** (`src/luxir/search/Similarity.h#L98-L117, L275-L285`, `Similarity.cpp#L33-L48`):
  - Modern Lucene-style BM25:
    $$\text{IDF} = \ln\left(1 + \frac{N - n + 0.5}{n + 0.5}\right)$$
    where $N = \text{docsWithField}$ and $n = \text{docFreq}$.
  - Parameters: $k_1 = 1.2$, $b = 0.75$. Standard Lucene BM25 without a delta term.
  - Quantized 1-byte document length norms (`SmallFloat::byteToLength`) with a 256-entry lookup table.
- **Lume** (`src/bm25.rs#L672-L678, L786-L820`):
  - Formula:
    $$\text{IDF} = \max\left(0.0, \, \ln\left(\frac{N - \text{df} + 0.5}{\text{df} + 0.5} + 1.0\right)\right)$$
  - Parameters: $k_1 = 1.2$, $b = 0.75$, $\delta = 1.0$.
  - Exact unquantized document length norms (`doc_len as f64 / avgdl`).
  - Note: Lume's BM25 includes $\delta = 1.0$ (the BM25+ offset in `calculate_bm25_term_score`), providing a lower-bound score contribution for term matches regardless of document length, whereas Luxir uses standard Lucene BM25 without a delta term.

---

### 1.5 Field Weighting & The "Introduction" Title Bug
- **Luxir**:
  - Indexes `title + "\n\n" + text` into a single unified `text` field, scoring uniformly across the entire content.
- **Lume** (`src/bm25.rs#L139-L141, L650-L688`):
  ```rust
  // src/bm25.rs:139-141
  title_weight: 2.0,
  body_weight: 1.0,
  ```
  ```rust
  // src/bm25.rs:687
  total_score += params.title_weight * title_score + params.body_weight * body_score;
  ```
  - **The Defect in Ingest Parsing** (`src/bm25.rs#L233-L278`):
    ```rust
    // src/bm25.rs:235
    let mut current_title = String::from("Introduction");
    ```
    `parse_markdown` only extracts titles from lines beginning with Markdown headers (`#`).
    In BEIR benchmarks, documents are staged as raw plain text without `#` headers.
    Consequently:
    1. Every single document section receives the literal title `"Introduction"`.
    2. The actual document title is relegated to the body.
    3. `title_score` evaluates to `0.0` for all benchmark queries.
    4. Because `title_weight` is `2.0` and `body_weight` is `1.0`, title terms appearing in the document text only receive body weight (1.0) instead of the intended 2.0 weight, under-rewarding matches in document titles relative to Lume's intended design.

---

### 1.6 Coordination Factor
- **Luxir**:
  - Scores are the sum of BM25 contributions from matched terms. No arbitrary coordination multiplier is applied.
- **Lume** (`src/bm25.rs#L690-L697`):
  ```rust
  let coverage = matched_terms.len() as f64 / num_distinct as f64;
  let coord = COORD_FLOOR + (1.0 - COORD_FLOOR) * coverage;
  total_score *= coord;
  ```
  - Down-weights documents that match only a subset of query terms.
  - In SciFact, test queries are long scientific hypothesis statements (averaging 12–18 terms). If a document matches 8 highly specific keywords but lacks 2 incidental words, the coordination factor heavily penalizes its total score.

---

### 1.7 Query Parsing & Execution
- **Luxir**:
  - `POST /collections/<name>/_search` with `{"match": {"text": "<query>"}}` parses queries as an OR-union of analyzed terms with per-term BM25 scoring. Duplicate terms are handled gracefully.
- **Lume**:
  - Gathers candidate document IDs via `MiniRoaring` union of term posting lists (`src/bm25.rs#L512-L523`), then computes scoring loops.
  - When enabled, SKG graph boosts (`src/graph_search.rs`) multiply BM25 scores by entity co-occurrence weights.

---

## 2. Ranked Explanation of the +0.0346 Gap

| Rank | Mechanism | Location in Lume | Root Cause & Mechanism | Rough guess, unmeasured |
|:---:|---|---|---|:---:|
| **1** | **Absence of Stemming** | `src/bm25.rs` (missing) | Lume has no stemmer. Queries with inflected terms (`biomaterials`, `regulated`, `mutations`) fail to match singular/stemmed forms in abstracts. Luxir uses KStem. | **+0.018 to +0.022** |
| **2** | **"Introduction" Title Weighting Mismatch** | `src/bm25.rs#L235`, `src/bm25.rs#L139` | Unmarked BEIR titles default to `"Introduction"`. `title_weight: 2.0` is wasted; title terms in the body only receive `1.0` weight. | **+0.008 to +0.012** |
| **3** | **Coordination Factor on Long Claims** | `src/bm25.rs#L695-L697` | Multi-term scientific claims are penalized if they match a subset of query terms, suppressing relevant passages that omit peripheral words. | **+0.005 to +0.008** |
| **4** | **Naive ASCII Tokenizer on Chemical Names** | `src/lib.rs#L647` | Splitting on all non-alphanumeric chars fragments hyphenated genes, drugs, and chemicals (`SARS-CoV-2` -> `["sars", "cov", "2"]`). | **+0.003 to +0.005** |

---

## 3. Recommended Cheap Ablations for Lume

These four ablations can be implemented independently by Tarantula and verified against `score.py` on both SciFact and TREC-COVID:

### Ablation 1: Fallback First-Line Title Extraction
- **Change**: In `src/bm25.rs#parse_markdown`, if no `#` header exists, extract the first non-empty line as `current_title` and the remainder as `body`. Alternatively, set `params.title_weight = 1.0` and `params.body_weight = 1.0` for plain-text ingests.
- **Cost**: 4 lines of code in `parse_markdown`.

### Ablation 2: Add Porter or KStem Stemming
- **Change**: Integrate a Rust stemming crate (such as `rust-stemmers` or a port of KStem) into `lume::tokenize` prior to term indexing and query lookup.
- **Cost**: Add dependency, call stemmer on token bytes.

### Ablation 3: Disable Coordination Factor for Queries > 4 Terms
- **Change**: In `src/bm25.rs#L696`, set `let coord = 1.0;` when `num_distinct > 4`, or introduce an environment flag `LUME_NO_COORD=1`.
- **Cost**: 1 line of code.

### Ablation 4: Hyphen-Preserving Tokenizer
- **Change**: In `src/lib.rs#tokenize`, treat internal hyphens (`-`) flanked by alphanumeric characters as token-internal characters rather than delimiters.
- **Cost**: 5 lines of code.

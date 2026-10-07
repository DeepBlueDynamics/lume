# M5 item 1: live MCP column resolution

The milestone requires tools in lume serve and correct columns in the top three
for at least 90 of 100 phrases. This evaluation calls tools/list and ti_resolve
through the real loopback HTTP MCP endpoint over the boat store, not SQL templates
or a direct resolver shortcut.

## Frozen evaluation

tests/golden/resolve_phrases.json contains 100 sailor/agent phrases, with 70
development and 30 holdout entries. It was committed as 0ebddfe before reading the
resolver implementation or running the baseline. Expectations use paths present
in store-full; units, colloquial language, abbreviations, single-edit typos and
vessel-qualified requests are represented. The phrase about wind from the bow asks
its strength because this corpus retains apparent wind speed, not wind angle.

Only phrase and limit=3 go over MCP. Hidden expected paths remain evaluator-side.
The default report withholds holdout miss details during development. The first
development evaluation reached 70/70 top-three, so there was no second tuning
round. Resolver implementation 68607a9 was committed before viewing final holdout
results. No fixture or expected value was changed during tuning.

## Observed accuracy

Rust 1.99 debug, current lane binary, read-only store-full. Accuracy does not
depend on bind-mount latency. All evaluations had zero MCP errors.

| Split | Baseline top 1 | Baseline top 3 | Improved top 1 | Improved top 3 |
|---|---:|---:|---:|---:|
| All 100 | 47/100 | 65/100 | 99/100 | 100/100 |
| Development 70 | 34/70 | 47/70 | 69/70 | 70/70 |
| Frozen holdout 30 | 13/30 | 18/30 | 30/30 | 30/30 |

Baseline: unchanged resolver at f17885d. Reports/transcripts are under
.lanes/data/resolve-eval/baseline-f17885d, development-v1 and final-v1.
They are not committed. The evaluator writes checkpointed JSON and a markdown
summary with misses. This is a finite test-set result, not a guarantee for every
sailor phrase or an unrelated catalog.

## General changes

BM25 still indexes actual catalog columns using paths and pinned Signal K
descriptions. vocabulary.json enriches wildcard catalog patterns with general
nautical synonyms/abbreviations and compatible display-unit words. Pinned units
fill missing catalog units; aliases support legacy leaf layouts such as battery
stateOfCharge. Returned units are canonical storage units; no value conversion
is performed.

Unique single-edit corrections, including adjacent transpositions, use the
catalog lexicon. Ambiguous corrections are left untouched. Port/starboard
abbreviations and nautical left/right synonyms normalize before BM25.
Top candidates represent distinct paths rather than spending all three slots
on aggregate variants. Explicit aggregate words still participate in ranking.
Source-provenance columns are downweighted unless requested. A unique vessel
name, MMSI or URN in a phrase qualifies the latest-value lookup; an explicit
vessel argument takes precedence.

plan/spec/03-single-box-budget.md contains no vocabulary table, so the
resolver's vocabulary is bundled beside its existing metadata. No dependency,
sealed-store format or global BM25/search implementation changed.

## Reproduction and checks

Start an existing server on loopback, then:

```sh
python3 bench/resolve_eval.py --mcp-url http://127.0.0.1:5863/mcp \
  --label <source-and-binary-revision> --output .lanes/data/resolve-eval/<fresh-run>
```

The harness starts nothing and refuses a non-loopback endpoint. Use --split
development while tuning; --show-holdout is reserved for the final evaluation.
Exit status is zero only at 90% top-three or better.

The ignored Rust live-store test manages a loopback server and can run explicitly:

```sh
CARGO_INCREMENTAL=0 TI_RESOLVE_STORE=<boat-store> \
TI_RESOLVE_OUTPUT=<fresh-output> TI_RESOLVE_LABEL=<revision> \
TI_RESOLVE_REQUIRE_PASS=1 cargo test --features ti --test ti_resolve -- --ignored --nocapture
```

TI_RESOLVE_SPLIT selects development/holdout/all; TI_RESOLVE_SHOW_HOLDOUT=1
prints final holdout misses. PYTHON can select the host interpreter.

Observed checks: 40 Python bench tests passed; resolver integration tests passed
3/3 with the real-store gate ignored in the ordinary run; the separate live
development and final gates both passed. The older metadata-wide 100-phrase
regression remains 97/100 top-three over 488 catalog fields. Tests also check
aggregate intent, unique paths, typo handling, units, source-independent reserve
bank vocabulary, latest non-null values and vessel qualification. Specific-file
rustfmt checks passed. Full TI suite and strict root clippy were not run locally.

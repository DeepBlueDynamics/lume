# q2-001 count-path investigation

The original TI query returned 14 windows. Exactly three belong to the primary
vessel and match the unchanged oracle rows and wind maxima. The other eleven
belong to background vessels, each with its own matching note. Every vessel
has 74 leak/water notes in the actual enabled store (1,610 documents total).

All vessel IDs below use `vessels.urn:mrn:imo:mmsi:<MMSI>`. Each note's owner
is the vessel in its row; each recorded note covers the first 60 seconds of
that window. The raw query reply is retained outside Git under
`.lanes/data/count-paths-q2/unscoped-reply.json`.

| Vessel MMSI | UTC window start | TI wind max | Unchanged oracle | Own note ID | Body keyword |
|---|---|---:|---:|---|---|
| 367000000 | 2026-04-05T00:00:00Z | 5.715 | 5.715 | notes/1775347200/5542ab098b3b07d4 | leak |
| 367000000 | 2026-04-05T12:00:00Z | 4.998 | 4.998 | notes/1775390400/2c81255719959d4b | water |
| 367000000 | 2026-04-22T12:00:00Z | 12.705 | 12.705 | notes/1776859200/29155be4aa8fddcc | leak |
| 367000001 | 2026-05-15T00:00:00Z | 7.7 | — | notes/1778803200/8de61b12be71b134 | leak |
| 367000001 | 2026-05-20T12:00:00Z | 8.909 | — | notes/1779278400/71a41b21d021a26f | water |
| 367000001 | 2026-05-27T12:00:00Z | 10.386 | — | notes/1779883200/80ca2c14c4672156 | leak |
| 367000002 | 2026-03-04T00:00:00Z | 5.722 | — | notes/1772582400/1e980b4d9d3da04c | water |
| 367000002 | 2026-03-24T00:00:00Z | 10.6 | — | notes/1774310400/9233259ee3f2e17d | water |
| 367000002 | 2026-04-27T12:00:00Z | 14.141 | — | notes/1777291200/d2a08007301db37c | leak |
| 367000003 | 2026-03-11T00:00:00Z | 16.244 | — | notes/1773187200/05231f574c5b8373 | leak |
| 367000003 | 2026-05-05T12:00:00Z | 8.309 | — | notes/1777982400/b582b1aa70c665e0 | water |
| 367000003 | 2026-05-17T12:00:00Z | 5.573 | — | notes/1779019200/87340b1040b89648 | leak |
| 367000004 | 2026-05-05T00:00:00Z | 4.971 | — | notes/1777939200/03dd94853444d4e5 | leak |
| 367000004 | 2026-05-30T12:00:00Z | 6.408 | — | notes/1780142400/3cfa66a5f211060d | water |

The generator uses one source (`can0.115`) and deliberately emits repeated
pump-event rows at the same timestamp. The raw oracle view does not deduplicate
them. The primary results agree without changing counts or aggregate logic.
The mismatch comes from different question scopes: the oracle's notes CTE
restricts context to the primary vessel, while the original TI SQL did not.

The approved change scopes TI to that exact primary URN and clarifies the case
description. Oracle SQL, expected rows and tolerances remain unchanged.

`tests/ti_text_vessels.rs` passed locally: two vessels have telemetry at the
same bucket, only one owns a note, and `match(notes, 'leak OR water')` returns
only that owner. An explicit other-vessel query returns no rows. Both checks
repeat to exercise populated and empty text-cache entries.

Reproduce the full comparison (no DuckDB required):

```bash
python3 tests/golden/count_paths_q2_compare.py \
  --lume-bin /path/to/lume --store /path/to/enabled-store \
  --unscoped --inspect-background --report /path/to/comparison.json
```

`--reply <json>` can reuse a captured TI reply while still validating each
window against its own stored notes and the unchanged primary expectations.
The lead ran count_paths_oracle.py on the host without --prepare or
--skip-duckdb, using the lane corpus and release lume.exe at 7bf038d:
exit 0; all 65 empty-list shard hashes match; verification is 61 passed,
0 failed, 1 excluded (qx-003). Independent DuckDB returned q2-001=3,
q6-006=4 and q1-007=1 rows, matching unchanged stored expectations.
The local two-vessel regression passed (1 test, no failures or skips).

# CRoaring evaluation (M4 item 3)

**Recommendation: retain roaring 0.11.5 in production; reject adoption of CRoaring/native Frozen storage for this milestone (D40).** CRoaring has material CPU wins on sparse BSI and run-optimized inputs, so a separately scoped portable-view backend experiment is justified. This recommendation does not claim that roaring is the faster library.

The exact criterion in [spec/12](../spec/12-milestones.md) is: “`croaring` frozen-view evaluation written up in the decisions log, adopt or reject”. It specifies no numeric speedup threshold. This evaluation and the D40 decision address that item; they do not close the other M4 gates or claim a CI/main milestone is complete.

## Reproducible experiment

[Harness and instructions](../../bench/croaring-eval/README.md), [raw results](../../bench/croaring-eval/results.json). Isolated workspace pins `roaring =0.11.5`, `croaring =2.8.0`, and lockfile `croaring-sys 5.2.2`. D39 authorizes this benchmark-only C dependency; it is absent from the root/ti production graph. No production dependency or store format changed.

Verified toolchain: rustc 1.99.0, LLVM 23.1.1, x86_64-unknown-linux-gnu container. Release, LTO off, 16 codegen units. Both backends execute in the **same process**, warmup then nine alternating-order rounds, 3,000 operations each (301 for interval enumeration). No local build overlapped the retained run. Ratios below are roaring duration / candidate duration, so larger is faster. Absolute nanoseconds are recorded for reproduction, not host performance claims.

A 65,536-bucket shard holds 12-bit unsigned values. Dense, approximately 10%-present sparse, and run-heavy inputs exercise high-to-low BSI range comparison [1200,2400] and six-step AND/ANDNOT chains. The BSI algorithm uses the core equal-prefix approach but computes only the two range partitions needed; it does not benchmark the complete signed BSI API or SQL execution. Interval enumeration walks 51,200 set positions forming 64 intervals.

Owned CRoaring and Frozen inputs are run-optimized before timing. All Portable views borrow **original roaring-produced bytes**, including interval input, to represent today's store serialization. Preparation costs are excluded from operations; deserialization/view-open costs are separately measured. Assertions check the BSI result against a naive scalar oracle, nonempty chain equality, interval counts, and portable cross-reading in both libraries. Every assertion passed in the retained run.

## Results

| Operation/input | CRoaring owned | Frozen view | Existing Portable view |
|---|---:|---:|---:|
| bsi-range/dense | 1.16× | 1.18× | 1.17× |
| and-andnot-chain/dense | 1.33× | 1.29× | 1.21× |
| deserialize/dense | 2.37× | 34.68× | 35.15× |
| bsi-range/sparse | 2.31× | 2.29× | 2.32× |
| and-andnot-chain/sparse | 1.35× | 1.31× | 1.35× |
| deserialize/sparse | 2.23× | 86.95× | 88.29× |
| bsi-range/runs | 11.83× | 11.76× | 1.09× |
| and-andnot-chain/runs | 10.25× | 10.30× | 1.05× |
| runs/interval-enumeration | 1.54× | 1.51× | 0.66× |
| deserialize/runs | 13.37× | 32.34× | 34.14× |

Frozen's advantage is principally avoiding copies at open, plus run representation where applicable; it does not make operations uniformly faster than CRoaring owned bitmaps. Portable views retain the ~2.32× sparse BSI gain without changing persisted bytes. Existing portable run-heavy chain/range gains are small, and interval enumeration is **0.66×** (about 1.51× slower). Thus a production switch has real workload tradeoffs.

Deserialization compares one highest-bit-plane row: roaring reads its portable bytes; CRoaring owned reads its own optimized portable bytes; views open their respective Frozen/original-roaring Portable buffers. These are **per-row warm in-memory opens**, not whole-shard startup, file validation, RSS, or mmap page-fault measurements. The run-heavy owned decoder benefits from a smaller serialized input, so its 13.37× ratio is not an equal-byte parser comparison.

| Highest-bit-plane input | roaring Portable bytes | CRoaring Portable bytes | Frozen bytes |
|---|---:|---:|---:|
| dense | 8208 | 8208 | 8201 |
| sparse | 6692 | 6692 | 6685 |
| runs | 8208 | 15 | 13 |

The long-run row shrinks from 8,208 to 15 portable bytes after CRoaring run optimization. Frozen is 13 bytes before up to 31 alignment-padding bytes. Dense/sparse savings are negligible. Both libraries cross-read every tested Portable representation with equal values. Re-encoding rows with run containers would nevertheless change seal hashes and cannot silently replace the frozen store contract.

## Adoption considerations and decision

[Upstream Frozen documentation](https://docs.rs/croaring/latest/croaring/enum.Frozen.html) describes a native memory layout with architecture/endianness/version dependence and required alignment. It is unsuitable as an unversioned portable replication format. [Portable documentation](https://docs.rs/croaring/latest/croaring/enum.Portable.html) describes the interoperable little-endian alternative; portable views avoid a format change on this platform. [View implementation/safety requirements](https://docs.rs/croaring/latest/src/croaring/bitmap/serialization.rs.html) require valid exact-length backing bytes and proper lifetime/alignment. The harness's unsafe blocks only borrow immutable self-serialized data, with alignment asserted for Frozen; they are **not** an implementation for untrusted or corrupted store files.

CRoaring also adds a C build via `croaring-sys`/`cc`. D39 permits it only in the isolated benchmark. A production adoption would need a separate dependency decision, safe validated-file/lifetime integration, Pi/aarch64-musl compilation and end-to-end CPU/RSS measurements. These remain untested here. A library microbenchmark alone cannot establish the shard-at-a-time RSS or SQL gains that motivate this gate.

D40 therefore rejects production adoption now and retains existing portable seals and roaring. Follow-up proposal: prototype validated **Portable views over current files**, measure sparse predicates and the interval slowdown in real query workloads, and compare RSS/open cost across many shards. If that wins end to end, propose a new C-dependency decision and implementation; native Frozen persistence is not required to obtain the portable-view sparse win.

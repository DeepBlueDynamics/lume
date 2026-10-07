# D43: offline library extraction

Status: approved by Industrial Pike (2026-10-07). Runtime: direct lopdf. Generated comparison and host release-size/aarch64 measurements are tracked below.

| Decision | Reason |
|---|---|
| Add optional root `lopdf =0.44.0` with default features disabled, plus optional `zip =8.6.0` with only deflate-flate2 and `quick-xml =0.42.0` without optional features, behind a `pdf` feature. Include `pdf` in `ti`, retain the current default feature set. | The Pi's existing TI build must index PDFs/EPUBs without Python or Grub. Direct lopdf provides per-page extraction and bounded decompression; zip/XML provide EPUB spine order and reuse Lume's HTML cleaner. These are MIT-licensed Rust libraries; no native PDF renderer, OCR, new C codec, aws-lc, or downloaded test fixture is intended. `cargo tree --locked --features pdf -p lume -e normal,build --prefix depth` verified the new PDF/ZIP/XML branches use Rust codecs/parsers and no new C build dependency; existing ureq/ring is separate. |

When available, the existing `uv run lib/lume_extractor.py pdf` remains first.
When unavailable or unsuccessful, use the compiled Rust fallback. PDF sections
retain `Page N`, original page number and filename. Image-only/empty pages,
encrypted files and per-page extraction errors are skipped with counted warnings.
EPUB sections follow the OPF spine, one section per chapter, with the existing
HTML cleaner. Missing/unsupported encryption is reported and skipped.

Extraction runs in an isolated child of the Lume executable. A parent watchdog
enforces a per-file deadline and bounded captured output; the Linux/Pi watchdog
also kills the worker above the RSS budget. Portable input, page-decompression,
chapter and total-text caps complement that RSS guard. A worker error or panic
becomes a counted file warning instead of aborting the indexing run.

Approved limits: 128 MiB input, 8 MiB decompressed page/chapter, 64 MiB
total extracted text, 512 MiB worker RSS on Linux, 120 s per file. Test timeout,
image-only, malformed and encrypted handling. Use generated fixtures, including
a 900-page PDF. The generated-fixture comparison is recorded below. No aarch64 build success is claimed here.

## Alternatives and sources

- [lopdf 0.44.0 manifest](https://raw.githubusercontent.com/J-F-Liu/lopdf/v0.44.0/Cargo.toml):
  MIT, MSRV 1.88, optional threading/date/image features disabled; direct
  cryptography/compression dependencies require lockfile audit.
- [lopdf page/decompression API](https://docs.rs/lopdf/0.44.0/lopdf/struct.Document.html):
  `extract_text_with_limit` and bounded page-content decoding.
- [pdf-extract 0.12.1 manifest](https://raw.githubusercontent.com/jrmuizel/pdf-extract/master/Cargo.toml):
  MIT, built on lopdf 0.42 plus Adobe CMap/PostScript/CFF/Type1 parsers. Potentially
  better font/layout extraction, with a larger dependency set; benchmark before
  making a speed or size claim.
- [zip documentation](https://docs.rs/zip/8.6.0/zip/): MIT; all codec defaults
  disabled, deflate only.
- [quick-xml documentation](https://docs.rs/quick-xml/0.42.0/quick_xml/):
  streaming XML; no external entity fetching.

## Generated 900-page comparison

Verified with the isolated [harness](../../bench/pdf-eval/README.md), Linux x86_64
container/rustc 1.99, sequential release builds, six full load/extract calls each.
First-call timings are not OS-cold-cache measurements.

| Candidate | First call ms | Warm median ms | Warm maximum ms | Standalone release bytes |
|---|---:|---:|---:|---:|
| direct lopdf 0.44.0 | 98.365 | 100.197 | 100.565 | 1,716,976 |
| pdf-extract 0.12.1 | 142.311 | 126.602 | 128.961 | 1,945,880 |

Both returned 900 pages and the same whitespace-normalized checksum
f6582c4b3fdf71b0. Raw characters differ by 900 (51,192 vs 52,092); preserve these
counts in [raw results](../../bench/pdf-eval/results.json). Direct lopdf's warm
median was 1.26x faster and its standalone executable 228,904 bytes smaller.
Together with bounded decompression and fewer font parsers, this supports the
approved direct-lopdf choice. Both top-level crates are MIT; source links above.

These executable sizes exclude Lume, EPUB and TI. Actual Lume release binary
growth is pending the lead's post-merge host build. The verified pre-D43 Windows
host release `--features ti` baseline at 925fa8c (including crawl --list) is
112,273,408 bytes, reported by the lead. It matches b3cce8b because of PE section
alignment. Compare the post-merge build against 925fa8c. No real 900-page book was benchmarked, and generated
Helvetica text does not prove complex-font/layout fidelity. aarch64 build/Pi
timings remain host checks. The audited Rust-only new dependency branches are
portability evidence, not a successful aarch64 build.

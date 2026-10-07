# PDF fallback comparison

Isolated benchmark workspace; pdf-extract is benchmark-only, not linked into
Lume. Runtime D43 selects direct lopdf 0.44.0 with defaults off. The candidate
pdf-extract 0.12.1 uses lopdf 0.42 plus font/layout parsers.

Run sequentially, with CARGO_INCREMENTAL=0:
```sh
CARGO_TARGET_DIR=target/pdf-eval cargo run --release --locked --manifest-path bench/pdf-eval/Cargo.toml --features direct -- .test-tmp/900-pages.pdf
CARGO_TARGET_DIR=target/pdf-eval cargo run --release --locked --manifest-path bench/pdf-eval/Cargo.toml --features extract -- .test-tmp/900-pages.pdf
```
If the path does not exist, the harness generates a 900-page Helvetica text
fixture, with no network. An existing PDF path can be supplied to compare a
real book (the fixture generator will not overwrite it). Six load/extract calls
per process; cold_ms means first call, not an OS-cache flush. Five remaining
calls give the warm median and maximum. Text whitespace is normalized per page
before a deterministic checksum, separating layout whitespace from missing words.
Raw character counts remain visible.

release_binary_bytes measures this **standalone candidate executable** with
LTO/codegen-units=1/strip=true. It does not measure full Lume growth, which the
lead measures before/after the host release TI build. The benchmark contains no
EPUB or TI engine code. Generated text does not represent complex font encodings,
multi-column books or image/OCR fidelity. Container-relative CPU comparisons do
not establish Pi absolute times or an aarch64 build.

Results are recorded in [results.json](results.json) and the
[D43 record](../../plan/decisions/D43-library-extraction.md).

# Offline cruiser library

Implementation is on ti/library-index. D43 is approved in
[the decision record](../decisions/D43-library-extraction.md).

## Query surfaces

Both commands accept an ordinary index:
```text
lume serve --ti-store STORE --docs-index INDEX
lume ti ingest --store STORE --serve --docs-index INDEX
```

The shared HTTP/MCP/pgwire session registers sections and, when available,
entities/entity_edges. This is the same lexical BM25 provider as lume sql and
lume ti query --docs-index. No LLM or external search service is used.

Index publication writes manifest.json after BM25, spelling and graph files.
Each query surface checks its mtime and length, waits for two stable seconds,
then swaps the complete engine. Existing prepared pgwire queries see new rows
on execution. A missing initial index exposes empty sections so the first
library build can happen later. Invalid publications retain the last working
snapshot and retry on a subsequent changed publication; deleting every document publishes empty tables. Old indexes
without manifest.json use bm25.json mtime/length as the compatibility marker.
Reload occurs on requests; no request means no polling thread or background I/O.

## Extraction and limits

The approved pdf feature enables PDF and EPUB; ti includes it and the plain
default build remains unchanged. PDF first attempts the existing UV extractor
when its script is installed. Otherwise the compiled Rust worker runs offline.
Page titles/line numbers remain Page N and EPUB sections follow OPF spine order.
XHTML uses the existing HTML cleaner. ZIP members stay in memory; no archive
member is extracted onto the filesystem. HTML chapters with encryption references
are skipped; font-obfuscation entries do not exclude readable chapters.

The parent supervises an isolated process, with a shared 120-second deadline for
UV and fallback, bounded JSON output and a Linux/Pi aggregate worker/descendant
RSS watchdog. Limits: 128 MiB input, 8 MiB decompressed streams/chapters, 64 MiB
total extracted text, 512 MiB RSS. Linux watchdog checks every 20 ms, so RSS can
briefly exceed the budget. Other platforms enforce input/output/decompression
limits; full process RSS enforcement is Linux-only. UNIX process groups kill
UV's Python descendants on timeout. Worker errors and crashes become counted
file warnings; blank/image-only and unreadable pages become counted unit warnings.
There is no OCR, DRM decryption or ZIM extraction in this slice.

Generated fixtures and candidate benchmarks are local and need no network.
Generated 900-page text is a controlled speed/size comparison, not evidence of
real-book font fidelity. aarch64 build and Pi timings remain host checks.

# D44 — bundled Signal K plugin binaries

Approved: bundle Linux arm64 and x64 Lume binaries in the npm tarball, with
no install-time scripts or binary downloads. Signal K 2.31 installs plugins
with --ignore-scripts, so a postinstall downloader would not run reliably.
The package uses an explicit files allowlist and a prepack integrity check.
See [Signal K's installer](https://github.com/SignalK/signalk-server/blob/v2.31.0/src/modules.ts).

The release builder validates 64-bit little-endian ELF machine, executable
entry point, Linux loader and maximum GLIBC symbol requirement (ceiling 2.39).
Strip on the build host with native strip --strip-all or release profile
strip = "symbols". Already-stripped inputs are accepted after checking absence
of .symtab and .debug_* sections. For unstripped inputs, the builder strips
staged copies with LLVM or target GNU strip and validates again;
generic objcopy that changes the machine header is rejected. Original
artifacts are preserved. The manifest records source and stripped sizes and
SHA-256 checksums. No binaries are committed.

Build arm64 natively inside Ubuntu 24.04 on the Pi with thin LTO, or cross
with cargo-zigbuild targeting glibc 2.36. Native x64 builds use glibc 2.36.
Use CARGO_PROFILE_RELEASE_STRIP=symbols (equivalent to release profile
strip = "symbols") for release builds; retain the existing repository-wide
profile and select this packaging profile through the environment. The
builder also strips older unstripped artifacts. Publication requires both
architectures from the same reviewed revision.

Measurement inputs: the supplied arm64 artifact is the native Pi thin-LTO
build of 1bbbac2, 184,876,328 bytes, SHA-256
27f598853d0846ea906da0e81823e9582144e14aec2a644d0dbf50146cc6a2f8,
with GLIBC maximum 2.39. The current x64 build uses the lane's 65b84a3 base.
These mixed-revision inputs are for packaging validation and size measurement;
they are not a matched publishable release. The lead supplied a natively
stripped arm64 copy: 133,878,064 bytes,
SHA-256 6a159dea2149de64636838ca6d375cb98dd44ba08da08f2b438afdf39c7b42bf.
This saves 50,998,264 bytes (27.6%). The lead verified it runs on the Pi,
reports lume 0.12.0 and retains GLIBC maximum 2.39. Local ELF and checksum
checks and final tarball measurements are recorded below.

## Measured package

Native x64 build command:

```bash
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_STRIP=symbols \
cargo build --locked --release --features ti
```

It passed in 16m24s, with the existing unused-assignment warning in
src/main.rs. Cache use remained about 1.5 GiB. The copied x64 artifact runs
and reports lume 0.12.0. Local ELF/readelf inspection and streamed SHA-256
checks verified both packaged inputs; file was unavailable in this container.

| Architecture | Before stripping (bytes) | Packaged bytes | Maximum GLIBC |
|---|---:|---:|---|
| arm64, 1bbbac2 | 184,876,328 | 133,878,064 | 2.39 |
| x64, 65b84a3 | 100,840,168 (built with strip=symbols) | 100,840,168 | 2.35 |

The arm64 before/after sizes include the lead's native stripping step; the
builder accepted its already-stripped result. The x64 input was already
stripped by Cargo, so the builder did not shrink it further.

npm pack produced **82,767,412 bytes gzip-compressed (82.77 MB / 78.93 MiB)**,
**234,961,891 bytes unpacked**, containing 25 files. Tarball SHA-256:
39707388d111fa2f61db6a590e3b61b3cfca795b6d390ba4a5f9ea192870791b.
x64 binary SHA-256:
c28af26886d368aaed31a6986507bea41d892684cc32b3e209c59a7723e60e73.

The artifacts and package-report.json are under
.lanes/data/release-artifacts and are not committed. This is a mixed-revision
measurement package, not a matched release for publication.

## Size follow-ups (not implemented)

The compressed package exceeds the approximately 60 MB target. Ranked by
expected gain, inferred from the current dependency/profile configuration;
these are hypotheses, not measured savings:

1. Use opt-level = "s" for non-hot crates. This is the broadest remaining
   reduction opportunity across the large dependency graph. Keep ingest,
   bitmap operations and query kernels at their existing optimization level,
   and benchmark throughput before accepting a profile.
2. Audit unused DataFusion features. ti-sql already disables defaults and
   selects SQL, Parquet, nested, datetime, math and string expressions, so
   remaining gains depend on proving selected functionality is unused.
   Preserve History/Grafana SQL, documents, position and intervals semantics.
3. Evaluate panic = "abort". Removing unwinding may reduce size, but changes
   failure behavior and needs a plugin-supervisor recovery check. Expected
   gain is smaller than shrinking the dependency code above.

No feature or panic/profile behavior was changed by this packaging work.
Plugin npm tests: 36 passed, no failures or skips, including ELF/GLIBC
rejection and offline npm pack/install with --ignore-scripts.

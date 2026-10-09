# Lume TI server image: the released linux-arm64 lume binary on Debian slim.
# COPY only (no RUN), so scripts/build-lume-container.sh can build the arm64
# image on any host without emulation. The build context holds `lume`, taken
# from the GitHub release tarball and checked against SHA256SUMS.
FROM debian:bookworm-slim
COPY --chmod=755 lume /usr/local/bin/lume
COPY --chmod=755 lume-container-run lume-container-health /usr/local/bin/
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/lume-container-run"]

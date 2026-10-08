#!/bin/bash
# Build HaLOS Debian packages for Grub Crawler and Ollama container applications.
#
# Generates .deb packages into dist/halos/ using container-packaging-tools.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${REPO_ROOT}/dist/halos"

# Locate container-packaging-tools
TOOLS_DIR="${CONTAINER_TOOLS_PATH:-${REPO_ROOT}/../../container-packaging-tools}"
if [ ! -d "${TOOLS_DIR}" ]; then
    TOOLS_DIR="/workspace/container-packaging-tools"
fi

if [ ! -d "${TOOLS_DIR}" ]; then
    echo "ERROR: container-packaging-tools not found at ${TOOLS_DIR}" >&2
    exit 1
fi

mkdir -p "${DIST_DIR}"
# Clean existing deb artifacts in dist/halos
rm -f "${DIST_DIR}"/*.deb "${DIST_DIR}"/*.buildinfo "${DIST_DIR}"/*.changes

APPS=(
    "marine-grubcrawler-container"
    "marine-ollama-container"
)

echo "=== Building HaLOS container .deb packages ==="
echo "Tools: ${TOOLS_DIR}"
echo "Output: ${DIST_DIR}"
echo

for app in "${APPS[@]}"; do
    app_dir="${REPO_ROOT}/deploy/halos/${app}"
    if [ ! -d "${app_dir}" ]; then
        echo "ERROR: App directory ${app_dir} not found" >&2
        exit 1
    fi

    echo "--- Building ${app} ---"
    (
        cd "${TOOLS_DIR}"
        uv run generate-container-packages -o "${DIST_DIR}" --prefix marine "${app_dir}"
    )
    echo
done

echo "=== Built Packages ==="
ls -lh "${DIST_DIR}"/*.deb

echo
echo "=== Verifying packages with dpkg-deb ==="
for deb in "${DIST_DIR}"/*.deb; do
    echo ">>> $(basename "${deb}") info:"
    dpkg-deb -I "${deb}"
    echo
    echo ">>> $(basename "${deb}") contents:"
    dpkg-deb -c "${deb}"
    echo
done

echo "Build and verification complete. Artifacts in ${DIST_DIR}"

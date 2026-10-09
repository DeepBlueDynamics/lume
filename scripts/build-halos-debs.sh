#!/bin/bash
# Build HaLOS Debian packages for Lume TI and HaLOS container applications.
#
# Generates .deb packages into dist/halos/ using dpkg-deb and container-packaging-tools.
#
# Usage:
#   scripts/build-halos-debs.sh [options]
#
# Options:
#   --lume-bin <path>   Path to linux-arm64 lume binary
#   --app <name>        Build only a specific package (marine-lume,
#                       marine-grubcrawler-container, marine-ollama-container, all)
#   -o, --output <dir>  Output directory for .deb packages (default: dist/halos)
#   -h, --help          Show this help message
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${REPO_ROOT}/dist/halos"
LUME_BIN=""
TARGET_APP="all"

print_usage() {
    cat << EOF
Usage: $(basename "$0") [options]

Build HaLOS Debian packages (.deb) into dist/halos/.

Options:
  --lume-bin <path>   Path to linux-arm64 lume binary
  --app <name>        Build only a specific package (marine-lume,
                      marine-grubcrawler-container, marine-ollama-container, all)
  -o, --output <dir>  Output directory for .deb packages (default: dist/halos)
  -h, --help          Show this help message
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --lume-bin)
            if [ $# -lt 2 ]; then
                echo "Error: --lume-bin requires a path argument" >&2
                exit 1
            fi
            LUME_BIN="$2"
            shift 2
            ;;
        --app)
            if [ $# -lt 2 ]; then
                echo "Error: --app requires a package name" >&2
                exit 1
            fi
            TARGET_APP="$2"
            shift 2
            ;;
        -o|--output)
            if [ $# -lt 2 ]; then
                echo "Error: --output requires a directory argument" >&2
                exit 1
            fi
            DIST_DIR="$2"
            shift 2
            ;;
        -h|--help)
            print_usage
            exit 0
            ;;
        *)
            echo "Error: Unknown argument $1" >&2
            print_usage >&2
            exit 1
            ;;
    esac
done

mkdir -p "${DIST_DIR}"

build_marine_lume() {
    echo "--- Building marine-lume ---"
    if ! command -v dpkg-deb >/dev/null 2>&1; then
        echo "ERROR: dpkg-deb is required to build marine-lume" >&2
        exit 1
    fi

    local bin_path="$LUME_BIN"
    if [ -z "$bin_path" ]; then
        if [ -f "${REPO_ROOT}/plugins/signalk-lume-ti/bin/linux-arm64/lume" ]; then
            bin_path="${REPO_ROOT}/plugins/signalk-lume-ti/bin/linux-arm64/lume"
        fi
    fi

    if [ -z "$bin_path" ] || [ ! -f "$bin_path" ]; then
        echo "ERROR: linux-arm64 lume binary not found. Pass --lume-bin <path>" >&2
        exit 1
    fi

    local pkg_dir="${REPO_ROOT}/deploy/halos/marine-lume"
    if [ ! -d "$pkg_dir" ]; then
        pkg_dir="${REPO_ROOT}/deploy/halos/marine-lume-container"
    fi
    if [ ! -d "$pkg_dir" ]; then
        echo "ERROR: Package source directory not found at deploy/halos/marine-lume" >&2
        exit 1
    fi

    local plugin_dir="${REPO_ROOT}/plugins/signalk-lume-ti"
    if [ ! -d "$plugin_dir" ]; then
        echo "ERROR: Plugin directory not found at plugins/signalk-lume-ti" >&2
        exit 1
    fi

    local version
    version="$(sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "${plugin_dir}/package.json")"
    if [ -z "$version" ]; then
        version="0.12.0"
    fi
    local deb_version="${version}-1"
    local deb_filename="marine-lume_${deb_version}_arm64.deb"

    local stage_dir
    stage_dir="$(mktemp -d -t marine-lume-stage.XXXXXX)"

    # 1. Copy Debian control files
    mkdir -p "${stage_dir}/DEBIAN"
    cp -p "${pkg_dir}/debian/control" "${stage_dir}/DEBIAN/control"
    # Ensure control version matches package version
    sed -i -E "s/^Version:.*/Version: ${deb_version}/" "${stage_dir}/DEBIAN/control"

    if [ -f "${pkg_dir}/debian/postinst" ]; then
        cp -p "${pkg_dir}/debian/postinst" "${stage_dir}/DEBIAN/postinst"
        chmod 755 "${stage_dir}/DEBIAN/postinst"
    fi
    if [ -f "${pkg_dir}/debian/prerm" ]; then
        cp -p "${pkg_dir}/debian/prerm" "${stage_dir}/DEBIAN/prerm"
        chmod 755 "${stage_dir}/DEBIAN/prerm"
    fi
    if [ -f "${pkg_dir}/debian/postrm" ]; then
        cp -p "${pkg_dir}/debian/postrm" "${stage_dir}/DEBIAN/postrm"
        chmod 755 "${stage_dir}/DEBIAN/postrm"
    fi

    # 2. Stage Signal K plugin payload
    local dest_plugin_dir="${stage_dir}/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti"
    mkdir -p "${dest_plugin_dir}"

    if command -v rsync >/dev/null 2>&1; then
        rsync -a --exclude 'node_modules' --exclude 'test' --exclude 'bin' "${plugin_dir}/" "${dest_plugin_dir}/"
    else
        tar -C "${plugin_dir}" --exclude='node_modules' --exclude='test' --exclude='bin' -cf - . | tar -C "${dest_plugin_dir}" -xf -
    fi

    # 3. Stage lume binary
    mkdir -p "${dest_plugin_dir}/bin/linux-arm64"
    cp -p "$bin_path" "${dest_plugin_dir}/bin/linux-arm64/lume"
    chmod 755 "${dest_plugin_dir}/bin/linux-arm64/lume"

    # 4. Normalize file permissions for Debian package
    find "${dest_plugin_dir}" -type d -exec chmod 755 {} +
    find "${dest_plugin_dir}" -type f -exec chmod 644 {} +
    chmod 755 "${dest_plugin_dir}/bin/linux-arm64/lume"

    # 5. Build debian package
    local out_deb="${DIST_DIR}/${deb_filename}"
    rm -f "$out_deb"
    dpkg-deb --build --root-owner-group "${stage_dir}" "${out_deb}" >/dev/null
    rm -rf "${stage_dir}"
    echo "Built: ${out_deb}"
}

build_container_app() {
    local app="$1"
    local app_dir="${REPO_ROOT}/deploy/halos/${app}"
    if [ ! -d "${app_dir}" ]; then
        echo "ERROR: App directory ${app_dir} not found" >&2
        exit 1
    fi

    local tools_dir="${CONTAINER_TOOLS_PATH:-${REPO_ROOT}/../../container-packaging-tools}"
    if [ ! -d "${tools_dir}" ]; then
        tools_dir="/workspace/container-packaging-tools"
    fi

    if [ ! -d "${tools_dir}" ]; then
        echo "WARNING: container-packaging-tools not found at ${tools_dir}; skipping ${app}" >&2
        return 0
    fi

    echo "--- Building ${app} ---"
    (
        cd "${tools_dir}"
        uv run generate-container-packages -o "${DIST_DIR}" --prefix marine "${app_dir}"
    )
}

echo "=== Building HaLOS .deb packages ==="
echo "Output: ${DIST_DIR}"
echo

case "$TARGET_APP" in
    marine-lume|marine-lume-container)
        build_marine_lume
        ;;
    marine-grubcrawler-container)
        build_container_app "marine-grubcrawler-container"
        ;;
    marine-ollama-container)
        build_container_app "marine-ollama-container"
        ;;
    all)
        build_marine_lume
        build_container_app "marine-grubcrawler-container"
        build_container_app "marine-ollama-container"
        ;;
    *)
        echo "ERROR: Unknown app '$TARGET_APP'" >&2
        exit 1
        ;;
esac

echo
echo "=== Verifying packages with dpkg-deb ==="
shopt -s nullglob
DEBS=("${DIST_DIR}"/*.deb)
shopt -u nullglob

if [ ${#DEBS[@]} -eq 0 ]; then
    echo "No .deb packages found in ${DIST_DIR}"
else
    for deb in "${DEBS[@]}"; do
        echo ">>> $(basename "${deb}") info:"
        dpkg-deb -I "${deb}"
        echo
        echo ">>> $(basename "${deb}") contents:"
        dpkg-deb -c "${deb}"
        echo
    done
fi

echo "Build and verification complete. Artifacts in ${DIST_DIR}"

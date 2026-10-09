#!/bin/bash
# Deploy Signal K Lume TI plugin and arm64 lume binary to the HaLOS Pi.
#
# Usage:
#   scripts/deploy-pi.sh <ssh-host> [--dry-run] [--lume-bin <path>]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PLUGIN_DIR="${REPO_ROOT}/plugins/signalk-lume-ti"
REMOTE_DEST="/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti"
REMOTE_STAGE="/tmp/signalk-lume-ti-stage"

SSH_HOST=""
DRY_RUN=false
LUME_BIN=""

print_usage() {
    cat << EOF
Usage: $(basename "$0") <ssh-host> [options]

Deploy signalk-lume-ti plugin and arm64 lume binary to a remote HaLOS Pi.

Arguments:
  <ssh-host>          Remote SSH destination (e.g. pi@halos.local, halos.local)

Options:
  --dry-run           Print all transfer and remote commands without executing them
  --lume-bin <path>   Path to arm64 lume binary (default: plugins/signalk-lume-ti/bin/linux-arm64/lume)
  -h, --help          Show this help message
EOF
}

# Parse command line arguments
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)
            DRY_RUN=true
            shift
            ;;
        --lume-bin)
            if [ $# -lt 2 ]; then
                echo "Error: --lume-bin requires a path argument" >&2
                exit 1
            fi
            LUME_BIN="$2"
            shift 2
            ;;
        -h|--help)
            print_usage
            exit 0
            ;;
        -*)
            echo "Error: Unknown option $1" >&2
            print_usage >&2
            exit 1
            ;;
        *)
            if [ -z "$SSH_HOST" ]; then
                SSH_HOST="$1"
                shift
            else
                echo "Error: Unexpected argument $1" >&2
                print_usage >&2
                exit 1
            fi
            ;;
    esac
done

if [ -z "$SSH_HOST" ]; then
    echo "Error: Missing required <ssh-host> argument" >&2
    print_usage >&2
    exit 1
fi

if [ ! -d "$PLUGIN_DIR" ]; then
    echo "Error: Plugin directory not found at $PLUGIN_DIR" >&2
    exit 1
fi

# Locate lume binary if specified or if present in source
if [ -n "$LUME_BIN" ]; then
    if [ ! -f "$LUME_BIN" ]; then
        echo "Error: Specified --lume-bin not found at $LUME_BIN" >&2
        exit 1
    fi
elif [ -f "${PLUGIN_DIR}/bin/linux-arm64/lume" ]; then
    LUME_BIN="${PLUGIN_DIR}/bin/linux-arm64/lume"
fi

SSH_CMD=(ssh)
SCP_CMD=(scp)
RSYNC_RSH=()
if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
    SSH_CMD=(ssh -F "$LUME_DEPLOY_SSH_CONFIG")
    SCP_CMD=(scp -F "$LUME_DEPLOY_SSH_CONFIG")
    RSYNC_RSH=(-e "ssh -F ${LUME_DEPLOY_SSH_CONFIG}")
fi

run_remote() {
    local cmd="$1"
    if [ "$DRY_RUN" = true ]; then
        if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
            echo "[dry-run] ssh -F ${LUME_DEPLOY_SSH_CONFIG} ${SSH_HOST} '${cmd}'"
        else
            echo "[dry-run] ssh ${SSH_HOST} '${cmd}'"
        fi
    else
        "${SSH_CMD[@]}" "$SSH_HOST" "$cmd"
    fi
}

echo "=== Signal K Lume TI Plugin Pi Deployment ==="
echo "Target: ${SSH_HOST}"
echo "Remote destination: ${REMOTE_DEST}"
[ -n "$LUME_BIN" ] && echo "Lume binary: ${LUME_BIN}"
[ "$DRY_RUN" = true ] && echo "Mode: DRY RUN (no commands will be executed)"
echo

if [ "$DRY_RUN" = true ]; then
    echo "--- Transfer Stage ---"
    if command -v rsync >/dev/null 2>&1; then
        if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
            echo "[dry-run] rsync -e 'ssh -F ${LUME_DEPLOY_SSH_CONFIG}' -avz --exclude 'node_modules' --exclude 'test' ${PLUGIN_DIR}/ ${SSH_HOST}:${REMOTE_STAGE}/"
        else
            echo "[dry-run] rsync -avz --exclude 'node_modules' --exclude 'test' ${PLUGIN_DIR}/ ${SSH_HOST}:${REMOTE_STAGE}/"
        fi
    else
        if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
            echo "[dry-run] scp -F ${LUME_DEPLOY_SSH_CONFIG} -r (excluding node_modules, test) ${PLUGIN_DIR}/* ${SSH_HOST}:${REMOTE_STAGE}/"
        else
            echo "[dry-run] scp -r (excluding node_modules, test) ${PLUGIN_DIR}/* ${SSH_HOST}:${REMOTE_STAGE}/"
        fi
    fi
    if [ -n "$LUME_BIN" ]; then
        if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
            echo "[dry-run] scp -F ${LUME_DEPLOY_SSH_CONFIG} ${LUME_BIN} ${SSH_HOST}:${REMOTE_STAGE}/bin/linux-arm64/lume"
        else
            echo "[dry-run] scp ${LUME_BIN} ${SSH_HOST}:${REMOTE_STAGE}/bin/linux-arm64/lume"
        fi
    fi
    echo
    echo "--- Remote Commands ---"
    run_remote "sudo -n mkdir -p ${REMOTE_DEST}"
    # The running binary cannot be overwritten in place (ETXTBSY): install it as lume.new,
    # keep the old one as lume.prev, then rename over it.
    run_remote "sudo -n mkdir -p ${REMOTE_DEST}/bin/linux-arm64 && if [ -f ${REMOTE_STAGE}/bin/linux-arm64/lume ]; then sudo -n mv ${REMOTE_STAGE}/bin/linux-arm64/lume ${REMOTE_DEST}/bin/linux-arm64/lume.new; fi && sudo -n cp -r ${REMOTE_STAGE}/* ${REMOTE_DEST}/ && if [ -f ${REMOTE_DEST}/bin/linux-arm64/lume.new ]; then if [ -f ${REMOTE_DEST}/bin/linux-arm64/lume ]; then sudo -n cp -p ${REMOTE_DEST}/bin/linux-arm64/lume ${REMOTE_DEST}/bin/linux-arm64/lume.prev; fi; sudo -n mv -f ${REMOTE_DEST}/bin/linux-arm64/lume.new ${REMOTE_DEST}/bin/linux-arm64/lume; fi"
    run_remote "sudo -n chmod 755 ${REMOTE_DEST}/bin/linux-arm64/lume 2>/dev/null || true"
    run_remote "sudo -n chown -R 1000:1000 ${REMOTE_DEST}"
    run_remote "rm -rf ${REMOTE_STAGE}"
    run_remote "sudo -n systemctl restart marine-signalk-server-container"
    # shellcheck disable=SC2016 # Expression in single quotes intentionally expands on remote host
    run_remote 'echo "Waiting for marine-signalk-server-container health..."; for i in $(seq 1 30); do if sudo -n systemctl is-active --quiet marine-signalk-server-container && curl -fsS -m 3 http://127.0.0.1:3000/signalk >/dev/null 2>&1; then echo "Signal K is healthy (attempt $i/30)"; exit 0; fi; sleep 2; done; echo "Timed out waiting for Signal K health" >&2; exit 1'
    echo
    echo "Dry run complete."
    exit 0
fi

# Real deployment execution
STAGE_DIR="$(mktemp -d -t lume-plugin-stage.XXXXXX)"
trap 'rm -rf "${STAGE_DIR}"' EXIT

echo "Staging plugin files (excluding node_modules and test)..."
if command -v rsync >/dev/null 2>&1; then
    rsync -a --exclude 'node_modules' --exclude 'test' "${PLUGIN_DIR}/" "${STAGE_DIR}/"
else
    tar -C "${PLUGIN_DIR}" --exclude='node_modules' --exclude='test' -cf - . | tar -C "${STAGE_DIR}" -xf -
fi

if [ -n "$LUME_BIN" ]; then
    mkdir -p "${STAGE_DIR}/bin/linux-arm64"
    cp -p "$LUME_BIN" "${STAGE_DIR}/bin/linux-arm64/lume"
    chmod 755 "${STAGE_DIR}/bin/linux-arm64/lume"
fi

echo "Uploading staged files to ${SSH_HOST}:${REMOTE_STAGE}..."
run_remote "rm -rf ${REMOTE_STAGE} && mkdir -p ${REMOTE_STAGE}"

if command -v rsync >/dev/null 2>&1; then
    rsync "${RSYNC_RSH[@]}" -avz "${STAGE_DIR}/" "${SSH_HOST}:${REMOTE_STAGE}/"
else
    "${SCP_CMD[@]}" -r "${STAGE_DIR}/." "${SSH_HOST}:${REMOTE_STAGE}/"
fi

echo "Installing files into ${REMOTE_DEST}..."
run_remote "sudo -n mkdir -p ${REMOTE_DEST}"
# The running binary cannot be overwritten in place (ETXTBSY): install it as lume.new,
# keep the old one as lume.prev, then rename over it.
run_remote "sudo -n mkdir -p ${REMOTE_DEST}/bin/linux-arm64 && if [ -f ${REMOTE_STAGE}/bin/linux-arm64/lume ]; then sudo -n mv ${REMOTE_STAGE}/bin/linux-arm64/lume ${REMOTE_DEST}/bin/linux-arm64/lume.new; fi && sudo -n cp -r ${REMOTE_STAGE}/* ${REMOTE_DEST}/ && if [ -f ${REMOTE_DEST}/bin/linux-arm64/lume.new ]; then if [ -f ${REMOTE_DEST}/bin/linux-arm64/lume ]; then sudo -n cp -p ${REMOTE_DEST}/bin/linux-arm64/lume ${REMOTE_DEST}/bin/linux-arm64/lume.prev; fi; sudo -n mv -f ${REMOTE_DEST}/bin/linux-arm64/lume.new ${REMOTE_DEST}/bin/linux-arm64/lume; fi"
run_remote "sudo -n chmod 755 ${REMOTE_DEST}/bin/linux-arm64/lume 2>/dev/null || true"
run_remote "sudo -n chown -R 1000:1000 ${REMOTE_DEST}"
run_remote "rm -rf ${REMOTE_STAGE}"

echo "Restarting marine-signalk-server-container..."
run_remote "sudo -n systemctl restart marine-signalk-server-container"

echo "Polling Signal K for health..."
# shellcheck disable=SC2016 # Expression in single quotes intentionally expands on remote host
run_remote 'echo "Waiting for marine-signalk-server-container health..."; for i in $(seq 1 30); do if sudo -n systemctl is-active --quiet marine-signalk-server-container && curl -fsS -m 3 http://127.0.0.1:3000/signalk >/dev/null 2>&1; then echo "Signal K is healthy (attempt $i/30)"; exit 0; fi; sleep 2; done; echo "Timed out waiting for Signal K health" >&2; exit 1'

echo "Deployment complete and verified healthy."

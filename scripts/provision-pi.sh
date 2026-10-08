#!/bin/bash
# Provision a reflashed or new HaLOS Pi for Lume TI and Signal K.
#
# Usage:
#   scripts/provision-pi.sh <ssh-host> [options]
#
# Steps (idempotent; second run skips completed steps):
#   1. Report HaLOS/Signal K version, df -h /, and RAM (free -h).
#   2. Memory cgroup: check /proc/cmdline for cgroup_enable=memory; if missing,
#      backup cmdline.txt to cmdline.txt.bak-pre-memcg, append cgroup_enable=memory cgroup_memory=1,
#      and print REBOOT NEEDED (never reboots automatically).
#   3. Pin Signal K self vessel UUID: read existing UUID from baseDeltas.json or settings.json
#      and report it; if absent, print a warning only (never invent one).
#   4. Install HaLOS app .debs from --debs with sudo -n apt-get install -y ./x.deb,
#      skipping marine-ollama-container by default (override with --with-ollama).
#   5. Call deploy-pi.sh to deploy the plugin and verify health.
#   6. Print SETUP §13 key-file instructions (never handles a key).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEPLOY_SCRIPT="${SCRIPT_DIR}/deploy-pi.sh"

SSH_HOST=""
DRY_RUN=false
LUME_BIN=""
DEBS_DIR=""
WITH_OLLAMA=false

print_usage() {
    cat << EOF
Usage: $(basename "$0") <ssh-host> [options]

Provision or re-provision a HaLOS Raspberry Pi for Lume TI.

Arguments:
  <ssh-host>          Remote SSH destination (e.g. pi@halos.local, halos.local)

Options:
  --dry-run           Print all remote commands without executing them
  --lume-bin <path>   Path to arm64 lume binary passed to deploy-pi.sh
  --debs <dir>        Directory containing HaLOS app .deb packages to install
  --with-ollama       Also install marine-ollama-container deb (skipped by default)
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
        --debs)
            if [ $# -lt 2 ]; then
                echo "Error: --debs requires a directory argument" >&2
                exit 1
            fi
            DEBS_DIR="$2"
            shift 2
            ;;
        --with-ollama)
            WITH_OLLAMA=true
            shift
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

if [ -n "$LUME_BIN" ] && [ ! -f "$LUME_BIN" ]; then
    echo "Error: Specified --lume-bin not found at $LUME_BIN" >&2
    exit 1
fi

if [ -n "$DEBS_DIR" ] && [ ! -d "$DEBS_DIR" ]; then
    echo "Error: Specified --debs directory not found at $DEBS_DIR" >&2
    exit 1
fi

SSH_CMD=(ssh)
SCP_CMD=(scp)
RSYNC_RSH=()
if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
    SSH_CMD=(ssh -F "$LUME_DEPLOY_SSH_CONFIG")
    SCP_CMD=(scp -F "$LUME_DEPLOY_SSH_CONFIG")
    RSYNC_RSH=(-e "ssh -F ${LUME_DEPLOY_SSH_CONFIG}")
fi

PROC_CMDLINE="${LUME_PROC_CMDLINE:-/proc/cmdline}"
BOOT_CMDLINE="${LUME_BOOT_CMDLINE:-/boot/firmware/cmdline.txt}"

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

remote_test() {
    local cmd="$1"
    "${SSH_CMD[@]}" "$SSH_HOST" "$cmd" >/dev/null 2>&1
}

echo "=== HaLOS Pi Provisioning ==="
echo "Target: ${SSH_HOST}"
[ -n "$LUME_BIN" ] && echo "Lume binary: ${LUME_BIN}"
[ -n "$DEBS_DIR" ] && echo "HaLOS .debs directory: ${DEBS_DIR}"
echo "Include Ollama container: ${WITH_OLLAMA}"
[ "$DRY_RUN" = true ] && echo "Mode: DRY RUN (no commands will be executed)"
echo

if [ "$DRY_RUN" = true ]; then
    echo "--- Step 1: System Status (HaLOS/Signal K version, df, RAM) ---"
    run_remote "echo '=== HaLOS / Signal K System Status ==='; if [ -f /etc/os-release ]; then grep -E '^(PRETTY_NAME|NAME|VERSION)=' /etc/os-release; fi; dpkg -s marine-signalk-server-container 2>/dev/null | grep -E '^(Package|Version|Status):' || true; df -h /; free -h"
    echo
    echo "--- Step 2: Memory Cgroup Configuration ---"
    run_remote "if test -f '${PROC_CMDLINE}' && grep -q 'cgroup_enable=memory' '${PROC_CMDLINE}'; then echo 'Memory cgroup active in kernel'; elif test -f '${BOOT_CMDLINE}' && grep -q 'cgroup_enable=memory' '${BOOT_CMDLINE}'; then echo 'Memory cgroup configured in cmdline.txt (reboot pending)'; else sudo -n cp '${BOOT_CMDLINE}' '${BOOT_CMDLINE}.bak-pre-memcg' && sudo -n bash -c 'line=\$(tr -d \"\r\n\" < \"${BOOT_CMDLINE}\") && printf \"%s cgroup_enable=memory cgroup_memory=1\n\" \"\$line\" > \"${BOOT_CMDLINE}\"' && echo 'REBOOT NEEDED'; fi"
    echo
    echo "--- Step 3: Signal K Self Vessel UUID ---"
    run_remote "echo 'Reading Signal K self vessel UUID from baseDeltas.json or settings.json...'"
    echo
    echo "--- Step 4: Install HaLOS App Debs ---"
    if [ -n "$DEBS_DIR" ]; then
        if command -v rsync >/dev/null 2>&1; then
            if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
                echo "[dry-run] rsync -e 'ssh -F ${LUME_DEPLOY_SSH_CONFIG}' -avz ${DEBS_DIR}/*.deb ${SSH_HOST}:/tmp/halos-debs-stage/"
            else
                echo "[dry-run] rsync -avz ${DEBS_DIR}/*.deb ${SSH_HOST}:/tmp/halos-debs-stage/"
            fi
        else
            if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
                echo "[dry-run] scp -F ${LUME_DEPLOY_SSH_CONFIG} ${DEBS_DIR}/*.deb ${SSH_HOST}:/tmp/halos-debs-stage/"
            else
                echo "[dry-run] scp ${DEBS_DIR}/*.deb ${SSH_HOST}:/tmp/halos-debs-stage/"
            fi
        fi
        run_remote "sudo -n apt-get install -y /tmp/halos-debs-stage/*.deb (skipping marine-ollama-container unless --with-ollama)"
    else
        echo "[dry-run] No --debs directory specified; skipping HaLOS app package installation."
    fi
    echo
    echo "--- Step 5: Deploy Signal K Lume TI Plugin ---"
    DEPLOY_CMD=("bash" "$DEPLOY_SCRIPT" "$SSH_HOST" "--dry-run")
    [ -n "$LUME_BIN" ] && DEPLOY_CMD+=(--lume-bin "$LUME_BIN")
    "${DEPLOY_CMD[@]}"
    echo
    echo "--- Step 6: Ask Tab Key-File Instructions (SETUP §13) ---"
    cat << 'EOF'
[dry-run] Instructions for creating ollama.key on the Pi:
   d=/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti
   sudo install -d -o 1000 -g 1000 -m 755 "$d"
   sudo install -o 1000 -g 1000 -m 600 /dev/null "$d/ollama.key"
   read -rs -p 'ollama.com API key: ' k; echo
   printf '%s\n' "$k" | sudo tee "$d/ollama.key" > /dev/null; unset k
   sudo ls -l "$d/ollama.key"
EOF
    echo
    echo "Dry run complete."
    exit 0
fi

# ==============================================================================
# Real Execution (Idempotent)
# ==============================================================================

# ------------------------------------------------------------------------------
# Step 1: Report HaLOS / Signal K version, df, RAM
# ------------------------------------------------------------------------------
echo "=== Step 1: System Status (HaLOS/Signal K version, df, RAM) ==="
STATUS_MARKER="/tmp/.provision-pi-sysinfo-reported"
if remote_test "test -f '${STATUS_MARKER}'"; then
    echo "[SKIP] System status already reported."
else
    run_remote '
        echo "--- OS / Platform ---"
        if [ -f /etc/os-release ]; then
            grep -E "^(PRETTY_NAME|NAME|VERSION)=" /etc/os-release || true
        fi
        echo
        echo "--- Signal K Container App ---"
        if dpkg -s marine-signalk-server-container >/dev/null 2>&1; then
            dpkg -s marine-signalk-server-container | grep -E "^(Package|Version|Status):" || true
        elif [ -f /var/lib/container-apps/marine-signalk-server-container/data/data/package.json ]; then
            grep -E "\"version\"" /var/lib/container-apps/marine-signalk-server-container/data/data/package.json || true
        else
            echo "marine-signalk-server-container package not installed via dpkg"
        fi
        echo
        echo "--- Disk Usage (df -h /) ---"
        df -h /
        echo
        echo "--- Memory Usage (free -h) ---"
        free -h || true
        touch /tmp/.provision-pi-sysinfo-reported
    '
fi
echo

# ------------------------------------------------------------------------------
# Step 2: Memory cgroup configuration
# ------------------------------------------------------------------------------
echo "=== Step 2: Memory Cgroup Configuration ==="
if remote_test "test -f '${PROC_CMDLINE}' && grep -q 'cgroup_enable=memory' '${PROC_CMDLINE}'"; then
    echo "[SKIP] Memory cgroup already active in kernel (${PROC_CMDLINE})."
elif remote_test "test -f '${BOOT_CMDLINE}' && grep -q 'cgroup_enable=memory' '${BOOT_CMDLINE}'"; then
    echo "[SKIP] Memory cgroup already configured in ${BOOT_CMDLINE} (reboot pending)."
elif ! remote_test "test -f '${BOOT_CMDLINE}'"; then
    echo "Warning: cmdline.txt not found at ${BOOT_CMDLINE}; skipping memory cgroup configuration."
else
    echo "Backing up ${BOOT_CMDLINE} to ${BOOT_CMDLINE}.bak-pre-memcg..."
    run_remote "sudo -n cp '${BOOT_CMDLINE}' '${BOOT_CMDLINE}.bak-pre-memcg'"
    echo "Appending cgroup_enable=memory cgroup_memory=1 to ${BOOT_CMDLINE}..."
    run_remote "sudo -n bash -c 'line=\$(tr -d \"\r\n\" < \"${BOOT_CMDLINE}\") && printf \"%s cgroup_enable=memory cgroup_memory=1\n\" \"\$line\" > \"${BOOT_CMDLINE}\"'"
    echo "REBOOT NEEDED: Memory cgroup parameters added to ${BOOT_CMDLINE}. A reboot is required to activate them in the kernel."
fi
echo

# ------------------------------------------------------------------------------
# Step 3: Pin Signal K self vessel UUID
# ------------------------------------------------------------------------------
echo "=== Step 3: Pin Signal K Self Vessel UUID ==="
UUID_MARKER="/tmp/.provision-pi-uuid-checked"
SK_DATA_DIR="/var/lib/container-apps/marine-signalk-server-container/data/data"
if remote_test "test -f '${UUID_MARKER}'"; then
    CHECKED_UUID=$(run_remote "cat '${UUID_MARKER}' 2>/dev/null || echo 'checked'")
    echo "[SKIP] Signal K self vessel UUID already checked (${CHECKED_UUID})."
else
    UUID=$(run_remote "python3 -c '
import json, os, re
base_dir = \"${SK_DATA_DIR}\"
uuid = None
for fname in [\"baseDeltas.json\", \"settings.json\"]:
    fpath = os.path.join(base_dir, fname)
    if os.path.isfile(fpath):
        try:
            with open(fpath, \"r\", encoding=\"utf-8\") as f:
                content = f.read()
            m = re.search(r\"(urn:mrn:signalk:uuid:[0-9a-fA-F-]{36})\", content)
            if m:
                uuid = m.group(1)
                break
            m2 = re.search(r\"\\\"uuid\\\"\\s*:\\s*\\\"([0-9a-fA-F-]{36})\\\"\", content)
            if m2:
                uuid = \"urn:mrn:signalk:uuid:\" + m2.group(1)
                break
        except Exception:
            pass
if uuid:
    print(uuid)
' 2>/dev/null || grep -oE 'urn:mrn:signalk:uuid:[0-9a-fA-F-]{36}' '${SK_DATA_DIR}/baseDeltas.json' '${SK_DATA_DIR}/settings.json' 2>/dev/null | head -n 1 || true")

    if [ -n "$UUID" ]; then
        echo "Signal K self vessel UUID found: ${UUID}"
        run_remote "echo '${UUID}' > '${UUID_MARKER}'"
    else
        echo "Warning: Signal K self vessel UUID not found in ${SK_DATA_DIR}/baseDeltas.json or settings.json."
        echo "         Please configure vessel UUID in Signal K: Server -> Settings -> Vessel Base Data."
        run_remote "echo 'absent' > '${UUID_MARKER}'"
    fi
fi
echo

# ------------------------------------------------------------------------------
# Step 4: Install HaLOS app .debs
# ------------------------------------------------------------------------------
echo "=== Step 4: Install HaLOS App Packages ==="
if [ -z "$DEBS_DIR" ]; then
    echo "[SKIP] No --debs directory specified; skipping HaLOS app package installation."
else
    shopt -s nullglob
    LOCAL_DEBS=("$DEBS_DIR"/*.deb)
    shopt -u nullglob

    if [ ${#LOCAL_DEBS[@]} -eq 0 ]; then
        echo "[SKIP] No .deb files found in ${DEBS_DIR}; skipping installation."
    else
        REMOTE_DEB_STAGE="/tmp/halos-debs-stage"
        run_remote "rm -rf '${REMOTE_DEB_STAGE}' && mkdir -p '${REMOTE_DEB_STAGE}'"
        
        echo "Staging .deb packages to ${SSH_HOST}:${REMOTE_DEB_STAGE}..."
        if command -v rsync >/dev/null 2>&1; then
            rsync "${RSYNC_RSH[@]}" -avz "${LOCAL_DEBS[@]}" "${SSH_HOST}:${REMOTE_DEB_STAGE}/"
        else
            "${SCP_CMD[@]}" "${LOCAL_DEBS[@]}" "${SSH_HOST}:${REMOTE_DEB_STAGE}/"
        fi

        for deb_path in "${LOCAL_DEBS[@]}"; do
            deb_name="$(basename "$deb_path")"
            if [[ "$deb_name" =~ ollama ]] && [ "$WITH_OLLAMA" = false ]; then
                echo "[SKIP] Skipping marine-ollama-container package ${deb_name} (pass --with-ollama to install)."
                continue
            fi

            pkg_name=$(dpkg-deb -f "$deb_path" Package 2>/dev/null || echo "$deb_name" | sed -E 's/_.*|\.deb//')
            if remote_test "dpkg -s '${pkg_name}' 2>/dev/null | grep -q 'Status: install ok installed'"; then
                echo "[SKIP] Package ${pkg_name} is already installed."
            else
                echo "Installing package ${pkg_name} (${deb_name})..."
                run_remote "sudo -n apt-get install -y '${REMOTE_DEB_STAGE}/${deb_name}'"
            fi
        done
        run_remote "rm -rf '${REMOTE_DEB_STAGE}'"
    fi
fi
echo

# ------------------------------------------------------------------------------
# Step 5: Deploy Signal K plugin via deploy-pi.sh
# ------------------------------------------------------------------------------
echo "=== Step 5: Deploy Signal K Lume TI Plugin ==="
PLUGIN_DEST="/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti"
if remote_test "test -f '${PLUGIN_DEST}/package.json' && \
                test -f '${PLUGIN_DEST}/bin/linux-arm64/lume' && \
                test -x '${PLUGIN_DEST}/bin/linux-arm64/lume' && \
                sudo -n systemctl is-active --quiet marine-signalk-server-container"; then
    echo "[SKIP] Signal K Lume TI plugin is already deployed and active."
else
    DEPLOY_ARGS=("$SSH_HOST")
    if [ -n "$LUME_BIN" ]; then
        DEPLOY_ARGS+=(--lume-bin "$LUME_BIN")
    fi
    bash "$DEPLOY_SCRIPT" "${DEPLOY_ARGS[@]}"
fi
echo

# ------------------------------------------------------------------------------
# Step 6: Print SETUP §13 key-file instructions
# ------------------------------------------------------------------------------
echo "=== Step 6: Ask Tab with ollama.com Key File (SETUP §13) ==="
KEY_FILE="/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti/ollama.key"
KEY_MARKER="/tmp/.provision-pi-key-instructions-printed"
if remote_test "test -f '${KEY_FILE}' || test -f '${KEY_MARKER}'"; then
    echo "[SKIP] Ask tab key-file instructions already displayed (or key file exists at ${KEY_FILE})."
else
    cat << 'EOF'
The Ask tab (lume chat) defaults to calling https://ollama.com directly for
:cloud models (e.g. glm-5.3:cloud). To configure the API key securely on the Pi:

1. Create the key file empty, owned by the Signal K container user (1000:1000) with mode 600:
   d=/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti
   sudo install -d -o 1000 -g 1000 -m 755 "$d"
   sudo install -o 1000 -g 1000 -m 600 /dev/null "$d/ollama.key"

2. Write the key into it from a hidden prompt (never shows in argv, ps, or history):
   read -rs -p 'ollama.com API key: ' k; echo
   printf '%s\n' "$k" | sudo tee "$d/ollama.key" > /dev/null; unset k
   sudo ls -l "$d/ollama.key"   # expect: -rw------- 1 1000 1000

3. In Signal K Admin UI -> Server -> Plugin Config -> Lume TI:
   - Chat Ollama API URLs: https://ollama.com
   - Chat Ollama Model: glm-5.3:cloud
   - Chat API Key File Path: /home/node/.signalk/plugin-config-data/signalk-lume-ti/ollama.key

4. Verify that the key is never exposed on argv or in system logs:
   ps aux | grep "[l]ume chat"
   journalctl -u marine-signalk-server-container -n 50 | grep -i "key"
EOF
    run_remote "touch '${KEY_MARKER}' 2>/dev/null || true"
fi
echo

echo "=== Provisioning Complete ==="

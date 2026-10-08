#!/bin/bash
# Bring up a HaLOS Pi for Lume TI, Signal K, and run the gap 4 benchmark.
#
# Usage:
#   scripts/pi-bringup.sh <ssh-host> [options]
#
# Steps (chains in order, stops at first failure):
#   1. provision-pi.sh (passing --lume-bin and --debs through)
#   2. pi-retire-ollama.sh (stops/disables container, removes image, keeps data)
#   3. Pull deepbluedynamics/grubcrawler:latest-lite, restart marine-grubcrawler-container, poll health on 127.0.0.1:6792
#   4. Unless --skip-bench, run the gap 4 benchmark:
#      - ensure 1-vessel store exists on host (--bench-store), building via scripts/pi-bench-recipe.sh if missing
#      - copy store to Pi (/var/tmp/lume-pi-bench/store), skipping if remote store marker matches
#      - scp --bench-bin and corpus.json to Pi
#      - run ti-query-bench bench --pi
#      - scp bench/results/<date>-pi-<sha>.json back into bench/results/
#   5. Print summary: df -h /, active state of HaLOS services, newest telemetry_lume timestamp
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

PROVISION_SCRIPT="${SCRIPT_DIR}/provision-pi.sh"
RETIRE_SCRIPT="${SCRIPT_DIR}/pi-retire-ollama.sh"
BENCH_RECIPE_SCRIPT="${SCRIPT_DIR}/pi-bench-recipe.sh"

SSH_HOST=""
DRY_RUN=false
LUME_BIN=""
BENCH_BIN=""
BENCH_STORE=""
DEBS_DIR=""
SKIP_BENCH=false

print_usage() {
    cat << EOF
Usage: $(basename "$0") <ssh-host> [options]

Bring up a HaLOS Pi for Lume TI, configure services, and run the gap 4 benchmark.

Arguments:
  <ssh-host>          Remote SSH destination (e.g. pi@halos.local, halos.local)

Options:
  --dry-run           Print all remote commands without executing them
  --lume-bin <path>   Path to arm64 lume binary passed to provision-pi.sh
  --bench-bin <path>  Path to arm64 ti-query-bench binary
  --bench-store <dir> Path to local benchmark store root (builds on host if <dir>/store is missing)
  --debs <dir>        Directory containing HaLOS app .deb packages to install
  --skip-bench        Skip the gap 4 query benchmark
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
        --bench-bin)
            if [ $# -lt 2 ]; then
                echo "Error: --bench-bin requires a path argument" >&2
                exit 1
            fi
            BENCH_BIN="$2"
            shift 2
            ;;
        --bench-store)
            if [ $# -lt 2 ]; then
                echo "Error: --bench-store requires a directory argument" >&2
                exit 1
            fi
            BENCH_STORE="$2"
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
        --skip-bench)
            SKIP_BENCH=true
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

if [ -n "$BENCH_BIN" ] && [ ! -f "$BENCH_BIN" ]; then
    echo "Error: Specified --bench-bin not found at $BENCH_BIN" >&2
    exit 1
fi

if [ -n "$DEBS_DIR" ] && [ ! -d "$DEBS_DIR" ]; then
    echo "Error: Specified --debs directory not found at $DEBS_DIR" >&2
    exit 1
fi

if [ "$SKIP_BENCH" = false ] && [ -z "$BENCH_BIN" ]; then
    if [ -f "${REPO_ROOT}/bin/linux-arm64/ti-query-bench" ]; then
        BENCH_BIN="${REPO_ROOT}/bin/linux-arm64/ti-query-bench"
    elif [ -f "${REPO_ROOT}/target/aarch64-unknown-linux-gnu/release/ti-query-bench" ]; then
        BENCH_BIN="${REPO_ROOT}/target/aarch64-unknown-linux-gnu/release/ti-query-bench"
    elif [ "$DRY_RUN" = false ]; then
        echo "Error: --bench-bin required when benchmark is enabled (or pass --skip-bench)" >&2
        exit 1
    fi
fi

SSH_CMD=(ssh)
SCP_CMD=(scp)
if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
    SSH_CMD=(ssh -F "$LUME_DEPLOY_SSH_CONFIG")
    SCP_CMD=(scp -F "$LUME_DEPLOY_SSH_CONFIG")
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

echo "=== HaLOS Pi Bringup ==="
echo "Target: ${SSH_HOST}"
[ -n "$LUME_BIN" ] && echo "Lume binary: ${LUME_BIN}"
[ -n "$BENCH_BIN" ] && echo "Bench binary: ${BENCH_BIN}"
[ -n "$BENCH_STORE" ] && echo "Bench store: ${BENCH_STORE}"
[ -n "$DEBS_DIR" ] && echo "HaLOS .debs directory: ${DEBS_DIR}"
echo "Skip benchmark: ${SKIP_BENCH}"
[ "$DRY_RUN" = true ] && echo "Mode: DRY RUN (no commands will be executed)"
echo

compute_store_marker() {
    local dir="$1"
    if [ ! -d "$dir" ]; then
        echo ""
        return
    fi
    (
        cd "$dir" || exit 1
        find . -type f ! -name '.store_marker' | sort | while IFS= read -r f; do
            wc -c < "$f"
            echo "$f"
        done | if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi | awk '{print $1}'
    )
}

# -----------------------------------------------------------------------------
# Step 1: Provision Pi
# -----------------------------------------------------------------------------
echo "=== Step 1: Provision Pi (provision-pi.sh) ==="
PROVISION_ARGS=("$SSH_HOST")
[ "$DRY_RUN" = true ] && PROVISION_ARGS+=(--dry-run)
[ -n "$LUME_BIN" ] && PROVISION_ARGS+=(--lume-bin "$LUME_BIN")
[ -n "$DEBS_DIR" ] && PROVISION_ARGS+=(--debs "$DEBS_DIR")

bash "$PROVISION_SCRIPT" "${PROVISION_ARGS[@]}"
echo

# -----------------------------------------------------------------------------
# Step 2: Retire Ollama
# -----------------------------------------------------------------------------
echo "=== Step 2: Retire Ollama Container App (pi-retire-ollama.sh) ==="
RETIRE_ARGS=("$SSH_HOST")
[ "$DRY_RUN" = true ] && RETIRE_ARGS+=(--dry-run)

bash "$RETIRE_SCRIPT" "${RETIRE_ARGS[@]}"
echo

# -----------------------------------------------------------------------------
# Step 3: Pull Grub Crawler Image, Restart, and Poll Health
# -----------------------------------------------------------------------------
echo "=== Step 3: Pull Grub Crawler image and verify health ==="
if [ "$DRY_RUN" = true ]; then
    run_remote "sudo -n docker pull deepbluedynamics/grubcrawler:latest-lite"
    run_remote "sudo -n systemctl restart marine-grubcrawler-container"
    run_remote "for i in \$(seq 1 30); do if curl -fsS -m 3 http://127.0.0.1:6792/health >/dev/null 2>&1 || curl -fsS -m 3 http://127.0.0.1:6792/ >/dev/null 2>&1; then echo 'Grub is healthy'; exit 0; fi; sleep 2; done; echo 'Grub health check failed' >&2; exit 1"
else
    echo "Pulling deepbluedynamics/grubcrawler:latest-lite..."
    run_remote "sudo -n docker pull deepbluedynamics/grubcrawler:latest-lite"
    echo "Restarting marine-grubcrawler-container..."
    run_remote "sudo -n systemctl restart marine-grubcrawler-container"
    echo "Waiting for Grub health check on 127.0.0.1:6792..."
    # shellcheck disable=SC2016
    run_remote 'for i in $(seq 1 30); do if curl -fsS -m 3 http://127.0.0.1:6792/health >/dev/null 2>&1 || curl -fsS -m 3 http://127.0.0.1:6792/ >/dev/null 2>&1; then echo "Grub is healthy"; exit 0; fi; sleep 2; done; echo "Grub health check failed" >&2; exit 1'
fi
echo

# -----------------------------------------------------------------------------
# Step 4: Gap 4 Query Benchmark
# -----------------------------------------------------------------------------
echo "=== Step 4: Gap 4 Query Benchmark ==="
if [ "$SKIP_BENCH" = true ]; then
    echo "[SKIP] Benchmark skipped via --skip-bench."
else
    BENCH_ROOT="${LUME_PI_BENCH_ROOT:-/var/tmp/lume-pi-bench}"
    GIT_SHA="$(git -C "$REPO_ROOT" rev-parse --short=7 HEAD 2>/dev/null || echo "unknown")"
    BENCH_STORE_DIR="${BENCH_STORE:-${REPO_ROOT}/target/pi-bench}"

    if [ ! -d "${BENCH_STORE_DIR}/store" ]; then
        if [ "$DRY_RUN" = true ]; then
            echo "1-vessel store missing at ${BENCH_STORE_DIR}/store; would build on host..."
            echo "[dry-run] CARGO_INCREMENTAL=0 bash ${BENCH_RECIPE_SCRIPT} store ${BENCH_STORE_DIR}"
        else
            echo "Building 1-vessel benchmark store on host with pi-bench-recipe.sh..."
            CARGO_INCREMENTAL=0 bash "${BENCH_RECIPE_SCRIPT}" store "${BENCH_STORE_DIR}"
        fi
    fi

    if [ "$DRY_RUN" = true ]; then
        echo "Staging benchmark binary, 1-vessel store, and golden corpus..."
        run_remote "mkdir -p '${BENCH_ROOT}/results'"
        if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
            echo "[dry-run] scp -F ${LUME_DEPLOY_SSH_CONFIG} ${BENCH_BIN:-<bench-bin>} ${SSH_HOST}:${BENCH_ROOT}/ti-query-bench-aarch64"
            echo "[dry-run] scp -F ${LUME_DEPLOY_SSH_CONFIG} ${REPO_ROOT}/tests/golden/corpus.json ${SSH_HOST}:${BENCH_ROOT}/corpus.json"
        else
            echo "[dry-run] scp ${BENCH_BIN:-<bench-bin>} ${SSH_HOST}:${BENCH_ROOT}/ti-query-bench-aarch64"
            echo "[dry-run] scp ${REPO_ROOT}/tests/golden/corpus.json ${SSH_HOST}:${BENCH_ROOT}/corpus.json"
        fi
        run_remote "chmod +x '${BENCH_ROOT}/ti-query-bench-aarch64'"

        if command -v rsync >/dev/null 2>&1; then
            if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
                echo "[dry-run] rsync -az -e 'ssh -F ${LUME_DEPLOY_SSH_CONFIG}' ${BENCH_STORE_DIR}/store ${SSH_HOST}:${BENCH_ROOT}/"
            else
                echo "[dry-run] rsync -az ${BENCH_STORE_DIR}/store ${SSH_HOST}:${BENCH_ROOT}/"
            fi
        else
            if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
                echo "[dry-run] scp -F ${LUME_DEPLOY_SSH_CONFIG} -r ${BENCH_STORE_DIR}/store ${SSH_HOST}:${BENCH_ROOT}/"
            else
                echo "[dry-run] scp -r ${BENCH_STORE_DIR}/store ${SSH_HOST}:${BENCH_ROOT}/"
            fi
        fi
        run_remote "echo '<marker>' > '${BENCH_ROOT}/store/.store_marker'"

        run_remote "cd '${BENCH_ROOT}' && ./ti-query-bench-aarch64 bench --store '${BENCH_ROOT}/store' --parquet '${BENCH_ROOT}/store' --corpus '${BENCH_ROOT}/corpus.json' --out-dir '${BENCH_ROOT}/results' --iterations 7 --cache-bytes 268435456 --sha '${GIT_SHA}' --pi"
        if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
            echo "[dry-run] scp -F ${LUME_DEPLOY_SSH_CONFIG} \"${SSH_HOST}:${BENCH_ROOT}/results/*-pi-*.json\" ${REPO_ROOT}/bench/results/"
        else
            echo "[dry-run] scp \"${SSH_HOST}:${BENCH_ROOT}/results/*-pi-*.json\" ${REPO_ROOT}/bench/results/"
        fi
    else
        echo "Creating remote benchmark directory ${BENCH_ROOT}..."
        run_remote "mkdir -p '${BENCH_ROOT}/results'"

        echo "Transferring benchmark binary and corpus..."
        "${SCP_CMD[@]}" "$BENCH_BIN" "${SSH_HOST}:${BENCH_ROOT}/ti-query-bench-aarch64"
        "${SCP_CMD[@]}" "${REPO_ROOT}/tests/golden/corpus.json" "${SSH_HOST}:${BENCH_ROOT}/corpus.json"
        run_remote "chmod +x '${BENCH_ROOT}/ti-query-bench-aarch64'"

        STORE_MARKER=$(compute_store_marker "${BENCH_STORE_DIR}/store" | tr -d ' \r\n')
        REMOTE_MARKER=$("${SSH_CMD[@]}" "$SSH_HOST" "cat '${BENCH_ROOT}/store/.store_marker' 2>/dev/null || true" | tr -d ' \r\n')

        if [ -n "$STORE_MARKER" ] && [ "$REMOTE_MARKER" = "$STORE_MARKER" ]; then
            echo "[SKIP] Remote store matches local store marker (${STORE_MARKER:0:12})."
        else
            echo "Transferring 1-vessel benchmark store to ${SSH_HOST}:${BENCH_ROOT}/store..."
            if command -v rsync >/dev/null 2>&1; then
                if [ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ]; then
                    rsync -az -e "ssh -F '${LUME_DEPLOY_SSH_CONFIG}'" "${BENCH_STORE_DIR}/store" "${SSH_HOST}:${BENCH_ROOT}/"
                else
                    rsync -az "${BENCH_STORE_DIR}/store" "${SSH_HOST}:${BENCH_ROOT}/"
                fi
            else
                # scp -r into an existing store/ would nest it as store/store; start clean.
                run_remote "rm -rf '${BENCH_ROOT}/store'"
                "${SCP_CMD[@]}" -r "${BENCH_STORE_DIR}/store" "${SSH_HOST}:${BENCH_ROOT}/"
            fi
            run_remote "echo '${STORE_MARKER}' > '${BENCH_ROOT}/store/.store_marker'"
            echo "Benchmark store transferred and marker written."
        fi

        echo "Running query benchmark on the Pi (--pi, sha: ${GIT_SHA})..."
        run_remote "cd '${BENCH_ROOT}' && ./ti-query-bench-aarch64 bench --store '${BENCH_ROOT}/store' --parquet '${BENCH_ROOT}/store' --corpus '${BENCH_ROOT}/corpus.json' --out-dir '${BENCH_ROOT}/results' --iterations 7 --cache-bytes 268435456 --sha '${GIT_SHA}' --pi"

        echo "Retrieving benchmark results from Pi..."
        mkdir -p "${REPO_ROOT}/bench/results"
        "${SCP_CMD[@]}" "${SSH_HOST}:${BENCH_ROOT}/results/*-pi-*.json" "${REPO_ROOT}/bench/results/"
        echo "Benchmark results saved to bench/results/."
    fi
fi
echo

# -----------------------------------------------------------------------------
# Step 5: System Summary
# -----------------------------------------------------------------------------
echo "=== Step 5: System Summary ==="
if [ "$DRY_RUN" = true ]; then
    run_remote "echo '--- Filesystem Usage (/) ---'; df -h /"
    run_remote "echo '--- HaLOS Services State ---'; for s in marine-signalk-server-container marine-grubcrawler-container marine-ollama-container; do printf '%-35s %s\n' \"\$s:\" \"\$(sudo -n systemctl is-active \"\$s\" 2>/dev/null || echo 'inactive')\"; done"
    run_remote "echo '--- Newest telemetry_lume Timestamp ---'; ts=\$(curl -s -m 5 -X POST http://127.0.0.1:5863/ti/query -H 'Accept: application/json' -H 'Content-Type: application/json' -d '{\"sql\":\"SELECT max(ts) AS latest_ts FROM telemetry_lume\"}' 2>/dev/null || true); if [ -n \"\$ts\" ]; then echo \"\$ts\"; else echo 'telemetry_lume query unavailable'; fi"
else
    echo "--- Filesystem Usage (/) ---"
    run_remote "df -h /"
    echo
    echo "--- HaLOS Services State ---"
    # shellcheck disable=SC2016
    run_remote 'for s in marine-signalk-server-container marine-grubcrawler-container marine-ollama-container; do printf "%-35s %s\n" "$s:" "$(sudo -n systemctl is-active "$s" 2>/dev/null || echo "inactive")"; done'
    echo
    echo "--- Newest telemetry_lume Timestamp ---"
    # shellcheck disable=SC2016
    run_remote 'ts=$(curl -s -m 5 -X POST http://127.0.0.1:5863/ti/query -H "Accept: application/json" -H "Content-Type: application/json" -d '\''{"sql":"SELECT max(ts) AS latest_ts FROM telemetry_lume"}'\'' 2>/dev/null || true); if [ -n "$ts" ]; then echo "$ts"; else echo "telemetry_lume query unavailable (server not running or no data yet)"; fi'
fi
echo

echo "=== HaLOS Pi Bringup Complete ==="

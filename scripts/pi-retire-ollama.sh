#!/bin/bash
# Retire Ollama container app on the HaLOS Pi and reclaim disk space.
#
# Usage:
#   scripts/pi-retire-ollama.sh <ssh-host> [--dry-run]
set -euo pipefail

SSH_HOST=""
DRY_RUN=false

print_usage() {
    cat << EOF
Usage: $(basename "$0") <ssh-host> [options]

Stop and disable marine-ollama-container, remove the 4.2 GB Docker image,
keep the persistent data directory, and print disk usage before and after.

Arguments:
  <ssh-host>          Remote SSH destination (e.g. pi@halos.local, halos.local)

Options:
  --dry-run           Print all remote commands without executing them
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

run_remote() {
    local cmd="$1"
    if [ "$DRY_RUN" = true ]; then
        echo "[dry-run] ssh ${SSH_HOST} '${cmd}'"
    else
        ssh "$SSH_HOST" "$cmd"
    fi
}

echo "=== Retire Ollama Container App on Pi ==="
echo "Target: ${SSH_HOST}"
[ "$DRY_RUN" = true ] && echo "Mode: DRY RUN (no commands will be executed)"
echo

if [ "$DRY_RUN" = true ]; then
    echo "--- Remote Commands ---"
    run_remote "echo '>>> Disk space before retirement:'; df -h /"
    run_remote "echo '>>> Stopping marine-ollama-container...'; sudo -n systemctl stop marine-ollama-container"
    run_remote "echo '>>> Disabling marine-ollama-container...'; sudo -n systemctl disable marine-ollama-container"
    run_remote "echo '>>> Removing docker image ollama/ollama...'; sudo -n docker image rm ollama/ollama || true"
    run_remote "echo '>>> Preserving persistent data directory at:'; sudo -n ls -ld /var/lib/container-apps/marine-ollama-container/data 2>/dev/null || true"
    run_remote "echo '>>> Disk space after retirement:'; df -h /"
    echo
    echo "Dry run complete."
    exit 0
fi

echo "--- Disk space before retirement ---"
run_remote "df -h /"
echo

echo "Stopping and disabling marine-ollama-container..."
run_remote "sudo -n systemctl stop marine-ollama-container"
run_remote "sudo -n systemctl disable marine-ollama-container"
echo

echo "Removing Docker image ollama/ollama..."
run_remote "sudo -n docker image rm ollama/ollama || true"
echo

echo "Preserving persistent data directory..."
run_remote "sudo -n ls -ld /var/lib/container-apps/marine-ollama-container/data 2>/dev/null || true"
echo

echo "--- Disk space after retirement ---"
run_remote "df -h /"
echo

echo "Ollama retirement complete. Data directory preserved."

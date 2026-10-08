#!/usr/bin/env bash
# Integration test for scripts/deploy-pi.sh, scripts/pi-retire-ollama.sh,
# and scripts/provision-pi.sh.
# Tests deployment, shims, dry-run, idempotency, and permissions against
# a throwaway local sshd container.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
DEPLOY_SCRIPT="${REPO_ROOT}/scripts/deploy-pi.sh"
RETIRE_SCRIPT="${REPO_ROOT}/scripts/pi-retire-ollama.sh"
PROVISION_SCRIPT="${REPO_ROOT}/scripts/provision-pi.sh"

echo "=== Pi Deployment, Retirement & Provisioning Integration Test ==="

# 1. Dependency checks
command -v ssh >/dev/null 2>&1 || { echo "Error: ssh client required" >&2; exit 1; }
command -v ssh-keygen >/dev/null 2>&1 || { echo "Error: ssh-keygen required" >&2; exit 1; }

# Discover docker host if inside container without mounted socket
if [ -z "${DOCKER_HOST:-}" ] && [ ! -e /var/run/docker.sock ] && curl -s -m 1 http://host.docker.internal:2375/version >/dev/null 2>&1; then
    export DOCKER_HOST="tcp://host.docker.internal:2375"
fi

command -v docker >/dev/null 2>&1 || { echo "Error: docker required" >&2; exit 1; }
docker info >/dev/null 2>&1 || { echo "Error: docker daemon unreachable" >&2; exit 1; }

# Record initial ~/.ssh/config state (test must never modify or create it)
USER_SSH_CONFIG="${HOME}/.ssh/config"
ORIG_SSH_CONFIG_EXISTS=false
INITIAL_SSH_CONFIG_CKSUM=""
if [ -f "$USER_SSH_CONFIG" ]; then
    ORIG_SSH_CONFIG_EXISTS=true
    INITIAL_SSH_CONFIG_CKSUM="$(sha256sum "$USER_SSH_CONFIG" | awk '{print $1}')"
fi

# 2. Test identifiers and temporary workspace
TEST_ID="deploy-test-$(date +%s)-$$"
IMAGE_NAME="${TEST_ID}-img"
CONTAINER_NAME="${TEST_ID}-c"
SSH_HOST_ALIAS="${TEST_ID}-host"
TMP_DIR="$(mktemp -d -t deploy-test.XXXXXX)"
PROXY_PID=""

cleanup() {
    local exit_code=$?
    echo
    echo "--- Tearing down test resources ---"
    if [ -n "${PROXY_PID:-}" ]; then
        kill "$PROXY_PID" >/dev/null 2>&1 || true
    fi
    if [ -n "${CONTAINER_NAME:-}" ]; then
        docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
    fi
    if [ -n "${IMAGE_NAME:-}" ]; then
        docker rmi -f "$IMAGE_NAME" >/dev/null 2>&1 || true
    fi
    if [ -n "${TMP_DIR:-}" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi

    # Assert user's ~/.ssh/config was never modified or created
    if [ "$ORIG_SSH_CONFIG_EXISTS" = true ]; then
        if [ ! -f "$USER_SSH_CONFIG" ]; then
            echo "FAIL: ~/.ssh/config was deleted during test run" >&2
            exit_code=1
        else
            FINAL_CKSUM="$(sha256sum "$USER_SSH_CONFIG" | awk '{print $1}')"
            if [ "$INITIAL_SSH_CONFIG_CKSUM" != "$FINAL_CKSUM" ]; then
                echo "FAIL: ~/.ssh/config checksum changed ($INITIAL_SSH_CONFIG_CKSUM vs $FINAL_CKSUM)" >&2
                exit_code=1
            fi
        fi
    else
        if [ -f "$USER_SSH_CONFIG" ]; then
            echo "FAIL: ~/.ssh/config was created during test run" >&2
            exit_code=1
        fi
    fi

    if [ $exit_code -eq 0 ]; then
        echo "=== ALL INTEGRATION TESTS PASSED ==="
    else
        echo "=== INTEGRATION TESTS FAILED (code: $exit_code) ==="
    fi
    exit $exit_code
}
trap cleanup EXIT INT TERM

# 3. Generate throwaway SSH key pair
SSH_KEY="${TMP_DIR}/id_ed25519"
ssh-keygen -t ed25519 -N "" -f "$SSH_KEY" -C "$TEST_ID" >/dev/null 2>&1
chmod 600 "$SSH_KEY"

# 4. Prepare container build context
CONTEXT_DIR="${TMP_DIR}/context"
mkdir -p "$CONTEXT_DIR"
cp "${SSH_KEY}.pub" "${CONTEXT_DIR}/authorized_keys"

cat << 'EOF' > "${CONTEXT_DIR}/systemctl_shim.sh"
#!/bin/sh
LOGFILE="/var/log/shim_calls.log"
echo "systemctl $*" >> "$LOGFILE"
for arg in "$@"; do
    if [ "$arg" = "is-active" ]; then
        exit 0
    fi
done
exit 0
EOF

cat << 'EOF' > "${CONTEXT_DIR}/docker_shim.sh"
#!/bin/sh
LOGFILE="/var/log/shim_calls.log"
echo "docker $*" >> "$LOGFILE"
exit 0
EOF

cat << 'EOF' > "${CONTEXT_DIR}/apt_get_shim.sh"
#!/bin/sh
LOGFILE="/var/log/shim_calls.log"
echo "apt-get $*" >> "$LOGFILE"
mkdir -p /var/lib/fake-dpkg
for arg in "$@"; do
    case "$arg" in
        *.deb)
            pkg=""
            if [ -x /usr/bin/dpkg-deb ] && [ -f "$arg" ]; then
                pkg=$(/usr/bin/dpkg-deb -f "$arg" Package 2>/dev/null || true)
            fi
            if [ -z "$pkg" ]; then
                pkg=$(basename "$arg" | sed -E 's/_.*|\.deb//')
            fi
            if [ -n "$pkg" ]; then
                echo "$pkg" >> /var/lib/fake-dpkg/installed.list
            fi
            ;;
    esac
done
exit 0
EOF

cat << 'EOF' > "${CONTEXT_DIR}/dpkg_shim.sh"
#!/bin/sh
LOGFILE="/var/log/shim_calls.log"
echo "dpkg $*" >> "$LOGFILE"
if [ "$1" = "-s" ] || [ "$1" = "--status" ]; then
    pkg="$2"
    if [ -f /var/lib/fake-dpkg/installed.list ] && grep -qx "$pkg" /var/lib/fake-dpkg/installed.list; then
        echo "Package: $pkg"
        echo "Status: install ok installed"
        exit 0
    fi
fi
if [ -x /usr/bin/dpkg ]; then
    exec /usr/bin/dpkg "$@"
fi
exit 1
EOF

cat << 'EOF' > "${CONTEXT_DIR}/dummy_signalk.py"
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler

class HealthHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/signalk" or self.path.startswith("/signalk"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}\n')
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        pass

if __name__ == "__main__":
    server = HTTPServer(("127.0.0.1", 3000), HealthHandler)
    server.serve_forever()
EOF

cat << 'EOF' > "${CONTEXT_DIR}/entrypoint.sh"
#!/bin/sh
mkdir -p /run/sshd /var/run/sshd
python3 /usr/local/bin/dummy_signalk.py >/dev/null 2>&1 &
exec /usr/sbin/sshd -D -e
EOF

cat << 'EOF' > "${CONTEXT_DIR}/Dockerfile"
FROM debian:bookworm-slim

RUN apt-get update -qq && apt-get install -y -qq \
    openssh-server \
    sudo \
    curl \
    python3 \
    procps \
    rsync \
    dpkg-dev \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /var/run/sshd /etc/ssh

# Host keys
RUN ssh-keygen -A

# Create passwordless sudo user matching Signal K uid 1000
RUN groupadd -g 1000 testpi 2>/dev/null || true && \
    useradd -u 1000 -g 1000 -m -s /bin/bash testpi 2>/dev/null || true && \
    echo "testpi ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/testpi && \
    echo 'Defaults secure_path="/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin"' >> /etc/sudoers.d/testpi && \
    chmod 0440 /etc/sudoers.d/testpi

# Configure throwaway public key
COPY authorized_keys /home/testpi/.ssh/authorized_keys
RUN chown -R testpi:testpi /home/testpi/.ssh && \
    chmod 700 /home/testpi/.ssh && \
    chmod 600 /home/testpi/.ssh/authorized_keys

# Fake systemctl, docker, apt-get and dpkg shims
COPY systemctl_shim.sh /usr/local/bin/systemctl
COPY docker_shim.sh /usr/local/bin/docker
COPY apt_get_shim.sh /usr/local/bin/apt-get
COPY dpkg_shim.sh /usr/local/bin/dpkg
RUN chmod 755 /usr/local/bin/systemctl /usr/local/bin/docker /usr/local/bin/apt-get /usr/local/bin/dpkg

RUN touch /var/log/shim_calls.log && chmod 666 /var/log/shim_calls.log

# Dummy Signal K server script
COPY dummy_signalk.py /usr/local/bin/dummy_signalk.py
RUN chmod 755 /usr/local/bin/dummy_signalk.py

COPY entrypoint.sh /entrypoint.sh
RUN chmod 755 /entrypoint.sh

EXPOSE 22
CMD ["/entrypoint.sh"]
EOF

echo "Building throwaway Debian sshd image: ${IMAGE_NAME}..."
docker build -t "$IMAGE_NAME" "$CONTEXT_DIR" >/dev/null

echo "Starting container published on 127.0.0.1: ${CONTAINER_NAME}..."
docker run -d --name "$CONTAINER_NAME" -p 127.0.0.1::22 "$IMAGE_NAME" >/dev/null

SSH_PORT=$(docker port "$CONTAINER_NAME" 22 | head -n 1 | sed 's/.*://')
echo "Published on port: ${SSH_PORT}"

# If running inside a containerized runner where 127.0.0.1:PORT routes to the container
# rather than the Docker host, forward 127.0.0.1:PORT to host.docker.internal:PORT
# so HostName 127.0.0.1 works seamlessly and identically everywhere.
if ! timeout 1 bash -c "cat < /dev/null > /dev/tcp/127.0.0.1/${SSH_PORT}" 2>/dev/null; then
    if timeout 1 bash -c "cat < /dev/null > /dev/tcp/host.docker.internal/${SSH_PORT}" 2>/dev/null; then
        python3 -c "
import socket, threading, sys
def forward(src, dst):
    while True:
        try:
            d = src.recv(4096)
            if not d: break
            dst.sendall(d)
        except: break
    src.close(); dst.close()
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(('127.0.0.1', ${SSH_PORT}))
srv.listen(5)
while True:
    c, _ = srv.accept()
    r = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    r.connect(('host.docker.internal', ${SSH_PORT}))
    threading.Thread(target=forward, args=(c, r), daemon=True).start()
    threading.Thread(target=forward, args=(r, c), daemon=True).start()
" &
        PROXY_PID=$!
        sleep 0.5
    fi
fi

# 5. Write isolated SSH config to TMP_DIR/ssh_config and export LUME_DEPLOY_SSH_CONFIG
SSH_CONFIG_FILE="${TMP_DIR}/ssh_config"
cat << EOF > "$SSH_CONFIG_FILE"
Host ${SSH_HOST_ALIAS}
    HostName 127.0.0.1
    Port ${SSH_PORT}
    User testpi
    IdentityFile ${SSH_KEY}
    IdentitiesOnly yes
    StrictHostKeyChecking no
    UserKnownHostsFile ${TMP_DIR}/known_hosts
    LogLevel ERROR
EOF
chmod 600 "$SSH_CONFIG_FILE"

export LUME_DEPLOY_SSH_CONFIG="$SSH_CONFIG_FILE"
echo "Exported LUME_DEPLOY_SSH_CONFIG=${LUME_DEPLOY_SSH_CONFIG}"

echo "Waiting for SSH to become ready..."
SSH_READY=false
for i in $(seq 1 30); do
    if ssh -F "$LUME_DEPLOY_SSH_CONFIG" -q "$SSH_HOST_ALIAS" "echo ssh_ready" >/dev/null 2>&1; then
        SSH_READY=true
        echo "SSH is ready (attempt $i/30)."
        break
    fi
    sleep 1
done

if [ "$SSH_READY" != true ]; then
    echo "Error: Timed out waiting for SSH connection." >&2
    exit 1
fi

# Prepare dummy arm64 lume binary (intentionally 0644 to test chmod 755)
DUMMY_LUME="${TMP_DIR}/lume-arm64-dummy"
cat << 'EOF' > "$DUMMY_LUME"
#!/bin/sh
echo "lume dummy arm64 v1.0.0"
EOF
chmod 644 "$DUMMY_LUME"

# --------------------------------------------------------------------------
# Test 1: deploy-pi.sh --dry-run touches nothing
# --------------------------------------------------------------------------
echo
echo "=== Test 1: deploy-pi.sh --dry-run touches nothing ==="
DRY_RUN_OUTPUT=$(bash "$DEPLOY_SCRIPT" "$SSH_HOST_ALIAS" --dry-run --lume-bin "$DUMMY_LUME")
echo "$DRY_RUN_OUTPUT" | grep -q "Mode: DRY RUN" || { echo "FAIL: dry-run mode notice missing" >&2; exit 1; }

# Destination must not exist
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -e /var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti"; then
    echo "FAIL: Remote destination was created during deploy --dry-run" >&2
    exit 1
fi

# Stage must not exist
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -e /tmp/signalk-lume-ti-stage"; then
    echo "FAIL: Staging directory was left behind during deploy --dry-run" >&2
    exit 1
fi

# No shim calls logged
SHIM_CALLS=$(ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "wc -l < /var/log/shim_calls.log | tr -d ' '")
if [ "$SHIM_CALLS" -ne 0 ]; then
    echo "FAIL: Shim calls were made during deploy --dry-run ($SHIM_CALLS logged)" >&2
    exit 1
fi
echo "PASS: deploy-pi.sh --dry-run touched nothing."

# --------------------------------------------------------------------------
# Test 2: pi-retire-ollama.sh --dry-run touches nothing
# --------------------------------------------------------------------------
echo
echo "=== Test 2: pi-retire-ollama.sh --dry-run touches nothing ==="
# Pre-create Ollama data dir and mock model
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "sudo -n mkdir -p /var/lib/container-apps/marine-ollama-container/data/ollama/models && echo 'weights' | sudo -n tee /var/lib/container-apps/marine-ollama-container/data/ollama/models/test-model.bin >/dev/null"

RETIRE_DRY_RUN=$(bash "$RETIRE_SCRIPT" "$SSH_HOST_ALIAS" --dry-run)
echo "$RETIRE_DRY_RUN" | grep -q "Mode: DRY RUN" || { echo "FAIL: retire dry-run mode notice missing" >&2; exit 1; }

# No shim calls logged
SHIM_CALLS=$(ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "wc -l < /var/log/shim_calls.log | tr -d ' '")
if [ "$SHIM_CALLS" -ne 0 ]; then
    echo "FAIL: Shim calls were made during retire --dry-run ($SHIM_CALLS logged)" >&2
    exit 1
fi

# Ollama data must still exist
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -f /var/lib/container-apps/marine-ollama-container/data/ollama/models/test-model.bin"; then
    echo "FAIL: Ollama data was modified during retire --dry-run" >&2
    exit 1
fi
echo "PASS: pi-retire-ollama.sh --dry-run touched nothing."

# --------------------------------------------------------------------------
# Test 3: Real deploy-pi.sh execution (files land, lume mode 755, restart called)
# --------------------------------------------------------------------------
echo
echo "=== Test 3: Real deploy-pi.sh execution ==="
bash "$DEPLOY_SCRIPT" "$SSH_HOST_ALIAS" --lume-bin "$DUMMY_LUME"

# Assert files landed
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -f /var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti/package.json" || {
    echo "FAIL: package.json missing in remote destination" >&2; exit 1;
}
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -f /var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti/index.js" || {
    echo "FAIL: index.js missing in remote destination" >&2; exit 1;
}
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -d /var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti/lib" || {
    echo "FAIL: lib directory missing in remote destination" >&2; exit 1;
}

# Assert test directory excluded
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -e /var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti/test"; then
    echo "FAIL: test directory was copied but should have been excluded" >&2
    exit 1
fi

# Assert lume binary exists and has mode 755
LUME_PATH="/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti/bin/linux-arm64/lume"
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -f '$LUME_PATH'" || {
    echo "FAIL: lume binary missing at $LUME_PATH" >&2; exit 1;
}
LUME_MODE=$(ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "stat -c %a '$LUME_PATH'")
if [ "$LUME_MODE" != "755" ]; then
    echo "FAIL: Expected lume binary permissions 755, got '$LUME_MODE'" >&2
    exit 1
fi
echo "Verified: lume binary landed with mode 755."

# Assert restart call was made
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -q 'systemctl restart marine-signalk-server-container' /var/log/shim_calls.log"; then
    echo "FAIL: systemctl restart marine-signalk-server-container was not called" >&2
    exit 1
fi
echo "Verified: marine-signalk-server-container restart was called."
echo "PASS: deploy-pi.sh deployed files, set mode 755, and restarted container."

# --------------------------------------------------------------------------
# Test 4: Real pi-retire-ollama.sh execution (image-rm made, data dir kept)
# --------------------------------------------------------------------------
echo
echo "=== Test 4: Real pi-retire-ollama.sh execution ==="
# Reset shim calls log before retirement
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "sudo -n truncate -s 0 /var/log/shim_calls.log"

bash "$RETIRE_SCRIPT" "$SSH_HOST_ALIAS"

# Assert systemctl stop call
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -q 'systemctl stop marine-ollama-container' /var/log/shim_calls.log"; then
    echo "FAIL: systemctl stop marine-ollama-container was not called" >&2
    exit 1
fi

# Assert systemctl disable call
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -q 'systemctl disable marine-ollama-container' /var/log/shim_calls.log"; then
    echo "FAIL: systemctl disable marine-ollama-container was not called" >&2
    exit 1
fi

# Assert docker image rm call
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -q 'docker image rm ollama/ollama' /var/log/shim_calls.log"; then
    echo "FAIL: docker image rm ollama/ollama was not called" >&2
    exit 1
fi
echo "Verified: systemctl stop/disable and docker image rm calls were made."

# Assert Ollama data directory was kept
OLLAMA_MODEL_FILE="/var/lib/container-apps/marine-ollama-container/data/ollama/models/test-model.bin"
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -f '$OLLAMA_MODEL_FILE'"; then
    echo "FAIL: Ollama data directory was deleted or modified" >&2
    exit 1
fi
echo "Verified: Ollama data directory was preserved."
echo "PASS: pi-retire-ollama.sh stopped, disabled, removed image, and preserved data."

# --------------------------------------------------------------------------
# Setup for Provisioning Tests (faked /proc/cmdline, cmdline.txt, UUID, debs)
# --------------------------------------------------------------------------
echo
echo "=== Preparing Environment for provision-pi.sh Integration Tests ==="
# 1. Fake /proc/cmdline and cmdline.txt
FAKE_BOOT_CONTENT="console=serial0,115200 console=tty1 root=PARTUUID=1234-02 rootfstype=ext4 fsck.repair=yes rootwait"
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "
    echo '${FAKE_BOOT_CONTENT}' | sudo -n tee /tmp/fake_proc_cmdline >/dev/null && \
    echo '${FAKE_BOOT_CONTENT}' | sudo -n tee /tmp/fake_boot_cmdline.txt >/dev/null && \
    sudo -n chmod 666 /tmp/fake_proc_cmdline /tmp/fake_boot_cmdline.txt && \
    sudo -n rm -f /tmp/fake_boot_cmdline.txt.bak-pre-memcg
"
export LUME_PROC_CMDLINE="/tmp/fake_proc_cmdline"
export LUME_BOOT_CMDLINE="/tmp/fake_boot_cmdline.txt"
echo "Configured faked cmdline paths: LUME_PROC_CMDLINE=${LUME_PROC_CMDLINE}, LUME_BOOT_CMDLINE=${LUME_BOOT_CMDLINE}"

# 2. Pin Signal K self vessel UUID in baseDeltas.json
TEST_UUID="urn:mrn:signalk:uuid:0eb191d0-1f5a-42da-979e-ead792d676ee"
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "
    sudo -n mkdir -p /var/lib/container-apps/marine-signalk-server-container/data/data && \
    cat << 'JSON' | sudo -n tee /var/lib/container-apps/marine-signalk-server-container/data/data/baseDeltas.json >/dev/null
{
  \"updates\": [
    {
      \"values\": [
        {
          \"path\": \"vessels.self\",
          \"value\": \"${TEST_UUID}\"
        }
      ]
    }
  ]
}
JSON
"

# 3. Create test HaLOS .deb packages (marine-grubcrawler-container and marine-ollama-container)
TEST_DEBS_DIR="${TMP_DIR}/test_debs"
mkdir -p "${TEST_DEBS_DIR}/pkg-grub/DEBIAN" "${TEST_DEBS_DIR}/pkg-ollama/DEBIAN"
cat << 'EOF' > "${TEST_DEBS_DIR}/pkg-grub/DEBIAN/control"
Package: marine-grubcrawler-container
Version: 0.16.1-1
Architecture: all
Maintainer: test <test@example.com>
Description: fake grubcrawler container
EOF
dpkg-deb -b "${TEST_DEBS_DIR}/pkg-grub" "${TEST_DEBS_DIR}/marine-grubcrawler-container_0.16.1-1_arm64.deb" >/dev/null

cat << 'EOF' > "${TEST_DEBS_DIR}/pkg-ollama/DEBIAN/control"
Package: marine-ollama-container
Version: 0.1.0-1
Architecture: all
Maintainer: test <test@example.com>
Description: fake ollama container
EOF
dpkg-deb -b "${TEST_DEBS_DIR}/pkg-ollama" "${TEST_DEBS_DIR}/marine-ollama-container_0.1.0-1_arm64.deb" >/dev/null

# Clean remote destination and state markers before provision tests
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "
    sudo -n rm -rf /var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti \
                   /tmp/.provision-pi* /tmp/halos-debs-stage /var/lib/fake-dpkg
    sudo -n truncate -s 0 /var/log/shim_calls.log
"

# --------------------------------------------------------------------------
# Test 6: provision-pi.sh --dry-run touches nothing
# --------------------------------------------------------------------------
echo
echo "=== Test 6: provision-pi.sh --dry-run touches nothing ==="
PROVISION_DRY_RUN=$(bash "$PROVISION_SCRIPT" "$SSH_HOST_ALIAS" --dry-run --debs "$TEST_DEBS_DIR" --lume-bin "$DUMMY_LUME")
echo "$PROVISION_DRY_RUN" | grep -q "Mode: DRY RUN" || { echo "FAIL: dry-run mode notice missing" >&2; exit 1; }

# Backup cmdline file must NOT exist
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -e /tmp/fake_boot_cmdline.txt.bak-pre-memcg"; then
    echo "FAIL: cmdline backup file was created during provision --dry-run" >&2
    exit 1
fi

# Fake boot cmdline must NOT contain cgroup_enable=memory
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -q 'cgroup_enable=memory' /tmp/fake_boot_cmdline.txt"; then
    echo "FAIL: cmdline.txt was modified during provision --dry-run" >&2
    exit 1
fi

# No packages should be installed
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -d /var/lib/fake-dpkg"; then
    echo "FAIL: fake-dpkg directory was created during provision --dry-run" >&2
    exit 1
fi

# No shim calls logged
SHIM_CALLS=$(ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "wc -l < /var/log/shim_calls.log | tr -d ' '")
if [ "$SHIM_CALLS" -ne 0 ]; then
    echo "FAIL: Shim calls were made during provision --dry-run ($SHIM_CALLS logged)" >&2
    exit 1
fi
echo "PASS: provision-pi.sh --dry-run touched nothing."

# --------------------------------------------------------------------------
# Test 7: Real provision-pi.sh execution (first run)
# --------------------------------------------------------------------------
echo
echo "=== Test 7: Real provision-pi.sh execution (first run) ==="
FIRST_PROV_RUN=$(bash "$PROVISION_SCRIPT" "$SSH_HOST_ALIAS" --debs "$TEST_DEBS_DIR" --lume-bin "$DUMMY_LUME")

# Assert Step 1: System status output
echo "$FIRST_PROV_RUN" | grep -q "=== Step 1: System Status" || { echo "FAIL: Step 1 header missing" >&2; exit 1; }

# Assert Step 2: Memory cgroup backup created, line updated, REBOOT NEEDED printed
echo "$FIRST_PROV_RUN" | grep -q "REBOOT NEEDED" || { echo "FAIL: REBOOT NEEDED notice missing" >&2; exit 1; }
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -f /tmp/fake_boot_cmdline.txt.bak-pre-memcg"; then
    echo "FAIL: cmdline.txt backup file missing" >&2
    exit 1
fi
BAK_CONTENT=$(ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "cat /tmp/fake_boot_cmdline.txt.bak-pre-memcg")
if [ "$BAK_CONTENT" != "$FAKE_BOOT_CONTENT" ]; then
    echo "FAIL: cmdline.txt backup content differs from original ($BAK_CONTENT vs $FAKE_BOOT_CONTENT)" >&2
    exit 1
fi
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -q 'cgroup_enable=memory cgroup_memory=1' /tmp/fake_boot_cmdline.txt"; then
    echo "FAIL: cgroup_enable=memory cgroup_memory=1 missing in cmdline.txt" >&2
    exit 1
fi
echo "Verified: Memory cgroup backup created and parameters appended."

# Assert Step 3: Pinned UUID reported
echo "$FIRST_PROV_RUN" | grep -q "Signal K self vessel UUID found: ${TEST_UUID}" || {
    echo "FAIL: Signal K self vessel UUID was not reported correctly" >&2
    exit 1
}
echo "Verified: Signal K self vessel UUID reported correctly."

# Assert Step 4: HaLOS debs installed, skipping marine-ollama-container
echo "$FIRST_PROV_RUN" | grep -q "Skipping marine-ollama-container package" || {
    echo "FAIL: marine-ollama-container was not skipped" >&2
    exit 1
}
if ! ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -qx 'marine-grubcrawler-container' /var/lib/fake-dpkg/installed.list 2>/dev/null"; then
    echo "FAIL: marine-grubcrawler-container was not marked installed" >&2
    exit 1
fi
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -qx 'marine-ollama-container' /var/lib/fake-dpkg/installed.list 2>/dev/null"; then
    echo "FAIL: marine-ollama-container was installed when it should have been skipped" >&2
    exit 1
fi
echo "Verified: HaLOS debs installed and marine-ollama-container skipped."

# Assert Step 5: Plugin deployed via deploy-pi.sh
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "test -f /var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti/package.json" || {
    echo "FAIL: plugin package.json missing after provision" >&2; exit 1;
}
PROV_LUME_MODE=$(ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "stat -c %a /var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti/bin/linux-arm64/lume")
if [ "$PROV_LUME_MODE" != "755" ]; then
    echo "FAIL: plugin lume binary mode is $PROV_LUME_MODE, expected 755" >&2
    exit 1
fi
echo "Verified: Signal K plugin deployed with mode 755 lume binary."

# Assert Step 6: SETUP §13 instructions printed
echo "$FIRST_PROV_RUN" | grep -q "ollama.com API key" || { echo "FAIL: SETUP §13 instructions missing" >&2; exit 1; }
echo "$FIRST_PROV_RUN" | grep -q "chatApiKeyFile" || echo "$FIRST_PROV_RUN" | grep -q "ollama.key" || {
    echo "FAIL: SETUP §13 key file path missing" >&2
    exit 1
}
echo "PASS: provision-pi.sh initial run executed all steps successfully."

# --------------------------------------------------------------------------
# Test 8: Real provision-pi.sh second run (idempotency: all steps must SKIP)
# --------------------------------------------------------------------------
echo
echo "=== Test 8: Real provision-pi.sh second run (asserting all SKIP) ==="
CMDLINE_CKSUM_BEFORE=$(ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "sha256sum /tmp/fake_boot_cmdline.txt | awk '{print \$1}'")
ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "sudo -n truncate -s 0 /var/log/shim_calls.log"

SECOND_PROV_RUN=$(bash "$PROVISION_SCRIPT" "$SSH_HOST_ALIAS" --debs "$TEST_DEBS_DIR" --lume-bin "$DUMMY_LUME")
echo "$SECOND_PROV_RUN"

# Assert each step outputs [SKIP]
echo "$SECOND_PROV_RUN" | grep -q "\[SKIP\] System status already reported" || {
    echo "FAIL: Step 1 did not SKIP" >&2; exit 1;
}
echo "$SECOND_PROV_RUN" | grep -q "\[SKIP\] Memory cgroup already configured" || {
    echo "FAIL: Step 2 did not SKIP" >&2; exit 1;
}
echo "$SECOND_PROV_RUN" | grep -q "\[SKIP\] Signal K self vessel UUID already checked" || {
    echo "FAIL: Step 3 did not SKIP" >&2; exit 1;
}
echo "$SECOND_PROV_RUN" | grep -q "\[SKIP\] Package marine-grubcrawler-container is already installed" || {
    echo "FAIL: Step 4 deb did not SKIP" >&2; exit 1;
}
echo "$SECOND_PROV_RUN" | grep -q "\[SKIP\] Skipping marine-ollama-container" || {
    echo "FAIL: Step 4 ollama did not SKIP" >&2; exit 1;
}
echo "$SECOND_PROV_RUN" | grep -q "\[SKIP\] Signal K Lume TI plugin is already deployed and active" || {
    echo "FAIL: Step 5 did not SKIP" >&2; exit 1;
}
echo "$SECOND_PROV_RUN" | grep -q "\[SKIP\] Ask tab key-file instructions already displayed" || {
    echo "FAIL: Step 6 did not SKIP" >&2; exit 1;
}

# Assert cmdline.txt was not modified again
CMDLINE_CKSUM_AFTER=$(ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "sha256sum /tmp/fake_boot_cmdline.txt | awk '{print \$1}'")
if [ "$CMDLINE_CKSUM_BEFORE" != "$CMDLINE_CKSUM_AFTER" ]; then
    echo "FAIL: cmdline.txt checksum changed on second run ($CMDLINE_CKSUM_BEFORE vs $CMDLINE_CKSUM_AFTER)" >&2
    exit 1
fi

# Assert no apt-get or systemctl restart shim calls were made during second run
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -q 'apt-get install' /var/log/shim_calls.log"; then
    echo "FAIL: apt-get install was called during idempotent second run" >&2
    exit 1
fi
if ssh -F "$LUME_DEPLOY_SSH_CONFIG" "$SSH_HOST_ALIAS" "grep -q 'systemctl restart' /var/log/shim_calls.log"; then
    echo "FAIL: systemctl restart was called during idempotent second run" >&2
    exit 1
fi

echo "PASS: provision-pi.sh idempotent second run skipped all steps without side effects."

# --------------------------------------------------------------------------
# Test 9: Assert ~/.ssh/config checksum unchanged
# --------------------------------------------------------------------------
echo
echo "=== Test 9: Verify ~/.ssh/config was untouched ==="
if [ "$ORIG_SSH_CONFIG_EXISTS" = true ]; then
    if [ ! -f "$USER_SSH_CONFIG" ]; then
        echo "FAIL: ~/.ssh/config was deleted during test run" >&2
        exit 1
    fi
    CURRENT_CKSUM="$(sha256sum "$USER_SSH_CONFIG" | awk '{print $1}')"
    if [ "$INITIAL_SSH_CONFIG_CKSUM" != "$CURRENT_CKSUM" ]; then
        echo "FAIL: ~/.ssh/config checksum changed ($INITIAL_SSH_CONFIG_CKSUM vs $CURRENT_CKSUM)" >&2
        exit 1
    fi
    echo "PASS: ~/.ssh/config checksum is unchanged ($INITIAL_SSH_CONFIG_CKSUM)."
else
    if [ -f "$USER_SSH_CONFIG" ]; then
        echo "FAIL: ~/.ssh/config was created during test run" >&2
        exit 1
    fi
    echo "PASS: ~/.ssh/config was not created or touched."
fi

#!/usr/bin/env bash
# Run a real Signal K server in Docker with the Lume TI plugin loaded straight from
# this checkout, fed by Signal K's built-in sample boat data. For testing the plugin
# and its web app on a laptop before a release.
#
#   scripts/dev-signalk.sh                 start (or restart) on http://localhost:3000
#   scripts/dev-signalk.sh --secure        same, with Signal K security on (login, access requests)
#   scripts/dev-signalk.sh --lume <path>   use this Linux x86_64 lume binary instead of the release
#   scripts/dev-signalk.sh --port 3001     publish Signal K on another host port
#   scripts/dev-signalk.sh --logs          follow the server log
#   scripts/dev-signalk.sh --stop          stop and remove the container (data is kept)
#   scripts/dev-signalk.sh --reset         stop and also delete the Signal K data volume
#
# Edits to plugins/signalk-lume-ti show up after a page reload (public/) or a Signal K
# restart (index.js, lib/). The lume binary defaults to the release named in the
# plugin's package.json, downloaded once and checked against SHA256SUMS.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NAME=lume-dev-signalk
VOLUME=lume-dev-signalk-data
IMAGE="${SIGNALK_IMAGE:-signalk/signalk-server:latest}"
PORT=3000
SECURE=false
LUME_BIN=""
ACTION=start

while [ $# -gt 0 ]; do
    case "$1" in
        --secure) SECURE=true ;;
        --lume) LUME_BIN="${2:?--lume needs a path}"; shift ;;
        --port) PORT="${2:?--port needs a number}"; shift ;;
        --logs) ACTION=logs ;;
        --stop) ACTION=stop ;;
        --reset) ACTION=reset ;;
        -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Error: unknown option $1" >&2; exit 1 ;;
    esac
    shift
done

command -v docker >/dev/null || { echo "Error: docker is required" >&2; exit 1; }

# Docker on Windows (Git Bash) needs Windows-style paths for bind mounts.
host_path() {
    if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi
}
# Git Bash would rewrite the container paths in docker arguments; keep them as written.
dk() { MSYS_NO_PATHCONV=1 docker "$@"; }

case "$ACTION" in
    logs) dk logs -f "$NAME"; exit ;;
    stop) dk rm -f "$NAME" >/dev/null 2>&1 || true; echo "Stopped $NAME (data volume $VOLUME kept)"; exit 0 ;;
    reset)
        dk rm -f "$NAME" >/dev/null 2>&1 || true
        dk volume rm "$VOLUME" >/dev/null 2>&1 || true
        echo "Removed $NAME and its data volume $VOLUME"; exit 0 ;;
esac

PLUGIN="$ROOT/plugins/signalk-lume-ti"
VERSION="$(node -p "require('$(host_path "$PLUGIN")/package.json').version" 2>/dev/null || sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$PLUGIN/package.json" | head -1)"

# 1. A Linux x86_64 lume binary for the container.
if [ -z "$LUME_BIN" ]; then
    CACHE="$ROOT/target/dev-signalk/v$VERSION"
    LUME_BIN="$CACHE/lume"
    if [ ! -s "$LUME_BIN" ]; then
        mkdir -p "$CACHE"
        TARBALL="lume-v$VERSION-x86_64-unknown-linux-gnu.tar.gz"
        BASE="https://github.com/DeepBlueDynamics/lume/releases/download/v$VERSION"
        echo "Downloading lume v$VERSION for the container..."
        curl -fsSL -o "$(host_path "$CACHE/$TARBALL")" "$BASE/$TARBALL"
        curl -fsSL -o "$(host_path "$CACHE/SHA256SUMS")" "$BASE/SHA256SUMS"
        (cd "$CACHE" && grep " $TARBALL\$" SHA256SUMS | sha256sum -c - >/dev/null) \
            || { echo "Error: $TARBALL does not match SHA256SUMS" >&2; exit 1; }
        tar -xzf "$CACHE/$TARBALL" -C "$CACHE"
        found="$(find "$CACHE" -type f -name lume | head -1)"
        [ -n "$found" ] || { echo "Error: no lume binary in $TARBALL" >&2; exit 1; }
        [ "$found" = "$LUME_BIN" ] || mv "$found" "$LUME_BIN"
        chmod +x "$LUME_BIN"
    fi
fi
[ -f "$LUME_BIN" ] || { echo "Error: lume binary not found at $LUME_BIN" >&2; exit 1; }
LUME_DIR="$(cd "$(dirname "$LUME_BIN")" && pwd)"
LUME_NAME="$(basename "$LUME_BIN")"

# 2. Seed the data volume once: register the plugin, enable it, point it at the binary.
dk volume create "$VOLUME" >/dev/null
dk run --rm --entrypoint sh -v "$VOLUME:/home/node/.signalk" "$IMAGE" -c "
    set -e
    cd /home/node/.signalk
    [ -f package.json ] || printf '%s\n' '{\"name\":\"signalk-server-config\",\"version\":\"0.0.1\",\"dependencies\":{\"signalk-lume-ti\":\"file:./node_modules/signalk-lume-ti\"}}' > package.json
    mkdir -p node_modules/signalk-lume-ti plugin-config-data
    [ -f plugin-config-data/signalk-lume-ti.json ] || printf '%s\n' '{\"enabled\":true,\"enableLogging\":true,\"configuration\":{\"lumePath\":\"/opt/lume/$LUME_NAME\",\"servePort\":5863,\"signalkUrl\":\"ws://127.0.0.1:3000\"}}' > plugin-config-data/signalk-lume-ti.json
"

# 3. Start Signal K with sample data. The plugin is mounted from the checkout.
ARGS=(--sample-nmea0183-data --override-timestamps)
[ "$SECURE" = true ] && ARGS+=(--securityenabled)
dk rm -f "$NAME" >/dev/null 2>&1 || true
dk run -d --name "$NAME" \
    -p "127.0.0.1:$PORT:3000" \
    -v "$VOLUME:/home/node/.signalk" \
    -v "$(host_path "$PLUGIN"):/home/node/.signalk/node_modules/signalk-lume-ti:ro" \
    -v "$(host_path "$LUME_DIR"):/opt/lume:ro" \
    --entrypoint /home/node/signalk/node_modules/signalk-server/bin/signalk-server \
    "$IMAGE" "${ARGS[@]}" >/dev/null

echo "Waiting for Signal K..."
for _ in $(seq 1 60); do
    curl -fsS -o /dev/null "http://127.0.0.1:$PORT/signalk" 2>/dev/null && break
    sleep 1
done
curl -fsS -o /dev/null "http://127.0.0.1:$PORT/signalk" \
    || { echo "Error: Signal K did not start; see scripts/dev-signalk.sh --logs" >&2; exit 1; }

echo
echo "Signal K:  http://localhost:$PORT/admin/"
echo "Lume TI:   http://localhost:$PORT/signalk-lume-ti/"
echo "Plugin:    $PLUGIN (live from this checkout)"
echo "lume:      $LUME_BIN"
if [ "$SECURE" = true ]; then
    echo "Security is on: create the admin user on first visit, then approve Lume TI under Security -> Access Requests."
else
    echo "Security is off: no login needed. Use --secure to test sign-in and access approval."
fi
echo "Sample boat data is playing; the first rows appear in the SQL console within a minute."

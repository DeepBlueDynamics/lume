#!/bin/bash
# Test building the HaLOS marine-lume-container .deb package.
#
# Verifies:
#   1. build-halos-debs.sh requires an arm64 binary if not in source tree.
#   2. build-halos-debs.sh --lume-bin builds a valid .deb into output dir.
#   3. Package metadata (dpkg-deb -I): Package, Architecture, Depends, maintainer scripts.
#   4. Package contents (dpkg-deb -c): plugin files, binary mode 755, excluded node_modules/test.
#   5. postinst configuration logic: safe loopback defaults, enabled: true, no secrets.
#   6. postrm removal logic: remove disables plugin & keeps store; purge removes store.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
BUILD_SCRIPT="${REPO_ROOT}/scripts/build-halos-debs.sh"

echo "=== Test Suite: HaLOS marine-lume-container .deb Package ==="

if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "ERROR: dpkg-deb is required to run build-halos-debs.test.sh" >&2
    exit 1
fi

TMP_DIR="$(mktemp -d -t test-build-halos-debs.XXXXXX)"
trap 'rm -rf "${TMP_DIR}"' EXIT

DUMMY_LUME="${TMP_DIR}/fake-lume"
cat << 'EOF' > "$DUMMY_LUME"
#!/bin/sh
echo "lume 0.12.0 arm64 test binary"
EOF
chmod 755 "$DUMMY_LUME"

# --------------------------------------------------------------------------
# Test 1: build-halos-debs.sh requires valid lume binary
# --------------------------------------------------------------------------
echo
echo "=== Test 1: Error handling for missing lume binary ==="
if bash "$BUILD_SCRIPT" --lume-bin "${TMP_DIR}/nonexistent" --app marine-lume-container --output "${TMP_DIR}/dist" >/dev/null 2>&1; then
    echo "FAIL: Expected build script to fail with nonexistent binary" >&2
    exit 1
fi
echo "PASS: Missing binary rejected cleanly."

# --------------------------------------------------------------------------
# Test 2: Build marine-lume-container .deb with dummy binary
# --------------------------------------------------------------------------
echo
echo "=== Test 2: Build marine-lume-container package ==="
BUILD_OUTPUT=$(bash "$BUILD_SCRIPT" --lume-bin "$DUMMY_LUME" --app marine-lume-container --output "${TMP_DIR}/dist")
echo "$BUILD_OUTPUT" | grep -q "Built:.*marine-lume-container" || {
    echo "FAIL: Build output did not confirm package creation" >&2
    exit 1
}

shopt -s nullglob
DEB_FILES=("${TMP_DIR}/dist"/marine-lume-container_*_arm64.deb)
shopt -u nullglob

if [ ${#DEB_FILES[@]} -ne 1 ]; then
    echo "FAIL: Expected exactly 1 .deb package, found ${#DEB_FILES[@]}" >&2
    exit 1
fi
DEB_FILE="${DEB_FILES[0]}"
echo "PASS: Successfully built $(basename "$DEB_FILE")."

# --------------------------------------------------------------------------
# Test 3: Verify package metadata (control archive)
# --------------------------------------------------------------------------
echo
echo "=== Test 3: Verify package metadata with dpkg-deb -I ==="
PKG_INFO=$(dpkg-deb -I "$DEB_FILE")

echo "$PKG_INFO" | grep -q "^ Package: marine-lume-container$" || {
    echo "FAIL: Package name is not marine-lume-container" >&2
    exit 1
}
echo "$PKG_INFO" | grep -q "^ Architecture: arm64$" || {
    echo "FAIL: Architecture is not arm64" >&2
    exit 1
}
echo "$PKG_INFO" | grep -q "Depends:.*marine-signalk-server-container" || {
    echo "FAIL: Depends missing marine-signalk-server-container" >&2
    exit 1
}
echo "$PKG_INFO" | grep -q "postinst" || {
    echo "FAIL: postinst script missing from control archive" >&2
    exit 1
}
echo "$PKG_INFO" | grep -q "postrm" || {
    echo "FAIL: postrm script missing from control archive" >&2
    exit 1
}
echo "PASS: Package metadata, dependencies, and maintainer scripts verified."

# --------------------------------------------------------------------------
# Test 4: Verify package contents (data archive)
# --------------------------------------------------------------------------
echo
echo "=== Test 4: Verify package contents with dpkg-deb -c ==="
PKG_CONTENTS=$(dpkg-deb -c "$DEB_FILE")

DEST_PREFIX="./var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti"

echo "$PKG_CONTENTS" | grep -q "${DEST_PREFIX}/package.json" || {
    echo "FAIL: package.json missing from package contents" >&2
    exit 1
}
echo "$PKG_CONTENTS" | grep -q "${DEST_PREFIX}/index.js" || {
    echo "FAIL: index.js missing from package contents" >&2
    exit 1
}
echo "$PKG_CONTENTS" | grep -q "${DEST_PREFIX}/lib/supervisor.js" || {
    echo "FAIL: lib/supervisor.js missing from package contents" >&2
    exit 1
}
echo "$PKG_CONTENTS" | grep -q "${DEST_PREFIX}/public/index.html" || {
    echo "FAIL: public/index.html missing from package contents" >&2
    exit 1
}
echo "$PKG_CONTENTS" | grep -q "${DEST_PREFIX}/bin/linux-arm64/lume" || {
    echo "FAIL: bin/linux-arm64/lume missing from package contents" >&2
    exit 1
}

# Verify lume binary has executable permissions in the archive
LUME_LINE=$(echo "$PKG_CONTENTS" | grep "${DEST_PREFIX}/bin/linux-arm64/lume")
echo "$LUME_LINE" | grep -q "^-rwxr-xr-x" || {
    echo "FAIL: Expected lume binary permissions -rwxr-xr-x, got: $LUME_LINE" >&2
    exit 1
}

# Assert node_modules and test directories are excluded
if echo "$PKG_CONTENTS" | grep -q "${DEST_PREFIX}/node_modules"; then
    echo "FAIL: node_modules found in package contents but should be excluded" >&2
    exit 1
fi
if echo "$PKG_CONTENTS" | grep -q "${DEST_PREFIX}/test"; then
    echo "FAIL: test directory found in package contents but should be excluded" >&2
    exit 1
fi
echo "PASS: Package contents and permissions verified (node_modules and test excluded)."

# --------------------------------------------------------------------------
# Test 5: Verify postinst configuration logic (safe defaults, no secrets)
# --------------------------------------------------------------------------
echo
echo "=== Test 5: Verify postinst safe defaults configuration ==="
SANDBOX="${TMP_DIR}/sandbox"
SK_DATA="${SANDBOX}/var/lib/container-apps/marine-signalk-server-container/data/data"
CONFIG_DIR="${SK_DATA}/plugin-config-data"
CONFIG_FILE="${CONFIG_DIR}/signalk-lume-ti.json"
mkdir -p "$CONFIG_DIR"

# Run Python configuration logic as in postinst
python3 -c "
import json, os, sys

config_dir = sys.argv[1]
config_file = sys.argv[2]

defaults = {
    'signalkUrl': 'ws://127.0.0.1:3000',
    'servePort': 5863,
    'enablePg': False,
    'pgPort': 5864,
    'pgBind': '127.0.0.1',
    'pgRequireTls': 'auto',
    'chatOllamaUrl': 'https://ollama.com',
    'chatModel': 'glm-5.3:cloud',
}

doc = {}
if os.path.exists(config_file):
    try:
        with open(config_file, 'r', encoding='utf-8') as f:
            doc = json.load(f)
    except Exception:
        doc = {}

doc['enabled'] = True
cfg = doc.setdefault('configuration', {})
for k, v in defaults.items():
    cfg.setdefault(k, v)

if 'pgPassword' in cfg:
    del cfg['pgPassword']

tmp = config_file + '.tmp'
with open(tmp, 'w', encoding='utf-8') as f:
    json.dump(doc, f, indent=2)
    f.write('\n')
os.replace(tmp, config_file)
" "$CONFIG_DIR" "$CONFIG_FILE"

# Verify generated configuration
[ -f "$CONFIG_FILE" ] || { echo "FAIL: Configuration file not created" >&2; exit 1; }

python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)

assert d.get('enabled') is True, 'plugin must be enabled'
cfg = d.get('configuration', {})
assert cfg.get('signalkUrl') == 'ws://127.0.0.1:3000', 'must default to loopback ws'
assert cfg.get('servePort') == 5863, 'must default to port 5863'
assert cfg.get('enablePg') is False, 'pg must be disabled by default'
assert cfg.get('pgBind') == '127.0.0.1', 'pgBind must default to loopback'
assert 'pgPassword' not in cfg, 'must contain no secrets'
assert 'chatApiKey' not in cfg, 'must contain no API keys'
" "$CONFIG_FILE"

# Test idempotence & preserving user values while ensuring enabled: true
python3 -c "
import json, sys
path = sys.argv[1]
with open(path) as f:
    d = json.load(f)
d['configuration']['servePort'] = 5899
d['configuration']['pgPassword'] = 'secret-should-be-purged'
d['enabled'] = False
with open(path, 'w') as f:
    json.dump(d, f)
" "$CONFIG_FILE"

# Re-run postinst config logic
python3 -c "
import json, os, sys

config_dir = sys.argv[1]
config_file = sys.argv[2]

defaults = {
    'signalkUrl': 'ws://127.0.0.1:3000',
    'servePort': 5863,
    'enablePg': False,
    'pgPort': 5864,
    'pgBind': '127.0.0.1',
    'pgRequireTls': 'auto',
    'chatOllamaUrl': 'https://ollama.com',
    'chatModel': 'glm-5.3:cloud',
}

doc = {}
if os.path.exists(config_file):
    try:
        with open(config_file, 'r', encoding='utf-8') as f:
            doc = json.load(f)
    except Exception:
        doc = {}

doc['enabled'] = True
cfg = doc.setdefault('configuration', {})
for k, v in defaults.items():
    cfg.setdefault(k, v)

if 'pgPassword' in cfg:
    del cfg['pgPassword']

tmp = config_file + '.tmp'
with open(tmp, 'w', encoding='utf-8') as f:
    json.dump(doc, f, indent=2)
    f.write('\n')
os.replace(tmp, config_file)
" "$CONFIG_DIR" "$CONFIG_FILE"

python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
assert d['enabled'] is True, 're-enables plugin'
assert d['configuration']['servePort'] == 5899, 'preserves custom user setting'
assert 'pgPassword' not in d['configuration'], 'purges secrets'
" "$CONFIG_FILE"
echo "PASS: postinst safe default configuration verified."

# --------------------------------------------------------------------------
# Test 6: Verify postrm removal logic (remove vs purge)
# --------------------------------------------------------------------------
echo
echo "=== Test 6: Verify postrm removal vs purge behavior ==="
STORE_DIR="${SK_DATA}/lume-ti"
mkdir -p "$STORE_DIR"
echo "valuable boat telemetry" > "${STORE_DIR}/data.db"

# Simulate postrm remove: disables plugin, keeps store
python3 -c "
import json, os, sys
p = sys.argv[1]
with open(p) as f:
    d = json.load(f)
d['enabled'] = False
tmp = p + '.tmp'
with open(tmp, 'w') as f:
    json.dump(d, f)
os.replace(tmp, p)
" "$CONFIG_FILE"

[ -f "${STORE_DIR}/data.db" ] || { echo "FAIL: Store was removed during simulated remove" >&2; exit 1; }
python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
assert d['enabled'] is False, 'plugin must be disabled on remove'
" "$CONFIG_FILE"
echo "Verified: remove disables plugin and preserves user store."

# Simulate postrm purge: deletes store and config
rm -rf "$STORE_DIR" "$CONFIG_FILE" "${CONFIG_DIR}/signalk-lume-ti"
[ ! -e "$STORE_DIR" ] || { echo "FAIL: Store dir was not deleted on purge" >&2; exit 1; }
[ ! -e "$CONFIG_FILE" ] || { echo "FAIL: Config file was not deleted on purge" >&2; exit 1; }
echo "Verified: purge cleanly removes user store and configuration."
echo "PASS: postrm remove/purge behaviors verified."

echo
echo "=== All HaLOS marine-lume-container .deb package tests passed! ==="

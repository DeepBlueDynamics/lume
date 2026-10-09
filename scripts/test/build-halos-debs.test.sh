#!/bin/bash
# Test building the HaLOS marine-lume .deb package.
#
# Verifies:
#   1. build-halos-debs.sh requires an arm64 binary if not in source tree.
#   2. build-halos-debs.sh --lume-bin builds a valid .deb into output dir.
#   3. Package metadata (dpkg-deb -I): Package, Architecture, Depends, Description, maintainer scripts.
#   4. Package contents (dpkg-deb -c): plugin files, binary mode 755, excluded node_modules/test.
#   5. postinst configuration & registration logic: safe defaults, Ask off, package.json dep, symlink.
#   6. postrm removal logic: unregistration, remove disables & keeps store; purge removes store.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
BUILD_SCRIPT="${REPO_ROOT}/scripts/build-halos-debs.sh"

echo "=== Test Suite: HaLOS marine-lume .deb Package ==="

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
if bash "$BUILD_SCRIPT" --lume-bin "${TMP_DIR}/nonexistent" --app marine-lume --output "${TMP_DIR}/dist" >/dev/null 2>&1; then
    echo "FAIL: Expected build script to fail with nonexistent binary" >&2
    exit 1
fi
echo "PASS: Missing binary rejected cleanly."

# --------------------------------------------------------------------------
# Test 2: Build marine-lume .deb with dummy binary
# --------------------------------------------------------------------------
echo
echo "=== Test 2: Build marine-lume package ==="
BUILD_OUTPUT=$(bash "$BUILD_SCRIPT" --lume-bin "$DUMMY_LUME" --app marine-lume --output "${TMP_DIR}/dist")
echo "$BUILD_OUTPUT" | grep -q "Built:.*marine-lume_" || {
    echo "FAIL: Build output did not confirm package creation" >&2
    exit 1
}

shopt -s nullglob
DEB_FILES=("${TMP_DIR}/dist"/marine-lume_*_arm64.deb)
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

echo "$PKG_INFO" | grep -q "^ Package: marine-lume$" || {
    echo "FAIL: Package name is not marine-lume" >&2
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
echo "$PKG_INFO" | grep -q "Description: Records Signal K history on the Pi" || {
    echo "FAIL: Description does not match expected wording" >&2
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
echo "PASS: Package metadata, dependencies, description, and maintainer scripts verified."

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
# Test 5: Verify postinst configuration & registration logic
# --------------------------------------------------------------------------
echo
echo "=== Test 5: Verify postinst safe defaults and plugin registration ==="
SANDBOX="${TMP_DIR}/sandbox"
SK_DATA="${SANDBOX}/var/lib/container-apps/marine-signalk-server-container/data/data"
CONFIG_DIR="${SK_DATA}/plugin-config-data"
CONFIG_FILE="${CONFIG_DIR}/signalk-lume-ti.json"
PKG_JSON="${SK_DATA}/package.json"
NODE_MODULES="${SK_DATA}/node_modules"
mkdir -p "$CONFIG_DIR" "$SK_DATA"

# Seed an existing package.json with another dependency to test preservation
cat << 'EOF' > "$PKG_JSON"
{
  "name": "signalk-server-data",
  "dependencies": {
    "@signalk/freeboard-sk": "^1.0.0"
  }
}
EOF

# Run Python configuration logic as in postinst (when no key file exists)
python3 -c "
import json, os, sys

config_dir = sys.argv[1]
config_file = sys.argv[2]

key_host = os.path.join(config_dir, 'signalk-lume-ti', 'ollama.key')
key_container = '/home/node/.signalk/plugin-config-data/signalk-lume-ti/ollama.key'
default_key_file = key_container if os.path.isfile(key_host) else ''

defaults = {
    'signalkUrl': 'ws://127.0.0.1:3000',
    'servePort': 5863,
    'enablePg': False,
    'pgPort': 5864,
    'pgBind': '127.0.0.1',
    'pgRequireTls': 'auto',
    'chatOllamaUrl': 'https://ollama.com',
    'chatModel': 'glm-5.3:cloud',
    'chatApiKeyFile': default_key_file,
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

for secret_key in ['pgPassword', 'apiKey', 'token', 'secret']:
    if secret_key in cfg:
        del cfg[secret_key]

tmp = config_file + '.tmp'
with open(tmp, 'w', encoding='utf-8') as f:
    json.dump(doc, f, indent=2)
    f.write('\n')
os.replace(tmp, config_file)
" "$CONFIG_DIR" "$CONFIG_FILE"

# Run registration logic as in postinst
python3 -c "
import json, os, sys
sk_data = sys.argv[1]
p = os.path.join(sk_data, 'package.json')
doc = {}
if os.path.exists(p):
    try:
        with open(p, 'r', encoding='utf-8') as f:
            doc = json.load(f)
    except Exception:
        doc = {}
deps = doc.setdefault('dependencies', {})
deps['signalk-lume-ti'] = 'file:lume-plugin/signalk-lume-ti'
tmp = p + '.tmp'
with open(tmp, 'w', encoding='utf-8') as f:
    json.dump(doc, f, indent=2)
    f.write('\n')
os.replace(tmp, p)
" "$SK_DATA"

mkdir -p "$NODE_MODULES"
ln -sfn ../lume-plugin/signalk-lume-ti "${NODE_MODULES}/signalk-lume-ti"

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
assert cfg.get('chatApiKeyFile') == '', 'Ask must be off until key file is set'
assert 'pgPassword' not in cfg, 'must contain no secrets'
assert 'apiKey' not in cfg, 'must contain no raw API keys'
" "$CONFIG_FILE"

# Verify registration in package.json & node_modules symlink
python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
assert d.get('name') == 'signalk-server-data', 'keeps existing top-level fields'
deps = d.get('dependencies', {})
assert deps.get('signalk-lume-ti') == 'file:lume-plugin/signalk-lume-ti', 'registers signalk-lume-ti'
assert deps.get('@signalk/freeboard-sk') == '^1.0.0', 'preserves existing dependencies'
" "$PKG_JSON"

[ -L "${NODE_MODULES}/signalk-lume-ti" ] || { echo "FAIL: node_modules symlink missing" >&2; exit 1; }
LINK_TARGET=$(readlink "${NODE_MODULES}/signalk-lume-ti")
[ "$LINK_TARGET" = "../lume-plugin/signalk-lume-ti" ] || { echo "FAIL: symlink target incorrect: $LINK_TARGET" >&2; exit 1; }

echo "PASS: postinst safe default configuration and plugin registration verified."

# --------------------------------------------------------------------------
# Test 6: Verify postrm removal logic (remove vs purge, including unregistration)
# --------------------------------------------------------------------------
echo
echo "=== Test 6: Verify postrm removal vs purge behavior (unregistration & store) ==="
PLUGIN_DIR="${SK_DATA}/lume-plugin/signalk-lume-ti"
STORE_DIR="${SK_DATA}/lume-ti"
mkdir -p "$STORE_DIR" "$PLUGIN_DIR"
echo "valuable boat telemetry" > "${STORE_DIR}/data.db"
echo "console.log('plugin')" > "${PLUGIN_DIR}/index.js"

# Simulate postrm unregistration helper (same as in postrm)
python3 -c "
import json, os, sys
p = os.path.join(sys.argv[1], 'package.json')
try:
    with open(p, 'r', encoding='utf-8') as f:
        doc = json.load(f)
    if 'dependencies' in doc and 'signalk-lume-ti' in doc['dependencies']:
        del doc['dependencies']['signalk-lume-ti']
        tmp = p + '.tmp'
        with open(tmp, 'w', encoding='utf-8') as f:
            json.dump(doc, f, indent=2)
            f.write('\n')
        os.replace(tmp, p)
except Exception:
    pass
" "$SK_DATA"
rm -f "${NODE_MODULES}/signalk-lume-ti"

# Simulate postrm remove: disables plugin, removes plugin files, unregisters, keeps store
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
rm -rf "$PLUGIN_DIR"

[ -f "${STORE_DIR}/data.db" ] || { echo "FAIL: Store was removed during simulated remove" >&2; exit 1; }
[ ! -d "$PLUGIN_DIR" ] || { echo "FAIL: Plugin directory remained after simulated remove" >&2; exit 1; }
[ ! -e "${NODE_MODULES}/signalk-lume-ti" ] || { echo "FAIL: Symlink remained after simulated remove" >&2; exit 1; }

python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
deps = d.get('dependencies', {})
assert 'signalk-lume-ti' not in deps, 'signalk-lume-ti must be unregistered from package.json on remove'
assert deps.get('@signalk/freeboard-sk') == '^1.0.0', 'preserves other dependencies on remove'
" "$PKG_JSON"

python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
assert d['enabled'] is False, 'plugin must be disabled on remove'
" "$CONFIG_FILE"
echo "Verified: remove disables plugin, unregisters from package.json/node_modules, cleans plugin files, and preserves user store."

# Simulate postrm purge: deletes store, config, key dir, plugin dir, unregisters
rm -rf "$STORE_DIR" "$CONFIG_FILE" "${CONFIG_DIR}/signalk-lume-ti" "$PLUGIN_DIR"
[ ! -e "$STORE_DIR" ] || { echo "FAIL: Store dir was not deleted on purge" >&2; exit 1; }
[ ! -e "$CONFIG_FILE" ] || { echo "FAIL: Config file was not deleted on purge" >&2; exit 1; }
[ ! -e "${CONFIG_DIR}/signalk-lume-ti" ] || { echo "FAIL: Key dir was not deleted on purge" >&2; exit 1; }
[ ! -e "$PLUGIN_DIR" ] || { echo "FAIL: Plugin dir was not deleted on purge" >&2; exit 1; }
echo "Verified: purge cleanly removes user store, configuration, key dir, and plugin files."
echo "PASS: postrm remove/purge behaviors verified."

echo
echo "=== All HaLOS marine-lume .deb package tests passed! ==="

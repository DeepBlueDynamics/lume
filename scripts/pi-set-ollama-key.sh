#!/bin/bash
# Install an ollama.com API key on a HaLOS Pi for the Lume TI Ask tab (SETUP 13).
#
#   scripts/pi-set-ollama-key.sh <ssh-host> [--dry-run] [--check]
#
# Prompts for the key with hidden input on THIS machine. The key travels to the Pi
# only on SSH stdin: never on argv, in shell history or in logs. On the Pi it:
#   1. writes plugin-config-data/signalk-lume-ti/ollama.key (owner 1000:1000, mode 600,
#      atomic temp + rename);
#   2. sets chatApiKeyFile in the plugin config (backing the JSON up first), and sets
#      chatOllamaUrl/chatModel only if they're unset;
#   3. restarts marine-signalk-server-container and waits for Signal K to be healthy.
# --check only reports whether the key file exists, with its owner, mode and size,
# never its contents. Honours LUME_DEPLOY_SSH_CONFIG like deploy-pi.sh.
set -euo pipefail

SSH_HOST=""
DRY_RUN=false
CHECK=false
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        --check) CHECK=true ;;
        -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "Error: unknown option $arg" >&2; exit 1 ;;
        *)
            if [ -n "$SSH_HOST" ]; then
                echo "Error: unexpected argument $arg" >&2
                exit 1
            fi
            SSH_HOST="$arg"
            ;;
    esac
done
[ -n "$SSH_HOST" ] || { echo "Usage: $(basename "$0") <ssh-host> [--dry-run] [--check]" >&2; exit 1; }

SSH_CMD=(ssh)
[ -n "${LUME_DEPLOY_SSH_CONFIG:-}" ] && SSH_CMD=(ssh -F "$LUME_DEPLOY_SSH_CONFIG")

DATA=/var/lib/container-apps/marine-signalk-server-container/data/data
KEY_DIR="$DATA/plugin-config-data/signalk-lume-ti"
KEY_FILE="$KEY_DIR/ollama.key"
CONFIG="$DATA/plugin-config-data/signalk-lume-ti.json"
# Signal K mounts $DATA at /home/node/.signalk inside its container.
CONTAINER_KEY_FILE=/home/node/.signalk/plugin-config-data/signalk-lume-ti/ollama.key

if [ "$CHECK" = true ]; then
    "${SSH_CMD[@]}" "$SSH_HOST" "sudo -n stat -c '%U:%G %a %s bytes %n' '$KEY_FILE' 2>/dev/null || echo 'no key file at $KEY_FILE'; sudo -n python3 -c 'import json,sys; c=json.load(open(sys.argv[1])).get(\"configuration\",{}); print(\"chatApiKeyFile:\", c.get(\"chatApiKeyFile\",\"(unset)\"))' '$CONFIG'"
    exit 0
fi

if [ "$DRY_RUN" = true ]; then
    echo "[dry-run] prompt for the key (hidden), then on $SSH_HOST:"
    echo "[dry-run]   write $KEY_FILE (1000:1000, 600) from stdin via temp + rename"
    echo "[dry-run]   back up $CONFIG, set chatApiKeyFile=$CONTAINER_KEY_FILE"
    echo "[dry-run]   restart marine-signalk-server-container and wait for health"
    exit 0
fi

if [ ! -t 0 ]; then
    echo "Error: run this in an interactive terminal; it prompts for the key with hidden input." >&2
    exit 1
fi
read -rs -p 'ollama.com API key (input hidden): ' KEY
echo
KEY="${KEY//$'\r'/}"
if [ -z "$KEY" ] || [[ "$KEY" == *[[:space:]]* ]]; then
    unset KEY
    echo "Error: the key must be a single non-empty token without spaces." >&2
    exit 1
fi

echo "Writing the key file on $SSH_HOST..."
printf '%s\n' "$KEY" | "${SSH_CMD[@]}" "$SSH_HOST" "sudo -n sh -c 'umask 077 && mkdir -p \"$KEY_DIR\" && chown 1000:1000 \"$KEY_DIR\" && chmod 755 \"$KEY_DIR\" && cat > \"$KEY_FILE.tmp\" && chown 1000:1000 \"$KEY_FILE.tmp\" && chmod 600 \"$KEY_FILE.tmp\" && mv -f \"$KEY_FILE.tmp\" \"$KEY_FILE\"'"
unset KEY

echo "Pointing the plugin config at the key file..."
"${SSH_CMD[@]}" "$SSH_HOST" "sudo -n cp -p '$CONFIG' '$CONFIG.bak-pre-ollama-key' && sudo -n python3 - '$CONFIG' '$CONTAINER_KEY_FILE' <<'PY'
import json, os, sys
path, key_file = sys.argv[1], sys.argv[2]
with open(path) as f:
    doc = json.load(f)
cfg = doc.setdefault('configuration', {})
cfg['chatApiKeyFile'] = key_file
cfg.setdefault('chatOllamaUrl', 'https://ollama.com')
cfg.setdefault('chatModel', 'glm-5.3:cloud')
st = os.stat(path)
tmp = path + '.tmp'
with open(tmp, 'w') as f:
    json.dump(doc, f, indent=2)
    f.write('\n')
os.chown(tmp, st.st_uid, st.st_gid)
os.chmod(tmp, st.st_mode & 0o777)
os.replace(tmp, path)
print('chatApiKeyFile =', cfg['chatApiKeyFile'])
print('chatOllamaUrl  =', cfg['chatOllamaUrl'])
print('chatModel      =', cfg['chatModel'])
PY"

echo "Restarting Signal K..."
# shellcheck disable=SC2016 # $i and $(seq) expand on the Pi, not here
"${SSH_CMD[@]}" "$SSH_HOST" 'sudo -n systemctl restart marine-signalk-server-container; for i in $(seq 1 30); do if curl -fsS -m 3 http://127.0.0.1:3000/signalk >/dev/null 2>&1; then echo "Signal K is healthy (attempt $i/30)"; exit 0; fi; sleep 2; done; echo "Timed out waiting for Signal K" >&2; exit 1'

"${SSH_CMD[@]}" "$SSH_HOST" "sudo -n stat -c 'key file: %U:%G mode %a, %s bytes' '$KEY_FILE'"
echo "Done. Try a question in the Lume TI Ask tab."

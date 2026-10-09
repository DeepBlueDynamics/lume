#!/bin/bash
# Lume TI server prestart hook, installed as app-prestart.sh by
# generate-container-packages (sourced by the framework prestart, which
# defines RUNTIME_ENV and has loaded env.defaults and env). Sourced, so no exit.
#
# 1. LUME_MEMORY_LIMIT=auto sizes the container's memory cap from this machine's
#    RAM: 20 % of MemTotal, at least 768 MiB and at most 3 GiB (1.6 GiB on an
#    8 GB Pi 5). An explicit value such as 1g is left alone.
# 2. LUME_SET_PLUGIN_EXTERNAL=true sets the signalk-lume-ti plugin's
#    serverMode to external, so the plugin stops running lume inside Signal K
#    and writes the server arguments this container runs. Signal K is restarted
#    once, only when the setting changed.
if [ "${LUME_MEMORY_LIMIT:-auto}" = "auto" ]; then
    total_mb=$(awk '/^MemTotal:/ {print int($2 / 1024)}' /proc/meminfo)
    limit_mb=$(( total_mb * 20 / 100 ))
    [ "$limit_mb" -lt 768 ] && limit_mb=768
    [ "$limit_mb" -gt 3072 ] && limit_mb=3072
    echo "LUME_MEMORY_LIMIT=${limit_mb}m" >> "$RUNTIME_ENV"
    echo "Lume memory limit: ${limit_mb} MiB (auto, ${total_mb} MiB RAM)"
fi

lume_plugin_config=/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti.json
if [ "${LUME_SET_PLUGIN_EXTERNAL:-true}" = "true" ]; then
    # Exit status 10 means the file was changed. The directory belongs to the
    # Signal K container's uid 1000, so nothing is followed through a symlink
    # and the new file is created next to the old one, then renamed over it.
    lume_rc=0
    python3 - "$lume_plugin_config" << 'PY' || lume_rc=$?
import json, os, sys
path = sys.argv[1]
d, name = os.path.split(path)
if os.path.islink(d) or not os.path.isdir(d):
    print(f"Lume: {d} missing or a symlink; set Query Server Mode to external in the plugin settings")
    sys.exit(0)
try:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
except FileNotFoundError:
    print("Lume: signalk-lume-ti is not configured yet; set Query Server Mode to external in the plugin settings")
    sys.exit(0)
with os.fdopen(fd) as f:
    cfg = json.load(f)
conf = cfg.setdefault("configuration", {})
if conf.get("serverMode") == "external":
    sys.exit(0)
conf["serverMode"] = "external"
st = os.stat(path, follow_symlinks=False)
tmp = os.path.join(d, f".{name}.lume-{os.getpid()}")
fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
with os.fdopen(fd, "w") as f:
    os.fchown(f.fileno(), st.st_uid, st.st_gid)
    json.dump(cfg, f, indent=2)
os.replace(tmp, path)
sys.exit(10)
PY
    if [ "$lume_rc" -eq 10 ]; then
        echo "Lume: set signalk-lume-ti serverMode=external; restarting Signal K"
        systemctl try-restart --no-block marine-signalk-server-container.service || true
    fi
fi

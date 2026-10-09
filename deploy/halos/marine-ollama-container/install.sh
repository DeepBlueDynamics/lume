#!/bin/bash
# Install Ollama as a HaLOS container app, laid out the way HaLOS's
# container-packaging-tools install marine-*-container packages:
#   /var/lib/container-apps/marine-ollama-container/   compose, metadata, prestart
#   /etc/container-apps/marine-ollama-container/env.defaults
#   /etc/systemd/system/marine-ollama-container.service
# then pulls the default model (OLLAMA_DEFAULT_MODEL, `-` to skip).
#
#   sudo docker pull ollama/ollama:latest
#   sudo ./install.sh
set -euo pipefail

app=marine-ollama-container
src="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
lib="/var/lib/container-apps/$app"
etc="/etc/container-apps/$app"
unit="/etc/systemd/system/$app.service"

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
image="$(sed -n 's/^  OLLAMA_IMAGE: *//p' "$src/metadata.yaml")"
model="${OLLAMA_DEFAULT_MODEL:-$(sed -n 's/^  OLLAMA_DEFAULT_MODEL: *//p' "$src/metadata.yaml")}"
docker image inspect "$image" > /dev/null 2>&1 || docker pull "$image"

install -d -m 755 "$lib" "$lib/data/ollama" "$etc"
install -m 644 "$src/docker-compose.yml" "$src/metadata.yaml" "$lib/"
install -m 755 "$src/app-prestart.sh" "$lib/"

cat > "$lib/prestart.sh" << EOF
#!/bin/bash
# Prestart script for $app (same shape as container-packaging-tools output)
set -e
RUNTIME_ENV="/run/container-apps/$app/runtime.env"
mkdir -p "\$(dirname "\$RUNTIME_ENV")"
install -m 600 /dev/null "\$RUNTIME_ENV"
set -a
[ -f "$etc/env.defaults" ] && . "$etc/env.defaults"
[ -f "$etc/env" ] && . "$etc/env"
set +a
[ -f "$lib/app-prestart.sh" ] && . "$lib/app-prestart.sh"
EOF
chmod 755 "$lib/prestart.sh"

{
  echo "# System-managed variables (do not modify)"
  echo "CONTAINER_DATA_ROOT=\"$lib/data\""
  echo
  echo "# Application configuration"
  sed -n '/^default_config:/,$p' "$src/metadata.yaml" | sed -n 's/^  \([A-Z_]*\): *"\{0,1\}\([^"]*\)"\{0,1\}$/\1="\2"/p'
} > "$etc/env.defaults"
chmod 644 "$etc/env.defaults"

cat > "$unit" << EOF
[Unit]
Description=Ollama Container
After=docker.service
Requires=docker.service
StartLimitIntervalSec=600
StartLimitBurst=5

[Service]
Type=simple
WorkingDirectory=$lib
Environment=HALOS_SYSTEMD_STARTED=1
ExecStartPre=$lib/prestart.sh
EnvironmentFile=-$etc/env.defaults
EnvironmentFile=-$etc/env
EnvironmentFile=-/run/container-apps/$app/runtime.env
ExecStart=/usr/bin/docker compose -f docker-compose.yml up
ExecStop=/usr/bin/docker compose -f docker-compose.yml down
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable "$app.service"
# restart, not just start: a reinstall must pick up new settings and the app-prestart hook
systemctl restart "$app.service"
echo "Installed $app; waiting for Ollama..."
for _ in $(seq 1 60); do
  if curl -fsS -m 3 http://127.0.0.1:11434/api/version > /dev/null 2>&1; then
    curl -fsS http://127.0.0.1:11434/api/version; echo
    if [ "$model" != "-" ] && [ -n "$model" ]; then
      echo "Pulling $model (skip with OLLAMA_DEFAULT_MODEL=-)"
      docker exec ollama ollama pull "$model"
    fi
    exit 0
  fi
  sleep 5
done
echo "Ollama did not come up; see: journalctl -u $app -n 100" >&2
exit 1

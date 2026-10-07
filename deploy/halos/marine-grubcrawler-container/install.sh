#!/bin/bash
# Install Grub Crawler as a HaLOS container app, laid out the way HaLOS's
# container-packaging-tools installs marine-*-container packages:
#   /var/lib/container-apps/marine-grubcrawler-container/  compose, metadata, prestart
#   /etc/container-apps/marine-grubcrawler-container/env.defaults
#   /etc/systemd/system/marine-grubcrawler-container.service
#
# Run as root on the Pi, from this directory, after loading the image:
#   docker load < grubcrawler-arm64.tar.gz
#   sudo ./install.sh
set -euo pipefail

app=marine-grubcrawler-container
src="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
lib="/var/lib/container-apps/$app"
etc="/etc/container-apps/$app"
unit="/etc/systemd/system/$app.service"

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
image="$(sed -n 's/^  GRUB_IMAGE: *//p' "$src/metadata.yaml")"
docker image inspect "$image" > /dev/null 2>&1 || { echo "image $image is not loaded" >&2; exit 1; }

install -d -m 755 "$lib" "$lib/data/storage" "$etc"
owner="$(docker run --rm --entrypoint sh "$image" -c 'echo "$(id -u app):$(id -g app)"')"
chown "$owner" "$lib/data/storage" # the image's runtime `app` user
install -m 644 "$src/docker-compose.yml" "$src/metadata.yaml" "$lib/"

cat > "$lib/prestart.sh" << EOF
#!/bin/bash
# Prestart script for $app (same shape as container-packaging-tools output)
set -e
RUNTIME_ENV="/run/container-apps/$app/runtime.env"
mkdir -p "\$(dirname "\$RUNTIME_ENV")"
install -m 600 /dev/null "\$RUNTIME_ENV"
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
Description=Grub Crawler Container
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
systemctl enable --now "$app.service"
echo "Installed $app; waiting for Grub's health check..."
for _ in $(seq 1 60); do
  if curl -fsS -m 3 http://127.0.0.1:6792/health > /dev/null 2>&1; then
    curl -fsS http://127.0.0.1:6792/health; echo
    exit 0
  fi
  sleep 5
done
echo "Grub did not become healthy; see: journalctl -u $app -n 100" >&2
exit 1

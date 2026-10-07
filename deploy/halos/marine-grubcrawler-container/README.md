# Grub Crawler as a HaLOS container app

Runs [Grub Crawler](https://github.com/DeepBlueDynamics/grubcrawler) on a HaLOS Pi
next to the Lume TI Signal K plugin. The plugin uses it to fetch web pages and
PDFs for the offline cruiser library (`lume crawl --list`, `GRUB_BASE_URL`,
default `http://localhost:6792`).

- **Image:** the Chromium-only lite variant (`Dockerfile.lite`, Grub `52b9b51`
  or later), `grubcrawler:lite-arm64`. It is 3.05 GB on the Pi. Once Grub publishes
  multi-arch images, set `GRUB_IMAGE=deepbluedynamics/grubcrawler:latest-lite`.
- **Network:** `127.0.0.1:6792` only. Auth is off, so it must not be published to
  the LAN. The Signal K container uses the host network and reaches it on loopback.
- **Limits:** a 1.5 GB memory cap and one crawl at a time. No LLM agent or vision
  OCR on the boat; plain fetch, render and PDF text extraction work.
- **Layout:** `install.sh` writes the same files and systemd unit that HaLOS's
  container-packaging-tools write for `marine-*-container` packages. It does not
  appear in Cockpit's container store until it is packaged as a `.deb`.

```sh
# on the Pi, with the image loaded (docker load or docker pull)
sudo ./install.sh                       # installs, enables and waits for /health
sudo systemctl status marine-grubcrawler-container
journalctl -u marine-grubcrawler-container -n 100
```

Settings live in `/etc/container-apps/marine-grubcrawler-container/env` and override
`env.defaults`: `GRUB_IMAGE`, `GRUB_MEMORY_LIMIT`, `GRUB_MAX_CONCURRENT_CRAWLS`,
`GRUB_CRAWL_TIMEOUT`, `GRUB_BROWSER_ENGINE`.

Measured on a Raspberry Pi 5 (2026-10-07):

| Test | Time |
|---|---:|
| Chromium crawl of `https://example.com` (first browser launch) | 3.4 s |
| PDF to markdown (text layer) | 0.54 s |
| `lume crawl https://example.com` through Grub | 1.4 s |

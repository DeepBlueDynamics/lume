# Grub Crawler as a HaLOS container app

Runs [Grub Crawler](https://github.com/DeepBlueDynamics/grubcrawler) on a HaLOS Pi
next to the Lume TI Signal K plugin. The plugin uses it to fetch web pages and
PDFs for the offline cruiser library (`lume crawl --list`, `GRUB_BASE_URL`,
default `http://localhost:6792`).

- **Image:** `deepbluedynamics/grubcrawler:latest-lite`, the Chromium-only lite variant (Grub v0.16.1, `Dockerfile.lite`). It is a multi-arch manifest for linux/arm64 and amd64, built on native GitHub runners. It is about 2.8 GB unpacked on arm64. Pin `0.16.1-lite` through `GRUB_IMAGE` for reproducible installs.
- **Network:** `127.0.0.1:6792` only. Auth is off, so it must not be published to
  the LAN. The Signal K container uses the host network and reaches it on loopback.
- **Limits:** memory cap defaults to `auto` (dynamically sized by `app-prestart.sh`
  to 20% of system RAM, 1–4 GiB; ~1.6 GiB on an 8 GB Pi 5, enforced via memory cgroups
  enabled in `cmdline.txt`) and one crawl at a time. No LLM agent or vision OCR on the
  boat; plain fetch, render and PDF text extraction work.
- **Layout:** `install.sh` writes the same files and systemd unit that HaLOS's
  container-packaging-tools write for `marine-*-container` packages. It can also
  be installed directly as a `.deb` package to appear in Cockpit's container store.

## Installing the .deb

Build the package with `scripts/build-halos-debs.sh` or copy the `.deb` from `dist/halos/`:

```sh
# on the Pi
sudo docker pull deepbluedynamics/grubcrawler:latest-lite
sudo dpkg -i marine-grubcrawler-container_0.16.1-1_arm64.deb
sudo systemctl status marine-grubcrawler-container
journalctl -u marine-grubcrawler-container -n 100
```

## Manual installation (via install.sh)

```sh
# on the Pi
sudo docker pull deepbluedynamics/grubcrawler:latest-lite
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

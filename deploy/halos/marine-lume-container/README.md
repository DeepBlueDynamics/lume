# marine-lume-container

HaLOS Debian package prototype for Lume TI on Signal K.

## Overview

`marine-lume-container` packages Lume TI as an easily installable `.deb` package
for HaLOS Marine systems on Raspberry Pi 5.

It integrates directly with the HaLOS Signal K server container
(`marine-signalk-server-container`), installing:
1. The `signalk-lume-ti` Signal K plugin.
2. The native `linux-arm64` `lume` binary.

## Installation

Install the `.deb` package on the HaLOS host:

```bash
sudo apt-get install ./marine-lume-container_0.12.0-1_arm64.deb
```

During package installation (`postinst`):
- Plugin files and binary are placed into `/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti`.
- Ownership is set to `1000:1000` (the Signal K container runtime user) and binary permissions to `0755`.
- Safe defaults are written to `/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti.json` (loopback only, no secrets).
- If `marine-signalk-server-container` is active, it is restarted so Signal K automatically activates the plugin.

## Removal and Purge

- **Remove** (`sudo apt-get remove marine-lume-container`):
  Uninstalls the plugin files and disables the plugin in Signal K configuration.
  The user's historical telemetry store (`.../data/data/lume-ti`) is **preserved**.

- **Purge** (`sudo apt-get purge marine-lume-container`):
  Uninstalls the plugin files, configuration, and permanently removes the
  telemetry store directory (`.../data/data/lume-ti`).

## Building the Package

Build with `scripts/build-halos-debs.sh`:

```bash
scripts/build-halos-debs.sh --lume-bin /path/to/target/release/lume
```

The output `.deb` is written to `dist/halos/marine-lume-container_0.12.0-1_arm64.deb`.

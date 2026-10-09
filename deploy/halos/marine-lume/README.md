# marine-lume

HaLOS Debian package for Lume TI on Signal K.

## Overview

`marine-lume` packages Lume TI as an easily installable `.deb` package
for HaLOS Marine systems on Raspberry Pi 5.

It integrates directly with the HaLOS Signal K server container
(`marine-signalk-server-container`), installing:
1. The `signalk-lume-ti` Signal K plugin.
2. The native `linux-arm64` `lume` binary.

## Install

To install the `.deb` package on a HaLOS host:

```bash
sudo apt install ./marine-lume_0.12.0-1_arm64.deb
```

During package installation (`postinst`):
- Plugin files and binary are placed into `/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti`.
- File permissions are set to `1000:1000` (Signal K container runtime user) and binary permissions to `0755`.
- Safe defaults are written to `/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti.json`:
  - Query server bound to loopback only (`127.0.0.1:5863`).
  - No secrets stored in configuration.
  - Ask tab is disabled (`chatApiKeyFile` empty) until an API key file is configured.
- `marine-signalk-server-container` is restarted so Signal K automatically activates the plugin.

## Verify

Check that Signal K restarted and loaded the plugin:

```bash
sudo systemctl status marine-signalk-server-container
curl -s http://127.0.0.1:3000/signalk/v1/api/plugins/signalk-lume-ti
```

In the Signal K admin web interface, navigate to **Webapps** -> **Lume TI**.

## Removal and Purge

- **Remove** (`sudo apt remove marine-lume`):
  Disables the plugin in Signal K configuration, removes plugin files, and restarts Signal K.
  The user's historical telemetry store (`.../data/data/lume-ti`) is **preserved**.

- **Purge** (`sudo apt purge marine-lume`):
  Disables and removes the plugin, deletes plugin configuration, and permanently
  removes the telemetry store directory (`.../data/data/lume-ti`).

## Building the Package

Build the package using `scripts/build-halos-debs.sh`:

```bash
scripts/build-halos-debs.sh --lume-bin /path/to/target/release/lume
```

The output `.deb` is written to `dist/halos/marine-lume_0.12.0-1_arm64.deb`.

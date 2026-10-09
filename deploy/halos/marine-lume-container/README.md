# Lume TI Server as a HaLOS container app

Runs the Lume TI ingest and query server (`lume ti ingest --serve`) in its own
container with its own memory limit, instead of as a child of the Signal K
server. It follows the HaLOS pattern used for QuestDB: a container app for the
server, and the `signalk-lume-ti` Signal K plugin (from `marine-lume`) for the
history API, SQL console, Ask tab and Library.

- **Handover:** the plugin's *Query Server Mode* decides who runs lume.
  - In `external` mode the plugin writes the arguments it would have used to
    `plugin-config-data/signalk-lume-ti/lume-ti/server.args`, one per line.
  - The container's entrypoint (`image/lume-container-run`) runs
    `lume` with them, and restarts it when the file changes or lume exits.
  - The plugin settings page keeps controlling OTLP, PostgreSQL and the token.
  - In `embedded` mode the plugin deletes the file and the container idles.
- **Install switches the mode:** `prestart.sh` (installed as `app-prestart.sh`)
  sets `serverMode` to `external` and restarts Signal K once, only when it
  changed. Set `LUME_SET_PLUGIN_EXTERNAL=false` to manage it yourself.
- **Data:** Signal K's data folder is mounted at the same path,
  `/home/node/.signalk`, and the container runs as uid 1000. The store, token,
  pgwire config and Library index stay where they were, so nothing migrates.
  The plugin still runs lume itself for the Ask tab and the Library.
- **Network:** host network, like Signal K. lume binds what the plugin asks
  for: `127.0.0.1:5863`, plus pgwire on loopback or docker0 when enabled.
  Nothing is published.
- **Limits:** `LUME_MEMORY_LIMIT=auto` is 20% of RAM, from 768 MiB to 3 GiB
  (1.6 GiB on an 8 GB Pi 5).

## Build and install

The image and package are built from a published release. Nothing is compiled:

```sh
scripts/build-lume-container.sh v0.12.1                  # into dist/halos/
scripts/build-lume-container.sh v0.12.1 --pi halos       # and install on the Pi
scripts/build-lume-container.sh v0.12.1 --pi halos --install-only
```

The first run builds a local `lume-halos-packager:trixie` image with HaLOS's
`container-packaging-tools` (about 3 minutes under emulation). Later runs take
about a minute plus the copy to the Pi.

## Removing it

Set *Query Server Mode* back to `embedded` in the plugin settings, then
`sudo apt remove marine-lume-container`. If the app is removed first, the
plugin's status says the external server is not reachable until the mode is
changed.

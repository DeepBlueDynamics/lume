# Install Lume on HaLOS

This is the install guide for a boat computer. HaLOS comes first. A plain 64-bit Linux Signal K server is in [section 9](#9-install-on-other-64-bit-linux-signal-k-servers).

The current release used below is **v0.12.2**. For a later release, change `0.12.2` everywhere it appears. The HaLOS package file always ends in `-1_arm64.deb`.

Only arm64 HaLOS and x86_64 stock Signal K are tested. The stock test was `signalk/signalk-server:latest`, Signal K 2.33.0, on x86_64 with glibc 2.39.

## 1. What you get

The `marine-lume` package adds Lume TI to the Signal K server that is already on the boat. After it is installed you get:

- A history of Signal K data on the Pi, kept in 10-second buckets.
- A SQL console, an Ask tab, a Library tab, and a status page, under **Webapps → Lume TI**.
- A Signal K History API provider named `signalk-lume-ti`, so other apps such as Freeboard can read that history. Signal K uses one default history provider. If another provider is already the default, clients must pass `provider=signalk-lume-ti`, or you set Lume as the default in **Admin → Data → Preferences**. Installing the plugin does not change that default.
- Lume's own health numbers, in a separate table called `telemetry_lume`. Those numbers are about Lume, not about the boat. The table appears only after the first data has been recorded.

The query service stays on `127.0.0.1:5863` inside the Pi. The package does not open a new port on the boat network. PostgreSQL for Grafana is off until you turn it on.

Ask uses a cloud model by default (`glm-5.3:cloud` at `https://ollama.com`). That needs an API key and an internet connection. The SQL console and the Library do not.

## 2. Install

You need a 64-bit Raspberry Pi running HaLOS, with the Signal K app `marine-signalk-server-container` already installed. The package is built for a Pi 5. Signal K must be 2.31 or newer. You do not need a compiler.

Do this from the directory where the two downloaded files are, after you copy them to the Pi.

On a computer that has the GitHub `gh` command:

```sh
VERSION=0.12.2
gh release download "v${VERSION}" -R DeepBlueDynamics/lume \
  -p 'marine-lume_*_arm64.deb' -p SHA256SUMS
scp "marine-lume_${VERSION}-1_arm64.deb" SHA256SUMS pi@halos.local:
```

`pi@halos.local` is the example login used in this repo. Use your own Pi login if it is different. If `gh` is already on the Pi, run the `gh release download` line there and skip `scp`.

Or, in a browser, open the [v0.12.2 release](https://github.com/DeepBlueDynamics/lume/releases/tag/v0.12.2) and download both `marine-lume_0.12.2-1_arm64.deb` and `SHA256SUMS`. Copy those two files to the Pi with `scp`.

On the Pi, check the file, then install it:

```sh
VERSION=0.12.2
grep "marine-lume_${VERSION}-1_arm64.deb$" SHA256SUMS | sha256sum -c -
sudo apt install "./marine-lume_${VERSION}-1_arm64.deb"
```

`sha256sum -c` must print `OK`. If it does not, do not install the file.

The package does the following, from `deploy/halos/marine-lume/debian/postinst`:

- Copies the plugin and the arm64 `lume` program to `/var/lib/container-apps/marine-signalk-server-container/data/data/lume-plugin/signalk-lume-ti`. The owner is `1000:1000`. The program is executable.
- Turns the plugin on. Settings go in `/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti.json`. The query port is `5863` on `127.0.0.1`. The Ask key path stays empty until a key file is set. Settings you already saved, including `chatApiKeyFile`, are left as they are.
- Registers the plugin in Signal K's `package.json` as `signalk-lume-ti` = `file:lume-plugin/signalk-lume-ti`.
- Links `node_modules/signalk-lume-ti` to that plugin folder.
- Restarts `marine-signalk-server-container`.

You do not have to turn the plugin on by hand. To change a setting later, open Signal K admin and go to **Server → Plugin Config**, then **Lume TI**.

Open the pages from **Webapps → Lume TI**. The plugin path on Signal K is `/signalk-lume-ti` (`signalk.appPath` in `plugins/signalk-lume-ti/package.json`). On the Pi, Signal K is at port 3000, so that page is `http://127.0.0.1:3000/signalk-lume-ti/`. From a phone or laptop, use the same Signal K admin you already use, then the Webapps menu. This repo does not include HaLOS `/etc/halos/routing.d` or `webapps.d`, so this guide does not give a public `https://` address for that page.

Before you rely on history, pin the boat's identity in **Server → Settings → Vessel Base Data**. If that identity changes, old and new history split apart.

### Approve the access request

Do this before you expect boat data. The plugin asks Signal K for a read-only device token. It stores that token itself. You never type it in.

Open **Security → Access Requests** and approve the Lume TI device. Until an admin approves it, and when anonymous read-only access is off, the plugin records nothing. The status can show `Running` with `Ingested: 0`, and the log says `Access request pending administrator approval`.

If this HaLOS server already allows anonymous read-only access, telemetry can start before that approval. Notes and the logbook still need the approved token. On a stock Signal K server, security is on, so the approval is required. See [section 9](#9-install-on-other-64-bit-linux-signal-k-servers).

## 3. Check it works

On the Pi:

```sh
dpkg -s marine-lume
```

The version line should say `0.12.2-1`.

In Signal K admin, the Lume TI plugin status should start with `Running` and show a process id, lag, how much has been ingested, and disk use. After you approve access and the boat is sending data, `Ingested` rises above 0. If Lume is in its own container instead, the line says `External server (marine-lume-container) answering on 127.0.0.1:5863`.

This asks Signal K whether the plugin is loaded:

```sh
curl -s http://127.0.0.1:3000/signalk/v1/api/plugins/signalk-lume-ti
```

This asks Lume whether its own health table answers. Run it on the Pi:

```sh
curl -s -m 5 -X POST http://127.0.0.1:5863/ti/query \
  -H 'Accept: application/json' \
  -H 'Content-Type: application/json' \
  -d '{"sql":"SELECT max(ts) AS latest_ts FROM telemetry_lume"}'
```

A JSON reply means the query service is up. That table is Lume's own counters. It is registered only after the first data has been recorded. Before that, the query says the table was not found, which is normal right after install. For boat data, open **Webapps → Lume TI**, then the SQL console, and press **Recent SOG & Wind**.

## 4. Ask tab

The Ask tab defaults to `https://ollama.com` and the model `glm-5.3:cloud`. Create a key at [ollama.com](https://ollama.com/). Then, from your computer, in a copy of this repo:

```sh
scripts/pi-set-ollama-key.sh pi@halos.local
```

The prompt hides the key. The key goes to the Pi only on the SSH connection. It is not put in the command line or in the plugin settings. On the Pi the script writes `/var/lib/container-apps/marine-signalk-server-container/data/data/plugin-config-data/signalk-lume-ti/ollama.key` (owner `1000:1000`, mode `600`), points **Chat API Key File Path** at the file as the container sees it, and restarts Signal K.

To see whether a key file is there, without printing the key:

```sh
scripts/pi-set-ollama-key.sh pi@halos.local --check
```

To use a model on the boat or on a computer on the boat network instead, set **Chat Ollama API URLs** in the plugin settings. A local server does not get the cloud key. The addresses are comma-separated, and the first one that answers and has the model is the one that is used. Example: `https://ollama.com,http://192.168.1.20:11434`.

## 5. Optional: Lume in its own container

`marine-lume` runs Lume inside Signal K. That is **Query Server Mode** `embedded`, and it is the normal install.

`marine-lume-container` runs Lume in a separate app so it has its own memory limit. The image is **not** published to a registry. You build it on a computer that has Docker, from a Lume release. Details and the memory numbers are in [the container README](../deploy/halos/marine-lume-container/README.md).

```sh
scripts/build-lume-container.sh v0.12.2 --pi pi@halos.local
```

Installing that app switches **Query Server Mode** to `external` and restarts Signal K. The plugin still owns the Ask tab and the Library. History stays in the same place.

To go back: in **Server → Plugin Config → Lume TI**, set **Query Server Mode** to `embedded`, then on the Pi run:

```sh
sudo apt remove marine-lume-container
```

If you remove the container first, the plugin status says the external server is not reachable until you set the mode back to `embedded`.

## 6. Optional: Grafana and Grub

**Grafana.** In **Webapps → Lume TI**, open **PostgreSQL / Grafana**. Turn PostgreSQL on, set the bind address to `172.17.0.1`, the port to `5864`, and the user to `grafana`. Set the password in that form, not in the ordinary plugin settings. Point Grafana at `halos.local:5864`. The steps and the reason for that address are in [PostgreSQL / Grafana on the HaLOS Pi](../plugins/signalk-lume-ti/README.md) and [bench/grafana/README.md](../bench/grafana/README.md).

**Grub.** The Library tab can use Grub Crawler to fetch web pages and PDFs onto the boat. The app is `marine-grubcrawler-container`. It listens only on `127.0.0.1:6792`. Install notes are in [the Grub container README](../deploy/halos/marine-grubcrawler-container/README.md). Pages already fetched into the Library stay on the Pi. Grub is what fetches new pages.

## 7. Upgrade and uninstall

Upgrade with the newer package, the same way as the first install: check `SHA256SUMS`, then:

```sh
sudo apt install ./marine-lume_NEWER-1_arm64.deb
```

Replace `NEWER` with the new version, for example `0.12.3`. An upgrade restarts Signal K. It keeps the history store and the settings you already saved, including the Ask key file path.

To remove the plugin and keep the history:

```sh
sudo apt remove marine-lume
```

That turns the plugin off, removes the plugin files and the `node_modules` link, and restarts Signal K. The history stays in `/var/lib/container-apps/marine-signalk-server-container/data/data/lume-ti`.

To remove the history and the plugin settings as well, including the Ask key file:

```sh
sudo apt purge marine-lume
```

Purge deletes that store directory, `plugin-config-data/signalk-lume-ti.json`, and `plugin-config-data/signalk-lume-ti/`.

## 8. Troubleshooting

**The Lume page is not found.** Open it from **Webapps → Lume TI**. Do not guess a `https://` address. On the Pi the Signal K path is `http://127.0.0.1:3000/signalk-lume-ti/`. This repo does not document HaLOS's public route for that path.

**No boat data.** In **Security → Access Requests**, approve the Lume TI device. Until that approval, when anonymous read-only access is off, the status stays on `Running` with `Ingested: 0`, and the log says `Access request pending administrator approval`. The plugin asks for read-only access and stores the token itself. After approval, the SQL console preset **Recent SOG & Wind** should show rows once the boat has sent data. Also confirm the vessel identity in **Server → Settings → Vessel Base Data** has not changed.

**The health query says the table was not found.** `telemetry_lume` is created with the first recorded data. Wait until the plugin has been running and approved, then run the query in [section 3](#3-check-it-works) again.

**Ask stays on "Thinking" and never answers.** The tab shows `Thinking…` while it waits. The cloud model needs a key file and a path to the internet. If **Chat API Key File Path** is set and that file cannot be read, the error names `chatApiKeyFile`. From your computer run:

```sh
scripts/pi-set-ollama-key.sh pi@halos.local --check
```

If it reports no key file, run the same command without `--check`. A local model needs **Chat Ollama API URLs** pointed at that server, and it does not use the cloud key.

**A log line about the logbook.** When the signalk-logbook plugin is not installed, the log can show `Signal K document poll: Signal K document request failed: /plugins/signalk-logbook/logs`, or `Signal K document source not present: logbook`. That does not stop recording. A quieter handling of this line is tracked.

**`halos.local` does not open.** The examples in this repo use `halos.local`. If your computer cannot find that name, use the Pi's IP address in `scp`, `ssh`, and the key script.

## 9. Install on other 64-bit Linux Signal K servers

This is the shorter path for a Signal K server that is not the HaLOS package. It was tested on `signalk/signalk-server:latest` (Signal K 2.33.0, x86_64, glibc 2.39). Only that stock server, and arm64 HaLOS, are tested.

You need 64-bit Linux, x86_64 or arm64, with glibc 2.35 or newer. In the v0.12.2 plugin tarball, `bin/manifest.json` records `glibc_max` `2.35` for both binaries. The packager refuses anything above 2.39. Signal K must be 2.31 or newer, and Node.js must be 18 or newer. The tested server already had Node.

Not supported: 32-bit Raspberry Pi OS, Victron Venus OS (armv7), macOS, and Windows. The plugin package is Linux arm64 and x64 only. Lume's own command-line downloads for other systems are a different install, on the main README.

The v0.12.2 file `signalk-lume-ti-0.12.2.tgz` is 70,389,599 bytes, about 70 MB, because it bundles both the arm64 and the x64 programs. The files inside add up to 191,819,263 bytes, about 183 MiB. On the test server the install was about 184 MB.

On a computer that has `gh`:

```sh
VERSION=0.12.2
gh release download "v${VERSION}" -R DeepBlueDynamics/lume \
  -p "signalk-lume-ti-${VERSION}.tgz" -p SHA256SUMS
grep "signalk-lume-ti-${VERSION}.tgz$" SHA256SUMS | sha256sum -c -
```

`sha256sum -c` must print `OK`. Keep the `.tgz`. Do not delete it after install.

Open a shell as the user Signal K runs as, in that user's `~/.signalk`. In the official Signal K container, that directory is inside the container.

```sh
cd ~/.signalk
npm install "/path/signalk-lume-ti-0.12.2.tgz"
```

Use the real path of the file you checked. On the test server this finished in about 5 seconds. npm records a `file:` dependency in `~/.signalk/package.json`, so a later install looks for that same `.tgz`.

Restart Signal K the way you usually restart it. Then enable **Lume TI** in **Server → Plugin Config**. This path does not enable the plugin for you. The HaLOS package does.

**Approve the access request before you expect data.** Security is on by default on this stock server. Open **Security → Access Requests** and approve Lume TI. Until you do, the status shows `Running` with `Ingested: 0`, and the log says `Access request pending administrator approval`. Nothing is recorded until then.

Signal K uses one default History API provider. If another provider is the default, clients must pass `provider=signalk-lume-ti`, or you choose Lume in **Admin → Data → Preferences**.

`telemetry_lume` appears only after the first data has been recorded. Querying it earlier says the table was not found.

The same troubleshooting in [section 8](#8-troubleshooting) applies, including the harmless logbook line. The `marine-lume` package commands in the earlier sections are for HaLOS only.

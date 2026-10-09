# Email draft: marine-lume for HaLOS

**Draft status:** the package builder and release pipeline are added; the first
release asset and a verified HaLOS installation are still pending.

**Subject: marine-lume — boat history and document search for HaLOS**

Hi,

I'm building **marine-lume**, a boat history and document search tool for HaLOS.
One arm64 Debian package will add the existing Lume plugin and binary to your
running Signal K server. It needs no app store or extra container.

It lets you:

- Keep supported Signal K telemetry and alerts as searchable history on the Pi.
- Ask plain-English questions and see the SQL the assistant actually ran.
- Run SQL directly, or connect Grafana through an optional PostgreSQL interface.
- Provide history to Freeboard and other Signal K History API clients.
- Search your own manuals and documents in the Library tab, offline after indexing.

Once a release containing the package is published, download its asset and
checksum file, verify it, then install. Replace `0.12.0` with that release's
version; the Debian package adds `-1`:

```sh
VERSION=0.12.0
BASE="https://github.com/DeepBlueDynamics/lume/releases/download/v${VERSION}"
curl -fLO "${BASE}/marine-lume_${VERSION}-1_arm64.deb"
curl -fLO "${BASE}/SHA256SUMS"
awk -v file="marine-lume_${VERSION}-1_arm64.deb" '$2 == file' SHA256SUMS | sha256sum --check --strict - &&
  sudo apt install "./marine-lume_${VERSION}-1_arm64.deb"
```

These commands depend on the first package release, which is still pending.

After installation, open **Signal K → Webapps → Lume**. Enable recording and
approve its Signal K read-only access request if required. Keep the vessel's
configured identity stable. For Ask, supply an Ollama cloud API key; the current
default is GLM through ollama.com. Ordinary queries and indexed Library search
work without that cloud service.

Removal will keep your history; purge will delete it:

```sh
sudo apt remove marine-lume
# Only when you want to delete its saved data:
sudo apt purge marine-lume
```

**Requirements:** a 64-bit Raspberry Pi 5 running HaLOS with Signal K 2.31+.
The current target is an 8 GB Pi. It needs persistent writable disk space for
history and documents; capacity depends on recording duration and Library size.
No marine HAT, Rust compiler or local language model is required.

**System changes:** installs the plugin and arm64 binary in Signal K's persistent
data area and restarts Signal K. Lume's query service stays on
`127.0.0.1:5863`; users reach it through the existing Signal K web proxy.
The default install opens no new network-facing ports. It saves its Signal K
token and history under Signal K's data directory. Grafana access is optional
and requires separate PostgreSQL listener/password setup.

For a hardware-free demo, Skiff on a PC simulates a Lagoon 450S and sends
Signal K data to `halos.local:3000` over WebSocket. Approve its readwrite
access token once. Lume records that stream; OpenCPN reads the same Signal K
server and can send route guidance back to Skiff.

The plugin has been deployed on our Pi, but the Debian install/upgrade/remove
flow still needs testing. History normally stores 10-second bucket aggregates,
not every original sample or every Signal K value type. Ask needs internet for
the cloud model, sends questions and query results to that service, and can be
wrong. This first release does not include fleet/shore features, a separate app,
public exposure, a required local model or bundled copyrighted documents.

Repository: [github.com/DeepBlueDynamics/lume](https://github.com/DeepBlueDynamics/lume)

Would you be interested in trying it and reviewing the HaLOS integration?

## Still to do before the email

- [x] Pipeline added: build the arm64 `marine-lume` package and include it in
  release checksums. **First release pending**; confirm the asset download and
  replace the example version above with the published version.
- Test install, upgrade and rollback against HaLOS's persistent Signal K layout,
  plugin registration and restart. Set the visible Webapps name to **Lume**.
- Test that remove preserves data and purge deletes only Lume-owned data;
  document the exact paths and backup procedure.
- Run the Skiff demo on the Pi, including token approval, OpenCPN, Library and
  Ask. Measure installation space and recording growth; report realistic limits.

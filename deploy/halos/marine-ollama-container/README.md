# Ollama as a HaLOS container app

Runs [Ollama](https://ollama.com/) on a HaLOS Pi next to the Lume TI Signal K
plugin. The plugin's **Ask** tab sends `lume chat` to it, so you can ask questions
of telemetry and the cruiser library in plain words and get SQL-backed answers.

- **Image:** `ollama/ollama:latest` (multi-arch, arm64 on the Pi). Pin a version
  through `OLLAMA_IMAGE` for reproducible installs.
- **Network:** `127.0.0.1:11434` only. Ollama has no auth, so it must not be
  published to the LAN. The Signal K container uses the host network and reaches it
  on loopback, which is the Ask tab's default URL.
- **Models:** stored in `/var/lib/container-apps/marine-ollama-container/data/ollama`
  and kept across image updates. The installer pulls `OLLAMA_DEFAULT_MODEL`
  (`qwen3:4b`, about 2.5 GB). Pass `OLLAMA_DEFAULT_MODEL=-` to skip that.
- **Memory:** `lume chat` asks for a 16k context. Flash attention with an 8-bit KV
  cache keeps a 4B model plus that context within a 4 GB cap. Only one model is
  loaded and one request handled at a time, and the model unloads after 10 minutes
  idle.
- **Layout:** `install.sh` writes the same files and systemd unit that HaLOS's
  container-packaging-tools write for `marine-*-container` packages. It can also
  be installed directly as a `.deb` package to appear in Cockpit's container store.

## Installing the .deb

Build the package with `scripts/build-halos-debs.sh` or copy the `.deb` from `dist/halos/`:

```sh
# on the Pi
sudo docker pull ollama/ollama:latest
sudo dpkg -i marine-ollama-container_0.1.0-1_arm64.deb
sudo systemctl status marine-ollama-container
# Pull the default offline model (qwen3:4b):
docker exec ollama ollama pull qwen3:4b
sudo docker exec ollama ollama list
```

## Manual installation (via install.sh)

```sh
# on the Pi
sudo docker pull ollama/ollama:latest
sudo ./install.sh                         # installs, enables, waits, pulls qwen3:4b
sudo systemctl status marine-ollama-container
sudo docker exec ollama ollama list
```

## Three ways to answer, in order

The Ask tab's **Chat Ollama API URLs** setting takes a comma-separated list. It tries
each URL in turn and uses the first reachable one that already has the model.

1. **The Pi's own Ollama,** `http://127.0.0.1:11434`. This works fully offline with
   the local model.
2. **Cloud models through the same Ollama:** once you have signed in on the Pi,
   `:cloud` models such as `glm-5.3:cloud` work whenever the boat has internet. Lume
   never sees the key.

   ```sh
   sudo docker exec -it ollama ollama signin       # interactive: opens a sign-in link
   sudo docker exec ollama ollama pull glm-5.3:cloud
   ```

3. **A laptop's Ollama on the LAN,** for example `http://192.168.68.58:11434`. On
   the laptop, set `OLLAMA_HOST=0.0.0.0`, restart Ollama and allow TCP 11434 for the
   private network only. That Ollama has no auth either.

For example: `http://127.0.0.1:11434,http://192.168.68.58:11434`.

Settings live in `/etc/container-apps/marine-ollama-container/env` and override
`env.defaults`: `OLLAMA_IMAGE`, `OLLAMA_MEMORY_LIMIT`, `OLLAMA_KEEP_ALIVE`,
`OLLAMA_DEFAULT_MODEL`.

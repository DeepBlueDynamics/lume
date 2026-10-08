# Ollama as a HaLOS container app (cloud models for the Ask tab)

Runs [Ollama](https://ollama.com/) on a HaLOS Pi next to the Lume TI Signal K
plugin, **as a gateway to Ollama's cloud models**. The plugin's **Ask** tab sends
`lume chat` to it at `127.0.0.1:11434`. After a one-time `ollama signin` on the Pi,
models such as `glm-5.3:cloud` run on Ollama's servers whenever the boat has
internet. Lume never holds a key.

The Pi does not run models locally by default. Measured on a Pi 5 on 2026-10-08,
`qwen3:1.7b` produced 0.34 tokens/s at the Ask tab's 16k context: about 100 s for a
one-sentence answer. Local inference belongs on a laptop or a shore machine (see the
endpoint list below).

- **Image:** `ollama/ollama:latest`, multi-arch. The arm64 image is **4.2 GB**
  (Ollama 0.40.1), most of it GPU libraries the Pi does not use, and it pulled at about
  1 MB/s. Check free space first. Pin a version through `OLLAMA_IMAGE`.
- **Network:** `127.0.0.1:11434` only. Ollama has no auth, so it must not be published
  to the LAN. The Signal K container uses the host network and reaches it on loopback,
  which is the Ask tab's default URL.
- **Data:** the sign-in key and cloud-model stubs live in
  `/var/lib/container-apps/marine-ollama-container/data/ollama` and survive image
  updates.
- **Memory:** `OLLAMA_MEMORY_LIMIT` defaults to 1g, plenty for a proxy. **The HaLOS
  Pi boots with `cgroup_disable=memory`, so Docker enforces no memory limit on any
  container there**, this one, Grub and HaLOS's own apps included. CPU limits
  (`cpus: 3`) do apply.
- **Layout:** `install.sh` writes the same files and systemd unit that HaLOS's
  container-packaging-tools write. `scripts/build-halos-debs.sh` builds the `.deb`,
  which registers with Cockpit's container store.

## Install

```sh
# on the Pi
sudo docker pull ollama/ollama:latest
sudo dpkg -i marine-ollama-container_0.1.0-1_arm64.deb   # or: sudo ./install.sh
sudo systemctl status marine-ollama-container
```

## Sign in once and add cloud models

```sh
sudo docker exec -it ollama ollama signin            # prints a link: open it and approve
sudo docker exec ollama ollama pull glm-5.3:cloud     # small stub; the model runs in the cloud
sudo docker exec ollama ollama list
```

Then, in **Signal K → Server → Plugin Config → Lume TI**, set **Chat Ollama Model**
to the cloud model, for example `glm-5.3:cloud`. With only Lume MCP tools, glm-5.3
answered 17 of 20 fleet questions (benchmark report §6). The sign-in persists across
restarts and updates. To sign out: `sudo docker exec -it ollama ollama signout`.

## Endpoint order

The Ask tab's **Chat Ollama API URLs** setting takes a comma-separated list. It tries
each URL in turn and uses the first reachable one that has the model.

1. **The Pi's Ollama,** `http://127.0.0.1:11434`: cloud models, whenever the boat
   has internet.
2. **A laptop's Ollama on the LAN,** for example `http://192.168.68.58:11434`: local
   models with no internet. On the laptop, set `OLLAMA_HOST=0.0.0.0`, restart
   Ollama and allow TCP 11434 for the private network only. That Ollama has no auth
   either.

For example: `http://127.0.0.1:11434,http://192.168.68.58:11434`.

Settings live in `/etc/container-apps/marine-ollama-container/env` and override
`env.defaults`: `OLLAMA_IMAGE`, `OLLAMA_MEMORY_LIMIT`, `OLLAMA_KEEP_ALIVE`,
`OLLAMA_DEFAULT_MODEL` (`-` means no local model).

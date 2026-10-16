![Lume](https://raw.githubusercontent.com/DeepBlueDynamics/lume/main/assets/lume-icon-96.svg)

# Lume TI for Signal K

**Your boat's history, live gauges and your manuals, all on the boat.** Lume TI records everything Signal K sees, draws it, and answers questions about it, with no cloud database and no new ports on the boat network.

## Highlights

- 🧭 **Ask for a gauge, get a live one.** "Show the true wind as a dial." "Draw a compass." "Make a navigation-lights panel." Ask builds the widget right in the answer, wired to live Signal K data. Built-in styles: tank, dial, arc, bar, digital readout and line graph, plus custom drawings for anything else.
- 📊 **A Dashboard tab.** Press **Add to dashboard** under any widget and it stays, live, on a phone or a chart-table screen.
- 📈 **Graphs and tracks.** Any query with a time column draws itself as a chart. Positions draw as a track with a scale bar. "Graph the wind for the last 24 hours" just works.
- 🕰️ **Months of history.** Speed, depth, wind, position, engine and battery data, summarised every 10 seconds (average, low, high and latest), queryable with SQL.
- 📚 **An offline Library.** Pick from about 470 free references (navigation rules, Bowditch, first aid, engine manuals) and Lume downloads and indexes them. Search results open at the right page, with no internet.
- 🚨 **Alerts bring up the right pages.** Shallow water, anchor drag, fire, an engine alarm: the Library shows the relevant pages.
- 💬 **Ask in plain English** about your data or your manuals, using `glm-5.3:cloud` at ollama.com (save your key in the Ask tab) or a model on your own network.
- 🔌 **Plays well with others:** a History API provider for KIP and Freeboard, and an optional PostgreSQL endpoint for Grafana.

Dashboards and widgets are brand new in 0.16, and more gauge styles are on the way.

## Install

1. In Signal K admin, open **Appstore → Available**, find **Lume TI**, press **Install**, then **Restart**.
2. In **Server → Plugin Config → Lume TI**, tick **Enabled** and press **Submit**.
3. In **Security → Access Requests**, approve the Lume TI device.
4. Open **Webapps → Lume TI**.

Or with npm, as the Signal K user: `cd ~/.signalk && npm install signalk-lume-ti`, then restart.

Needs 64-bit Linux (Raspberry Pi arm64 or x64 PC), Signal K 2.31+ and Node 18+. npm pulls in the one ~35 MB `lume` program your machine needs; there's no compiler and no install script. On HaLOS, see [Install Lume on HaLOS](https://github.com/DeepBlueDynamics/lume/blob/main/docs/HALOS.md).

## Connect hertz

To receive VHF/NOAA radio logs and agent telemetry from `hertz` over OTLP:

1. In **Server → Plugin Config → Lume TI**, enable **Accept radio and agent logs from hertz (OTLP)** (`otlpEnabled`) and press **Submit**.
2. Lume TI creates `<plugin-config-data>/signalk-lume-ti/otlp.token` (32 random bytes hex, mode `0600`) if missing and writes `<plugin-config-data>/signalk-lume-ti/serve.json` on bind.
3. The **Status & Diagnostics** tab shows **OTLP receiver: on / off** and the token file path (never the token contents).
4. **HaLOS / container caveat:** `lume` binds loopback (`127.0.0.1:5863`) inside the Signal K container. When `hertz` runs in a separate container, use host networking (or the container gateway bind) and mount the token file (`otlp.token`) into the `hertz` container.
5. **Discovery file notes:** If two standalone `lume serve` processes run on one machine without `--discovery-file <path>`, both write `~/.lume/serve.json` and the last one started wins; on Windows Ctrl-C has no signal handler and leaves a stale file. Clients reading `serve.json` should check that `pid` is alive or probe `/health`.

## More

- [Troubleshooting](https://github.com/DeepBlueDynamics/lume/blob/main/docs/HALOS.md#8-troubleshooting)
- [Technical reference](https://github.com/DeepBlueDynamics/lume/blob/main/plugins/signalk-lume-ti/REFERENCE.md): History API, Grafana setup, building, testing
- [Lume](https://github.com/DeepBlueDynamics/lume), the small Rust program underneath
- [Issues](https://github.com/DeepBlueDynamics/lume/issues) · License: BSD-3-Clause

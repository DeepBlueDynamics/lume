# Alert → library reference searches

Lume TI turns conditions in the telemetry into **alerts** (rule alerts from `ti.toml` `[[rules]]`, plus Signal K
notifications). Alerts are documents (`docs.kind = 'alerts'`), so they are searchable and joinable with the telemetry
that raised them. Each active alert can then **trigger a search of the cruiser library**
(`docs/cruiser_library.csv`, indexed from the plugin's Library tab) to put the relevant manual page in front of the crew.

The plugin does this automatically: **Library → Alert references** maps every active alert to a query using
`plugins/signalk-lume-ti/library/alert_references.json` and shows the top pages with title, publisher and source URL.

## Rules that fire on the data we have

The Pi's replayed N2K data (own vessel, 2026-10-06/07) covers depth 10.6–71 m, apparent wind 4.9–9.6 m/s, SOG 2.9–3.8 m/s,
battery 12.5–14.6 V and 0–233 A. These rules are tuned to fire on it. Real thresholds belong in a boat's own `ti.toml`.

```toml
[[rules]]
name = "shallow-water"
severity = "warn"
when = "\"environment.depth.belowTransducer@min\" < 12"
for = "20s"
message = "Depth {environment.depth.belowTransducer@min} m"

[[rules]]
name = "battery-low"
severity = "warn"
when = "\"electrical.batteries.1.voltage@min\" < 12.6"
for = "30s"
message = "House battery {electrical.batteries.1.voltage@min} V"

[[rules]]
name = "battery-high-current"
severity = "warn"
when = "\"electrical.batteries.1.current@max\" > 200"
for = "10s"
message = "Battery current {electrical.batteries.1.current@max} A"

[[rules]]
name = "wind-rising"
severity = "info"
when = "\"environment.wind.speedApparent@max\" > 9"
for = "1m"
message = "Apparent wind {environment.wind.speedApparent@max} m/s"
```

## Example alert-triggered searches

| Alert (rule or notification) | Library search it triggers | References it should surface (defaults in **bold**) |
|---|---|---|
| `shallow-water`, `notifications.environment.depth.*` | shallow water grounding aground refloat kedge anchor | **Navigation Rules handbook**, **IALA buoyage**, anchoring guides |
| `battery-low`, `notifications.electrical.batteries.*` | battery voltage low charging alternator state of charge | **Battle Born 12 V battery manual**, NEETS electrical modules |
| `battery-high-current` | battery voltage low charging alternator state of charge | **Battle Born manual** (current limits, BMS behaviour) |
| `wind-rising`, `notifications.environment.wind.*` | gale warning heavy weather storm reef heave to forecast | **Mariner's Guide to Marine Weather Services**, NWS observing handbook |
| `notifications.navigation.anchor` (anchor drag) | anchor dragging scope holding ground reset anchor | Rocna anchor guide, seamanship texts |
| `notifications.mob` | man overboard recovery williamson turn lifesling | **Abandon-ship / safety procedures**, seamanship |
| `notifications.fire` | fire on board extinguisher engine room fire fighting | Fireman rate training manual |
| `notifications.flooding`, bilge alarms | flooding bilge pump leak damage control sinking | damage-control and hull repair texts |
| `notifications.abandon` | abandon ship liferaft grab bag epirb | **Abandon Ship template** (Transport Canada) |
| `notifications.navigation.closestApproach` (AIS CPA) | collision risk give way stand on vessel restricted visibility | **Navigation Rules handbook** (Rules 7–19) |
| `notifications.propulsion.*.overTemperature` / oil pressure | engine overheating coolant raw water pump impeller oil pressure | Engineman rate training manuals |
| CO / gas detector | carbon monoxide propane gas leak ventilation | safety texts |
| Medical log entry or note | first aid casualty hypothermia bleeding | **Ship Captain's Medical Guide ch. 1**, WHO medical guide |
| `notifications.*.dsc` / distress | distress call mayday dsc vhf channel 16 procedure | **VHF-DSC radio guide**, GMDSS guides |

## SQL you can run

Alerts in the last 24 hours, with the conditions that raised them:

```sql
SELECT d.title, d.ts_start, d.ts_end,
       min(t."environment.depth.belowTransducer@min") AS min_depth_m,
       min(t."electrical.batteries.1.voltage@min")    AS min_volts
FROM docs d
JOIN telemetry t ON t.vessel = d.vessel AND t.ts >= d.ts_start AND t.ts < coalesce(d.ts_end, now())
WHERE d.kind = 'alerts' AND d.ts_start > now() - INTERVAL '24 hours'
GROUP BY 1, 2, 3 ORDER BY d.ts_start DESC;
```

Search the alerts themselves (alerts are documents):

```sql
SELECT title, ts_start FROM docs WHERE kind = 'alerts' AND match(body, 'battery') ORDER BY ts_start DESC;
```

Library pages for a battery alert (CLI today, `lume sql` over the library index):

```sh
lume sql --db <dataDir>/library/index \
  "SELECT file, title, score FROM sections WHERE match(body, 'battery voltage low charging') ORDER BY score DESC LIMIT 5" --format json
```

Once `--docs-index` lands on the serving process (`ti/library-index`), the same `sections` table is queryable next to
`docs` and `telemetry` in one SQL statement over HTTP and Grafana.

## Adding a mapping

Append a rule to `plugins/signalk-lume-ti/library/alert_references.json`. `match` is a case-insensitive regular
expression tested against the alert id/path and title; the first match wins; `query` is plain words for BM25.

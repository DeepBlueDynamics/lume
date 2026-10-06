# 2. Pilot vessel (PV-1)

Spec pp. 2–5.

PV-1 is the v1 reference boat: first real dataset, first install, and the test of
whether Lume TI plugs into a Pi in an afternoon. Privately owned 16 m aluminium
hybrid catamaran in fit-out, replacing the owner's previous boat. Electronics
were still being chosen as of February 2026, so ingest must work with whatever
lands on the NMEA 2000 backbone.

## Boat computer

Hat Labs HALPI. If HALPI2: Raspberry Pi CM5 in an IP65 aluminium case, NVMe SSD,
isolated CAN-FD (NMEA 2000), isolated RS-485 (NMEA 0183), 10–32 V or N2K bus power.
Default OS HaLOS is container-based; its Marine image ships Signal K, Grafana,
InfluxDB and AvNav behind Cockpit, Homarr and SSO. A desktop image with Signal K +
OpenCPN is the alternative.

Consequences for the build:

- **Ingest:** N2K reaches Signal K straight from `can0`; no gateway hardware.
- **Backfill:** existing history is in InfluxDB, not Parquet → need an InfluxDB backfill reader ([W3](../lanes/W3-ingest.md)).
- **Install:** on HaLOS the primary path is a container app, not a process spawned from a Signal K plugin ([W7](../lanes/W7-serve.md)).

## Systems

| System | Equipment (status) | Reaches Signal K via | Main paths | TI fields |
|---|---|---|---|---|
| Propulsion | Nanni N4.80 diesel + 2 × 25 kW electric (published) | N2K engine gateway; format to confirm with Nanni | `propulsion.*.revolutions`, `.temperature`, `.state`, motor power | `bsi`, `set`, `count` (`@starts`) |
| Energy storage | 40 kWh bank (published); Victron Multiplus 24 V on previous boat | Victron VE.Direct or Venus OS via signalk-venus-plugin | `electrical.batteries.*.voltage`, `.current`, `.stateOfCharge` | `bsi` |
| Solar | 3.75 kWp (published); Victron SmartSolar MPPT on previous boat | as above | `electrical.solar.*.panelPower`, yield | `bsi` |
| Navigation | B&G Zeus 3S, Halo 20+, Triton 2 on previous boat; Oct 2025 research recommends B&G for 2.0 | HALPI native CAN (`can0`) | `navigation.position`, `.speedOverGround`, `.courseOverGroundTrue`, `.headingMagnetic` | `bsi`, `geo` |
| Instruments | Maretron N2K wind, depth, speed, baro, temp, humidity on previous boat | N2K | `environment.wind.*`, `environment.depth.*`, `environment.outside.*` | `bsi` |
| Water | 900 L tankage with roof rain catchment (published) | N2K tank senders | `tanks.freshWater.*.currentLevel` | `bsi` (`slow` profile) |
| AIS and VHF | AIS on previous boat; Meridian VHF monitor proposed | N2K; Meridian publishes transcripts | AIS contexts; transcripts as Lume documents | `set`, `text` |
| Satellite link | Viasat Sailor 600 on previous boat; Starlink unconfirmed | n/a | link state for `ti-sync` | — |

## Queries this boat makes worth it

Energy boat: 40 kWh, 3.75 kWp solar, induction cooking (previous boat), hybrid
drive. First saved webapp queries should answer energy questions. Path names are
illustrative until the N2K list is final.

```sql
-- Daily energy balance, 10 s buckets: mean W × 10 s / 3.6e6 = kWh
SELECT date_trunc('day', ts) AS day,
       sum("electrical.solar.house.panelPower") * 10 / 3.6e6 AS solar_kwh,
       sum(coalesce("propulsion.port.motorPower", 0)
         + coalesce("propulsion.starboard.motorPower", 0)) * 10 / 3.6e6 AS motor_kwh,
       min("electrical.batteries.house.stateOfCharge@min") AS min_soc
FROM telemetry
WHERE ts >= now() - INTERVAL '30 days'
GROUP BY day ORDER BY day;

-- Electric-only motoring: motors pulling, diesel off, at least 10 minutes
SELECT * FROM intervals(
  '"propulsion.port.motorPower" > 500 AND "propulsion.main.state" != ''started''',
  min_len => '10m');
```

Other likely owner questions:

- How many miles did we do on sun alone this month?
- What did the rain catchment add?
- Which anchorages had VHF traffic mentioning weather while the wind was over 25 kn?

## Assumptions to confirm with the owner

- [ ] Boat computer: exact HALPI model (Hat Labs documents HALPI2; no "HALPI-M" found), CM5 RAM size, OS image (HaLOS Marine vs desktop + OpenCPN), and whether OpenCPN runs on the same HALPI as Signal K (believed so; see [03-single-box-budget](03-single-box-budget.md)).
- [ ] Final NMEA 2000 equipment list: MFD brand, instrument vendor, engine gateway.
- [ ] Whether Victron stays on 2.0, and whether a Cerbo GX or Venus OS device is aboard.
- [ ] Consent to record 90 days of data for the benchmark dataset, and what may be shared publicly.
- [ ] Satellite link (Starlink or Viasat) and the data budget `ti-sync` may use.

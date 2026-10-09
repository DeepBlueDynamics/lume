# Skiff feed into Lume

Run Skiff on the Windows PC. It pushes Signal K deltas to the Pi at `halos.local:3000`. Lume is already the `signalk-lume-ti` plugin inside that Signal K server. Open Webapps, then Lume TI, to query.

This runbook does not start Skiff, does not contact the Pi, and does not stop the sample replay. Those steps are for the live demo.

Skiff itself is not edited. The client facts below are from `src/signalk.rs` and `src/main.rs` in `C:\Users\kordl\Code\DeepBlueDynamics\skiff`.

## 1. Start Skiff pointed at the Pi

Once, if `web\dist` is missing, build the local page from the Skiff repo:

```powershell
cd C:\Users\kordl\Code\DeepBlueDynamics\skiff\web
npm install
npm run build
```

The delta feed does not need the browser. The simulator keeps running headless. The page is `http://localhost:18081/` unless `SKIFF_PORT` or `PORT` is set.

Get a token with the helper in the next section, then start Skiff:

```powershell
cd C:\Users\kordl\Code\DeepBlueDynamics\skiff
$env:SIGNALK_HOST = "halos.local:3000"
$env:SIGNALK_TOKEN = "<token printed by scripts/skiff-to-pi.ps1>"
cargo run --bin skiff
```

`SIGNALK_HOST` is `host[:port]`. A scheme is optional and is ignored. The client always opens:

```text
ws://<host>/signalk/v1/stream?subscribe=none
```

`wss://` is not implemented. If `SIGNALK_HOST` is unset, Skiff warns and sends no deltas. If `SIGNALK_TOKEN` is unset or empty, it connects with no `Authorization` header. A token is sent as `Authorization: Bearer <token>` on the websocket handshake.

Deltas are websocket text frames. Skiff does not POST them to the HTTP API.

The connection loop retries on its own. Backoff starts at one second and resets after a successful connect. A successful connect logs `SignalK connected:` plus that `ws://` URL.

When the guidance channel is set, Skiff also subscribes on context `vessels.self` to `navigation.*` and `steering.*` (period 1000, policy instant). That subscription is inbound course guidance. The demo feed is the outbound delta below.

## 2. Get a readwrite token

Skiff never calls `/signalk/v1/access/requests`. Nothing in the Skiff tree requests access. Put a token in `SIGNALK_TOKEN` before `cargo run`.

The Lume plugin requests `readonly` for its own client. That token cannot send deltas. Do not copy it.

On the Windows PC, run `scripts/skiff-to-pi.ps1` from this branch (`docs/skiff-feed`). The helper only talks to the access-request API. It does not start Skiff, and it does not write the token to disk.

```powershell
powershell -NoProfile -File .\scripts\skiff-to-pi.ps1 -SignalKHost halos.local:3000
```

What it does:

1. Reuses a client UUID stored in `%USERPROFILE%\.skiff\signalk-client-id.json`, or creates one. That file is not the token, and it is outside the repo.
2. POSTs `http://halos.local:3000/signalk/v1/access/requests` with `permissions` set to `readwrite` and description `Skiff sailing simulator`.
3. Asks you to approve the request in the Signal K admin: Security > Access Requests.
4. Polls the returned `href` for up to five minutes, until the state is `COMPLETED` or `DENIED`.
5. Prints three lines to paste into the Skiff window: `SIGNALK_HOST`, `SIGNALK_TOKEN`, and `cargo run --bin skiff`.

Approve the request while the script is polling. A denied request exits with an error. A timeout leaves the request pending; approve it and run the script again. The same client id is reused.

## 3. Confirm the data

In Signal K:

- The Skiff log contains `SignalK connected: ws://halos.local:3000/signalk/v1/stream?subscribe=none`.
- In the admin Data Browser, `navigation.speedOverGround` on the self vessel shows source `sailing-simulator@<COMPUTERNAME>`.
- Or, with the same Bearer token:

```text
curl -s -H "Authorization: Bearer <token>" http://halos.local:3000/signalk/v1/api/vessels/self/navigation/speedOverGround
```

Skiff sends `context` `vessels.self`. The server merges that onto its pinned self vessel. Lume does the same: a context of `vessels.self` is stored as the ingest self URN (`crates/ti-ingest/src/decode.rs`). Skiff does not have a separate vessel id.

Read the live self before querying:

```text
curl -s http://halos.local:3000/signalk
```

Use the `self` field as the SQL `vessel` value when it already starts with `vessels.`. When it is a bare `urn:...`, prefix `vessels.`. Lume's ingest hello does that canonicalization (`crates/ti-ingest/src/service.rs`).

`plan/SETUP.md` records the lead's pinned Pi identity as `urn:mrn:signalk:uuid:0eb191d0-1f5a-42da-979e-ead792d676ee`. That is an example from that Pi, not a universal id. Confirm `self` on the server you are using. For that example the vessel string is `vessels.urn:mrn:signalk:uuid:0eb191d0-1f5a-42da-979e-ead792d676ee`.

In Lume TI:

```sql
SELECT max(ts) FROM telemetry
WHERE vessel = 'vessels.urn:mrn:signalk:uuid:<self-uuid>';
```

`max(ts)` moves for every source on that vessel, including the N2K sample replay. To see Skiff rows:

```sql
SELECT max(ts) FROM telemetry
WHERE vessel = 'vessels.urn:mrn:signalk:uuid:<self-uuid>'
  AND "navigation.speedOverGround$source" = 'sailing-simulator@<COMPUTERNAME>';
```

The source column is a list. SQL equality checks membership. The label is `sailing-simulator@` plus `COMPUTERNAME`, then `HOSTNAME`, then `unknown`. There is no `src`, CAN name, or talker on the delta, so the stored source is that label alone.

Rows only on `vessels.urn:mrn:imo:mmsi:000000000` mean Lume has not applied the server hello yet. That string is the ingest placeholder.

The webapp preset queries do not filter by vessel or source. On this Pi they show the self vessel, sample replay included, until that connection is disabled.

## 4. Paths Skiff sends, and what Lume stores

One delta, about once per simulator step:

| Signal K path | Value | Lume column |
| --- | --- | --- |
| `navigation.position` | `{ latitude, longitude }` in degrees | `navigation.position.latitude`, `navigation.position.longitude`, plus a geo value on `navigation.position` |
| `navigation.headingTrue` | radians | same path |
| `navigation.speedThroughWater` | m/s | same path |
| `navigation.speedOverGround` | m/s | same path |
| `navigation.courseOverGroundTrue` | radians | same path |
| `navigation.leewayAngle` | radians | same path |
| `navigation.attitude` | `{ roll, pitch, yaw }` radians | `navigation.attitude.roll`, `.pitch`, `.yaw` |
| `steering.rudderAngle` | radians | same path |
| `environment.current.drift` | m/s | same path |
| `environment.current.setTrue` | radians | same path |
| `environment.wind.speedApparent` | m/s | same path |
| `environment.wind.angleApparent` | radians | same path |
| `environment.wind.speedTrue` | m/s | same path |
| `environment.wind.directionTrue` | radians | same path |
| `environment.wind.angleTrueWater` | radians | same path |
| `environment.depth.belowSurface` | meters | same path |
| `environment.depth.belowKeel` | meters | same path |
| `tanks.fuel.0.currentLevel` | ratio 0..1, name `Port` | same path |
| `tanks.fuel.0.currentVolume` | m³ (liters / 1000) | same path |
| `tanks.fuel.0.capacity` | m³ | same path |
| `tanks.fuel.0.name` | `Port` | same path |
| `tanks.fuel.1.*` | same fields, name `Starboard` | same paths |
| `propulsion.port.state` | `started` if thrust magnitude is over 1 N, otherwise `stopped` | same path |
| `propulsion.starboard.state` | same rule | same path |

The source object on every update is label `sailing-simulator@<COMPUTERNAME>` and type `simulator`.

Lume's preset buttons (in `plugins/signalk-lume-ti/public/index.html`) line up as follows:

- `navigation.speedOverGround` and `environment.wind.speedApparent` match. The numbers are m/s, not knots.
- `navigation.position.latitude` and `navigation.position.longitude` match after Lume splits the position object. The numbers are degrees.
- `environment.depth.belowKeel` matches, in meters.
- `environment.depth.belowTransducer` is in the Recent Depth preset. Skiff does not send that path, so the column stays empty for Skiff rows.
- Angles in the table are radians. A heading that looks like `1.57` is a right angle, not 1.57 degrees.
- Preset columns with `@max` or `@min` fill in when the bucket closes. A fresh second may only have the base column.

Tank ids are `0` (port) and `1` (starboard). Propulsion ids are the words `port` and `starboard`. The simulator comment describes two 275 L diesel tanks; `capacity` is that volume divided by 1000, in cubic meters.

## 5. Make Skiff the only source

List only. Do not do this from the docs lane. Do it on the Pi if the demo should show Skiff alone.

Skiff and the N2K sample replay both land on the self vessel. Until the replay stops, `max(ts)` without a `$source` filter cannot tell them apart.

1. Open the Signal K admin on halos.local.
2. Go to Server > Data Connections.
3. Disable the connection that replays N2K sample data onto the self vessel. Do not delete it.
4. Confirm with the `$source` query in section 3. New sample rows should stop. Skiff rows should continue.

The Lume repo does not name that connection. `navigation.speedThroughWater.values[n2kFromFile.43]` in `plan/design/signalk-formats.md` is a per-source format example, not a checked-in Pi connection id. Confirm the name in the admin before disabling anything.

## What this lane does not do

- No edit to the Skiff repo.
- No `wss://` client.
- No live Pi session and no live Skiff process.
- The helper is not executed here, and no token is stored in git.

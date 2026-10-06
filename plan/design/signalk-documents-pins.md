# Signal K documents and explicit chart pins

Spec/06 Sources §4 and spec/08 Pin to chart. No Signal K data-path writes.

## Polling

The ingest service starts one read-only HTTP worker. It fetches notes immediately and at a fixed 60-second cadence, with the same bearer token as telemetry or anonymous reads. Completed snapshots go through a bounded two-message channel; the ingest thread applies them alongside notification/rule document writes. Fetching never holds a telemetry store lock or writes DocStore. Startup/poll/apply errors are reported and do not escape the telemetry loop.

Notes use resource IDs unchanged, kind notes, title/name and description/text. Position is appended to body text. Start comes from a supplied range, datetime/timestamp or creation time; absent time uses a stable first-seen value. Range endpoints support timeRange/range, ts_start/ts_end, and pin metadata. Existing Document validation remains in force, including the 2020 epoch. Pre-epoch rejections are counted as attempts in ingest_status.json and exposed by ti_status; a snapshot containing them preserves all previous documents. Resources wait for the server's self-vessel hello before being applied, unless a real self URN was supplied. Entire snapshots validate before mutation; malformed timestamps cannot delete old documents.

A persisted ownership ledger tracks only poller-created IDs by vessel/kind. Before mutation it records the union of old/new IDs, so a crash cannot orphan new IDs from reconciliation. After DocStore's single-write reconciliation it narrows the set. Unowned imported notes survive deletion snapshots. Ledger errors preserve the prior document set and are reported.

Modern logbook detection uses GET resources/logentries with an explicit 2020-to-now window; 404 falls back to the legacy plugin logs/day routes. A missing plugin is optional. Only a complete successful listing is authoritative for deletion. Requests have 5-second timeouts, no redirects, an 8 MiB response/snapshot cap and 20,000 entries; legacy scanning has a 45-second cycle budget and cancellation. Over-budget snapshots preserve the last successful result. A historical logbook larger than those bounds needs a later paginated/windowed importer, rather than unsafe deletion from a partial list.

Sources checked: [Resources API](https://github.com/SignalK/signalk-server/blob/master/docs/develop/rest-api/resources_api.md), [resource record shape](https://github.com/SignalK/signalk-server/blob/master/packages/server-api/src/typebox/resources-schemas.ts), [modern logbook contract](https://github.com/meri-imperiumi/signalk-logbook/blob/main/docs/logentries-resource.md), [legacy OpenAPI](https://github.com/meri-imperiumi/signalk-logbook/blob/main/schema/openapi.yaml).

## Chart writes

The webapp loads a dependency-free pin module. Only Pin/Unpin click handlers invoke it; status polling and SQL execution never write resources. Browser requests use the same-origin user session, leaving the ingest token's privileges unchanged.

Intervals require start/end columns. Notes carry title, description, range/query metadata and group lume-ti inside the standard properties object. Region links use v2 href=/resources/regions/<uuid>; region names/descriptions stay at the resource top level. Group here means ownership metadata on each resource; this slice does not create a Freeboard display-selection /resources/groups record. Optional latitude/longitude columns or user-supplied coordinates anchor interval notes. In_bbox queries with one four-literal call also create linked GeoJSON regions; antimeridian boxes split into MultiPolygon. Unlocated interval notes stay in the Resources list. Bounding arguments follow TI's lat_min,lon_min,lat_max,lon_max order.

Resources use client-generated UUIDs and PUT to explicit note/region IDs. Validate the whole result before any write; reject truncated/over-500-row results. Partial failures report written count and do not auto-retry. Unpin lists both collections, verifies group and ownership metadata locally (providers may ignore filters), and deletes owned notes before regions. Browser confirmation precedes Unpin all lume-ti. No query is executed merely to pin, and no endpoint can change vessel/data paths.

Verification coverage: mock Resources create/update/delete/restart/failure tests in ti-ingest; root integration checks the actual lexical match(notes,...) backend and cache invalidation; Node tests exercise resource HTTP methods, bbox geometry, ownership, failure and explicit-click behavior. Host strict clippy/formatting applies only to TI crates/specific files.

#!/usr/bin/env python3
"""Read-only InfluxDB 2.x / Lume benchmark; Python 3 stdlib only.
See influx_vs_lume.md for sampling semantics and execution instructions.
"""
import argparse
import csv
import datetime as dt
import io
import http.client
import json
import math
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

UTC = dt.timezone.utc
DEPTH = "environment.depth.belowTransducer"
SOG = "navigation.speedOverGround"
POSITION = "navigation.position"


class BenchError(Exception):
    pass


def timestamp(value):
    text = str(value)
    if not re.search(r"(Z|[+-]\d\d:\d\d)$", text):
        raise BenchError("Timestamp requires an explicit timezone")
    try:
        return dt.datetime.fromisoformat(text.replace("Z", "+00:00")).astimezone(UTC)
    except ValueError as error:
        raise BenchError("Invalid timestamp") from error


def iso(value):
    return value.astimezone(UTC).isoformat(timespec="microseconds").replace("+00:00", "Z")


def nanoseconds(value):
    # datetime truncates sub-microsecond fractions; value checks must not.
    match = re.fullmatch(r"(.*T\d\d:\d\d:\d\d)(?:\.(\d{1,9}))?(Z|[+-]\d\d:\d\d)", str(value))
    if not match:
        raise BenchError("Invalid RFC3339 result timestamp")
    base = timestamp(match[1] + match[3])
    epoch = dt.datetime(1970, 1, 1, tzinfo=UTC)
    delta = base - epoch
    return (delta.days * 86400 + delta.seconds) * 1_000_000_000 + int((match[2] or "").ljust(9, "0"))


def result_time(value):
    seconds, fraction = divmod(nanoseconds(value), 1_000_000_000)
    base = dt.datetime.fromtimestamp(seconds, UTC).strftime("%Y-%m-%dT%H:%M:%S")
    return base + "." + format(fraction, "09d") + "Z"


def quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def ident(value):
    return '"' + str(value).replace('"', '""') + '"'


def flux_string(value):
    # Flux interpolates dollar-brace expressions inside strings.
    if any(ord(c) < 32 and c not in "\n\r\t" for c in str(value)):
        raise BenchError("Unsupported control character in a Flux string")
    return json.dumps(str(value), ensure_ascii=False).replace("${", "\\${")


def duration(value):
    match = re.fullmatch(r"(\d+(?:\.\d+)?)(s|m|h|d)", value)
    if not match:
        raise argparse.ArgumentTypeError("Window must be seconds/minutes/hours/days, e.g. 24h")
    seconds = float(match[1]) * {"s": 1, "m": 60, "h": 3600, "d": 86400}[match[2]]
    if not math.isfinite(seconds) or seconds <= 0:
        raise argparse.ArgumentTypeError("Window must be positive and finite")
    return seconds


def csv_rows(payload):
    """Decode annotated CSV including repeated headers, defaults and error tables."""
    header = types = defaults = None
    result = []
    for cells in csv.reader(io.StringIO(payload.lstrip("\ufeff"))):
        if not cells or not any(cells):
            header = None
            continue
        if cells[0].startswith("#"):
            if cells[0] == "#datatype":
                types = cells
            elif cells[0] == "#default":
                defaults = cells
            continue
        if header is None or cells[:3] == ["", "result", "table"]:
            if cells[:3] != ["", "result", "table"] and "error" not in cells:
                raise BenchError("Invalid Influx annotated CSV header")
            header = cells
            continue
        if len(cells) != len(header):
            raise BenchError("Malformed Influx CSV row")
        row = {}
        for i, name in enumerate(header):
            if not name:
                continue
            value = cells[i] or (defaults[i] if defaults and i < len(defaults) else "")
            kind = types[i] if types and i < len(types) else "string"
            if value == "":
                row[name] = None
            elif kind in ("long", "unsignedLong"):
                row[name] = int(value)
            elif kind == "double":
                row[name] = float(value)
            elif kind == "boolean":
                row[name] = value.lower() == "true"
            else:
                row[name] = value
        if "error" in row:
            raise BenchError("Influx query failed: " + str(row["error"]))
        result.append(row)
    return result


def endpoint(url, suffix):
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme not in ("http", "https") or not parsed.hostname or parsed.username or parsed.password:
        raise BenchError("Use an HTTP(S) URL without embedded credentials")
    if parsed.query or parsed.fragment:
        raise BenchError("Endpoint URL must not contain query parameters or a fragment")
    path = parsed.path.rstrip("/")
    if path.endswith("/ti/query"):
        path = path[:-len("/ti/query")]
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, path + suffix, "", ""))


class Client:
    def __init__(self, env, timeout=60):
        self.env = env
        self.timeout = timeout

    def redact(self, text):
        token = self.env.get("INFLUX_TOKEN", "")
        return str(text).replace(token, "[REDACTED]") if token else str(text)

    def http(self, url, body=None, headers=None):
        payload = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(url, data=payload, headers=headers or {},
                                         method="GET" if payload is None else "POST")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                data = response.read(128 * 1024 * 1024 + 1)
                if len(data) > 128 * 1024 * 1024:
                    raise BenchError("Response exceeds 128 MiB")
                return data.decode("utf-8")
        except urllib.error.HTTPError as error:
            detail = error.read(8192).decode("utf-8", errors="replace")
            raise BenchError(self.redact("HTTP " + str(error.code) + ": " + detail)) from None
        except (urllib.error.URLError, OSError, http.client.HTTPException) as error:
            raise BenchError(self.redact("HTTP transport failed: " + str(error))) from None

    def influx(self, flux):
        url = endpoint(self.env["INFLUX_URL"], "/api/v2/query") + "?" + urllib.parse.urlencode({"org": self.env["INFLUX_ORG"]})
        raw = self.http(url, {"query": flux, "type": "flux",
                             "dialect": {"annotations": ["datatype", "group", "default"], "header": True}},
                        {"Authorization": "Token " + self.env["INFLUX_TOKEN"],
                         "Content-Type": "application/json", "Accept": "text/csv"})
        return csv_rows(raw)

    def lume(self, sql):
        result = json.loads(self.http(endpoint(self.env["LUME_URL"], "/ti/query"),
                                     {"sql": sql, "max_rows": 500},
                                     {"Content-Type": "application/json", "Accept": "application/json"}))
        if not isinstance(result.get("rows"), list) or not isinstance(result.get("truncated"), bool):
            raise BenchError("Invalid Lume query envelope")
        return result

    def schema(self):
        return json.loads(self.http(endpoint(self.env["LUME_URL"], "/ti/schema"),
                                    headers={"Accept": "application/json"}))


def fields(catalog, table):
    selected = next((t for t in catalog["tables"] if t["name"] == table), None)
    if selected is None:
        raise BenchError("Requested Lume table is unavailable")
    names = {c["name"] for c in selected["columns"]}
    mapping = {}
    for aggregate in ("mean", "last", "min", "max"):
        for name in sorted(names):
            if name.endswith("@" + aggregate) and "$source" not in name:
                mapping.setdefault(name.rsplit("@", 1)[0], name)
    return names, mapping


def flux_base(bucket, context, start, stop):
    return ("from(bucket: " + flux_string(bucket) + ")\n"
            "  |> range(start: time(v: " + flux_string(iso(start)) + "), stop: time(v: " + flux_string(iso(stop)) + "))\n"
            "  |> filter(fn: (r) => r.context == " + flux_string(context) + ")")


def snap_window(start, stop, width):
    begin = nanoseconds(iso(start) if isinstance(start, dt.datetime) else start)
    end = nanoseconds(iso(stop) if isinstance(stop, dt.datetime) else stop)
    if begin >= end:
        raise BenchError("Window start must precede end")
    step = width * 1_000_000_000
    lo = begin // step * step
    hi = -(-end // step) * step
    return (dt.datetime.fromtimestamp(lo / 1e9, UTC),
            dt.datetime.fromtimestamp(hi / 1e9, UTC))


def choose_window(args, first, last):
    stop = timestamp(args.stop) if args.stop else last
    start = timestamp(args.start) if args.start else max(first, stop - dt.timedelta(seconds=args.window))
    mode = "common_data_coverage"
    if not args.start and not args.stop and args.window >= 3600:
        end_hour = dt.datetime.fromtimestamp(math.floor(last.timestamp() / 3600) * 3600, UTC)
        first_hour = dt.datetime.fromtimestamp(math.ceil(first.timestamp() / 3600) * 3600, UTC)
        full_start = max(first_hour, end_hour - dt.timedelta(hours=math.floor(args.window / 3600)))
        if full_start < end_hour:
            start, stop, mode = full_start, end_hour, "latest_full_common_hours"
        else:
            mode = "common_data_coverage_partial_hour"
    return start, stop, mode


def plans(args, start, stop, names, mapping, bucket, context, influx_context):
    where = ("vessel = " + quote(context) + " AND ts >= TIMESTAMP " + quote(iso(start))
             + " AND ts < TIMESTAMP " + quote(iso(stop)))
    table = ident(args.table)
    base = flux_base(bucket, influx_context, start, stop)
    def source(path):
        return base + "\n  |> filter(fn: (r) => r._measurement == " + flux_string(path) + ' and r._field == "value")\n  |> group(columns: [])'
    def retained(path, aggregate):
        column = path + "@" + aggregate
        return ident(column) if column in names else None
    raw = ident(args.depth_path + "@last" if args.depth_path + "@last" in names else mapping.get(args.depth_path, args.depth_path + "@mean"))
    mean = retained(args.sog_path, "mean")
    minimum = retained(args.depth_path, "min")
    maximum = retained(args.depth_path, "max")
    lat = mapping.get(POSITION + ".latitude")
    lon = mapping.get(POSITION + ".longitude")
    # Positions use paired last buckets where retained, not independent means.
    lat = POSITION + ".latitude@last" if POSITION + ".latitude@last" in names else lat
    lon = POSITION + ".longitude@last" if POSITION + ".longitude@last" in names else lon
    hour = "date_bin(INTERVAL '1 hour', ts, TIMESTAMP '1970-01-01T00:00:00Z')"
    minute = "date_bin(INTERVAL '1 minute', ts, TIMESTAMP '1970-01-01T00:00:00Z')"
    q = []
    def add(number, title, sql, flux, columns, bin_seconds=None, missing=None):
        q.append({"id": number, "name": title, "sql": sql, "flux": flux,
                  "columns": columns, "bin_seconds": bin_seconds, "missing": missing})
    add(1, "raw depth range",
        "SELECT ts AS time, " + raw + " AS value FROM " + table + " WHERE " + where + " AND " + raw + " IS NOT NULL ORDER BY ts",
        source(args.depth_path) + '\n  |> sort(columns: ["_time"])\n  |> keep(columns: ["_time", "_value"])',
        ["time", "value"], args.width, None if args.depth_path in mapping else "No retained depth column")
    add(2, "hourly maximum depth",
        "SELECT " + hour + " AS time, max(" + (maximum or '"MISSING_MAX"') + ") AS value FROM " + table + " WHERE " + where + " AND " + (maximum or '"MISSING_MAX"') + " IS NOT NULL GROUP BY 1 ORDER BY 1",
        source(args.depth_path) + '\n  |> aggregateWindow(every: 1h, fn: max, createEmpty: false, timeSrc: "_start")\n  |> map(fn: (r) => ({r with _time: date.truncate(t: r._time, unit: 1h)}))\n  |> keep(columns: ["_time", "_value"])',
        ["time", "value"], 3600, None if maximum else "No retained depth@max")
    add(3, "minute mean SOG",
        "SELECT " + minute + " AS time, avg(" + (mean or '"MISSING_MEAN"') + ") AS value FROM " + table + " WHERE " + where + " AND " + (mean or '"MISSING_MEAN"') + " IS NOT NULL GROUP BY 1 ORDER BY 1",
        source(args.sog_path) + '\n  |> aggregateWindow(every: 1m, fn: mean, createEmpty: false, timeSrc: "_start")\n  |> map(fn: (r) => ({r with _time: date.truncate(t: r._time, unit: 1m)}))\n  |> keep(columns: ["_time", "_value"])',
        ["time", "value"], 60, None if mean else "No retained SOG@mean")
    multi = ('s = ' + source(args.sog_path) + '\n  |> aggregateWindow(every: 1m, fn: mean, createEmpty: false, timeSrc: "_start")\n  |> map(fn: (r) => ({r with _time: date.truncate(t: r._time, unit: 1m)}))\n'
             'd = ' + source(args.depth_path) + '\n  |> aggregateWindow(every: 1m, fn: min, createEmpty: false, timeSrc: "_start")\n  |> map(fn: (r) => ({r with _time: date.truncate(t: r._time, unit: 1m)}))\n'
             'join(tables: {s: s, d: d}, on: ["_time"])\n'
             '  |> filter(fn: (r) => r._value_s > ' + str(args.sog_gt) + ' and r._value_d < ' + str(args.depth_lt) + ')\n'
             '  |> map(fn: (r) => ({_time: r._time, speed: r._value_s, depth: r._value_d}))')
    add(4, "minutes: mean SOG > X, minimum depth < Y",
        "SELECT " + minute + " AS time, avg(" + (mean or '"MISSING_MEAN"') + ") AS speed, min(" + (minimum or '"MISSING_MIN"') + ") AS depth FROM " + table + " WHERE " + where + " GROUP BY 1 HAVING avg(" + (mean or '"MISSING_MEAN"') + ") > " + str(args.sog_gt) + " AND min(" + (minimum or '"MISSING_MIN"') + ") < " + str(args.depth_lt) + " ORDER BY 1",
        multi, ["time", "speed", "depth"], 60, None if mean and minimum else "Missing SOG@mean or depth@min")
    position = (base + '\n  |> filter(fn: (r) => r._measurement == "navigation.position" and (r._field == "lat" or r._field == "lon"))\n'
                '  |> keep(columns: ["_time", "_field", "_value"])')
    bucket_fn = "bucketTime = (t) => time(v: int(v: t) / " + str(args.width * 1000000000) + " * " + str(args.width * 1000000000) + ")\n"
    deepest = ('import "math"\n' + bucket_fn +
               'd = ' + source(args.depth_path) + '\n  |> sort(columns: ["_value", "_time"])\n  |> limit(n: 1)\n'
               '  |> map(fn: (r) => ({bucket: bucketTime(t: r._time), _time: r._time, depth: float(v: r._value)}))\n'
               'p = ' + position + '\n  |> group(columns: [])\n'
               '  |> pivot(rowKey: ["_time"], columnKey: ["_field"], valueColumn: "_value")\n'
               '  |> filter(fn: (r) => exists r.lat and exists r.lon)\n'
               '  |> map(fn: (r) => ({bucket: bucketTime(t: r._time), position_time: r._time, lat: r.lat, lon: r.lon}))\n'
               'd |> map(fn: (r) => ({_time: r._time, depth: r.depth, role: "depth"})) |> yield(name: "depth")\n'
               'join(tables: {d: d, p: p}, on: ["bucket"])\n'
               '  |> map(fn: (r) => ({r with distance: math.abs(x: float(v: int(v: r.position_time) - int(v: r._time)))}))\n'
               '  |> sort(columns: ["distance", "position_time"]) |> limit(n: 1)\n'
               '  |> map(fn: (r) => ({_time: r._time, lat: r.lat, lon: r.lon, role: "position"})) |> yield(name: "position")')
    add(5, "minimum depth and position at that time",
        "SELECT ts AS time, " + (minimum or '"MISSING_MIN"') + " AS depth, " + (ident(lat) if lat else "NULL") + " AS latitude, " + (ident(lon) if lon else "NULL") + " AS longitude FROM " + table + " WHERE " + where + " AND " + (minimum or '"MISSING_MIN"') + " IS NOT NULL ORDER BY depth, ts LIMIT 1",
        deepest, ["time", "depth", "latitude", "longitude"], missing=None if minimum and lat and lon else "Missing depth@min or position coordinates")
    counts = {p: c for p, c in mapping.items() if not p.startswith(POSITION + ".")}
    if lat:
        counts[POSITION] = lat
    count_sql = ["SELECT " + quote(path) + " AS path, count(" + ident(column) + ") AS count FROM " + table + " WHERE " + where + " HAVING count(" + ident(column) + ") > 0" for path, column in sorted(counts.items())]
    count_flux = (bucket_fn + 'data = ' + base +
                  '\n  |> filter(fn: (r) => r._field == "value" or (r._measurement == "navigation.position" and r._field == "lat"))\n'
                  '  |> group(columns: ["_measurement"])\n'
                  'raw = data |> count()\n'
                  'buckets = data |> map(fn: (r) => ({r with _time: bucketTime(t: r._time)}))\n'
                  '  |> unique(column: "_time") |> count()\n'
                  'join(tables: {r: raw, b: buckets}, on: ["_measurement"])\n'
                  '  |> map(fn: (r) => ({path: r._measurement, count: r._value_r, bucket_count: r._value_b}))')
    add(6, "point counts per retained path", " UNION ALL ".join(count_sql),
        count_flux, ["path", "count"], missing=None if counts else "No retained paths")
    q[0]["raw_aggregate"] = raw.strip('"').rsplit("@", 1)[-1]
    q[-1]["count_statements"] = count_sql
    for item in q:
        if "date.truncate" in item["flux"]:
            item["flux"] = 'import "date"\n' + item["flux"]
    return q


def canonical(rows, columns, influx=False):
    aliases = {"time": "_time", "value": "_value", "latitude": "lat", "longitude": "lon"}
    output = []
    if influx and "depth" in columns and any(r.get("role") == "depth" for r in rows):
        depth_rows = [r for r in rows if r.get("role") == "depth"]
        positions = {r["_time"]: r for r in rows if r.get("role") == "position"}
        rows = [{**r, "lat": positions.get(r["_time"], {}).get("lat"), "lon": positions.get(r["_time"], {}).get("lon")} for r in depth_rows]
    for row in rows:
        for column in columns:
            name = aliases.get(column, column) if influx else column
            if name not in row and not (influx and column in ("latitude", "longitude")):
                raise BenchError("Backend result is missing " + column)
        normalized = {c: row.get(aliases.get(c, c) if influx else c) for c in columns}
        if "time" in normalized and normalized["time"] is None:
            raise BenchError("Backend result is missing a timestamp")
        if normalized.get("time") is not None:
            normalized["time"] = result_time(normalized["time"])
        if influx and "count" in columns and "bucket_count" in row:
            normalized["bucket_count"] = row["bucket_count"]
        output.append(normalized)
    return sorted(output, key=lambda r: json.dumps({k: r[k] for k in ("time", "path") if k in r}, sort_keys=True))


def compare(left, right, absolute, relative, time_tolerance=0):
    differences = []
    if len(left) != len(right):
        differences.append("row counts differ: Influx=" + str(len(left)) + ", Lume=" + str(len(right)))
    for i, (a, b) in enumerate(zip(left, right)):
        for key in a:
            x, y = a[key], b.get(key)
            if key == "time" and x is not None and y is not None:
                equal = abs(nanoseconds(x) - nanoseconds(y)) <= time_tolerance * 1_000_000_000
            elif key == "count":
                equal = x == y  # Counts are exact, regardless of numeric tolerance.
            elif isinstance(x, (float, int)) and not isinstance(x, bool) and isinstance(y, (float, int)) and not isinstance(y, bool):
                equal = math.isfinite(x) and math.isfinite(y) and math.isclose(x, y, abs_tol=absolute, rel_tol=relative)
            else:
                equal = x == y
            if not equal and len(differences) < 20:
                differences.append("row " + str(i) + " " + key + ": Influx=" + repr(x) + ", Lume=" + repr(y))
    return {"status": "MISMATCH" if differences else ("PASS" if left else "EMPTY"),
            "differences": differences}


def assess(item, left, right, args):
    """FIDELITY requires an explicit, query-specific invariant; no blanket pardon."""
    observations = {"row_count_difference": abs(len(left) - len(right))}
    def metric(key, value):
        observations[key] = max(observations.get(key, 0), value)
    for a, b in ([] if item["id"] in (1, 6) else zip(left, right)):
        for key in a:
            x, y = a[key], b.get(key)
            if key == "time" and y is not None:
                metric("time_seconds", abs(nanoseconds(x) - nanoseconds(y)) / 1e9)
            elif isinstance(x, (float, int)) and isinstance(y, (float, int)):
                if math.isfinite(x) and math.isfinite(y):
                    metric(key + "_absolute", abs(x - y))
                    metric(key + "_relative", abs(x - y) / max(abs(x), abs(y), 1e-300))
    strict = compare(left, right, args.abs_tol, args.rel_tol, 0 if item["id"] == 5 else args.time_tol)
    fidelity = []
    differences = []
    qid = item["id"]
    if qid == 1 and left:
        grouped = {}
        step = args.width * 1_000_000_000
        for row in left:
            bucket = nanoseconds(row["time"]) // step * step
            grouped.setdefault(bucket, []).append(row)
        collapsed = []
        agg = item.get("raw_aggregate", "last")
        for bucket, rows in sorted(grouped.items()):
            values = [r["value"] for r in rows]
            if not all(isinstance(v, (float, int)) and math.isfinite(v) for v in values):
                return {**strict, "max_observed_difference": observations}
            value = {"last": lambda: values[-1], "mean": lambda: sum(values) / len(values),
                     "min": lambda: min(values), "max": lambda: max(values)}[agg]()
            collapsed.append({"time": result_time(iso(dt.datetime.fromtimestamp(bucket / 1e9, UTC))), "value": value})
            metric("max_samples_per_bucket", len(rows))
            for row in rows:
                metric("sample_to_bucket_seconds", (nanoseconds(row["time"]) - bucket) / 1e9)
                metric("sample_to_bucket_value_absolute", abs(row["value"] - value))
        for a, b in zip(collapsed, right):
            if isinstance(b.get("value"), (float, int)) and math.isfinite(b["value"]):
                metric("value_absolute", abs(a["value"] - b["value"]))
                metric("value_relative", abs(a["value"] - b["value"]) / max(abs(a["value"]), abs(b["value"]), 1e-300))
        metric("bucket_row_count_difference", abs(len(collapsed) - len(right)))
        checked = compare(collapsed, right, args.abs_tol, args.rel_tol, 0)
        checked["strict_bucket_check"] = {"status": checked["status"], "raw_buckets": len(collapsed), "lume_buckets": len(right)}
        if checked["status"] == "PASS":
            if strict["status"] != "PASS":
                fidelity.append("Raw samples collapse to exactly the same retained buckets/values; each sample offset is < bucket width")
            strict = checked
        else:
            strict = checked
    elif qid == 6:
        a = {r["path"]: r for r in left}
        b = {r["path"]: r for r in right}
        paths = {"influx_only": sorted(a.keys() - b.keys()), "lume_only": sorted(b.keys() - a.keys()),
                 "symmetric_difference": sorted(a.keys() ^ b.keys())}
        if paths["symmetric_difference"]:
            differences.append("Path sets differ: " + repr(paths))
        bucket_checks = {}
        for key in sorted(a.keys() & b.keys()):
            raw, retained = a[key]["count"], b[key]["count"]
            bucket_count = a[key].get("bucket_count")
            bucket_checks[key] = {"influx": bucket_count, "lume": retained,
                                  "status": "PASS" if bucket_count == retained else "MISMATCH"}
            metric("count_absolute", abs(raw - retained))
            if bucket_count is not None:
                metric("bucket_count_absolute", abs(bucket_count - retained))
            if raw == retained and bucket_count == retained:
                continue
            if bucket_count == retained and raw >= bucket_count > 0:
                fidelity.append(key + ": raw samples >= exactly matched distinct bucket count")
            else:
                differences.append(key + ": raw=" + str(raw) + ", buckets=" + str(bucket_count) + ", Lume=" + str(retained))
        strict = {"status": "MISMATCH" if differences else ("PASS" if left or right else "EMPTY"),
                  "differences": differences[:20], "path_sets": paths, "strict_bucket_checks": bucket_checks}
    elif qid in (3, 4, 5) and len(left) == len(right):
        allowed = []
        for a, b in zip(left, right):
            row = dict(a)
            mean_key = "value" if qid == 3 else "speed" if qid == 4 else None
            if mean_key:
                x, y = a[mean_key], b[mean_key]
                if isinstance(x, (float, int)) and isinstance(y, (float, int)) and math.isfinite(x) and math.isfinite(y):
                    if not math.isclose(x, y, abs_tol=args.abs_tol, rel_tol=args.rel_tol) and math.isclose(x, y, abs_tol=0, rel_tol=args.fidelity_rel):
                        row[mean_key] = y
                        allowed.append("Mean differs within fidelity-rel=" + str(args.fidelity_rel))
            if qid == 5:
                delta = nanoseconds(a["time"]) - nanoseconds(b["time"])
                step = args.width * 1_000_000_000
                if delta != 0 and 0 <= delta < step and nanoseconds(a["time"]) // step * step == nanoseconds(b["time"]):
                    row["time"] = b["time"]
                    allowed.append("Minimum sample is inside the same retained bucket, offset < bucket width")
            checked = compare([row], [b], args.abs_tol, args.rel_tol, args.time_tol if qid != 5 else 0)
            differences.extend(checked["differences"])
        if not differences and allowed:
            strict = {"status": "PASS", "differences": []}
            fidelity.extend(allowed)
    if strict["status"] == "PASS" and fidelity:
        strict["status"] = "FIDELITY"
    return {**strict, "fidelity": sorted(set(fidelity)), "max_observed_difference": observations}


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    index = (len(ordered) - 1) * fraction
    low = int(index)
    high = min(low + 1, len(ordered) - 1)
    return ordered[low] + (ordered[high] - ordered[low]) * (index - low)


def execute_lume(client, item, args, start, stop, names, mapping, bucket, context, influx_context):
    requests = 0
    def query(sql):
        nonlocal requests
        requests += 1
        return client.lume(sql)
    if item["missing"]:
        raise BenchError(item["missing"])
    if item["id"] == 6:
        rows = []
        for i in range(0, len(item["count_statements"]), 100):
            reply = query(" UNION ALL ".join(item["count_statements"][i:i + 100]))
            if reply["truncated"]:
                raise BenchError("Lume path counts truncated; no partial answer accepted")
            rows.extend(reply["rows"])
        return rows, requests
    step = item["bin_seconds"]
    if not step:
        reply = query(item["sql"])
        if reply["truncated"]:
            raise BenchError("Lume result truncated; no partial answer accepted")
        return reply["rows"], requests
    def chunk(lo, hi):
        current = plans(args, lo, hi, names, mapping, bucket, context, influx_context)[item["id"] - 1]
        reply = query(current["sql"])
        if not reply["truncated"]:
            return reply["rows"]
        # Only split at complete output-bin boundaries, never across an aggregate.
        first_boundary = (math.floor(lo.timestamp() / step) + 1) * step
        last_boundary = math.ceil(hi.timestamp() / step) * step - step
        if first_boundary > last_boundary:
            raise BenchError("One Lume output bin is truncated; no partial answer accepted")
        middle = (math.floor((first_boundary + last_boundary) / (2 * step))) * step
        mid = dt.datetime.fromtimestamp(middle, UTC)
        return chunk(lo, mid) + chunk(mid, hi)
    return chunk(start, stop), requests


def discover(client, args, names, mapping, bucket, context, influx_context):
    condition = " OR ".join(ident(mapping[p]) + " IS NOT NULL" for p in (args.depth_path, args.sog_path) if p in mapping)
    if not condition:
        raise BenchError("No retained depth/SOG data")
    reply = client.lume("SELECT min(ts) AS first, max(ts) AS last FROM " + ident(args.table)
                        + " WHERE vessel = " + quote(context) + " AND (" + condition + ")")
    if reply["truncated"] or not reply["rows"] or not reply["rows"][0].get("first"):
        raise BenchError("No Lume depth/SOG coverage")
    flux = ('data = from(bucket: ' + flux_string(bucket) + ') |> range(start: 0)\n'
            '  |> filter(fn: (r) => r.context == ' + flux_string(influx_context) + ' and r._field == "value" and (r._measurement == ' + flux_string(args.depth_path) + ' or r._measurement == ' + flux_string(args.sog_path) + '))\n'
            '  |> group(columns: [])\n  |> sort(columns: ["_time"])\n'
            'data |> first() |> map(fn: (r) => ({edge: "first", at: r._time})) |> yield(name: "first")\n'
            'data |> last() |> map(fn: (r) => ({edge: "last", at: r._time})) |> yield(name: "last")')
    edges = {r["edge"]: timestamp(r["at"]) for r in client.influx(flux)}
    if "first" not in edges or "last" not in edges:
        raise BenchError("No Influx depth/SOG coverage for the selected context")
    row = reply["rows"][0]
    first = max(timestamp(row["first"]), edges["first"])
    last = min(timestamp(row["last"]), edges["last"])
    return first, last


def benchmark(client, args, start, stop, names, mapping, bucket, context, influx_context):
    results = []
    for item in plans(args, start, stop, names, mapping, bucket, context, influx_context):
        runs = {"influx": [], "lume": []}
        checks = []
        for iteration in range(args.runs):
            answers = {}
            # Alternate order to reduce a systematic first-engine scheduling bias.
            for engine in (("influx", "lume") if iteration % 2 == 0 else ("lume", "influx")):
                begin = time.perf_counter()
                try:
                    if engine == "influx":
                        rows, requests = client.influx(item["flux"]), 1
                    else:
                        rows, requests = execute_lume(client, item, args, start, stop, names, mapping, bucket, context, influx_context)
                    rows = canonical(rows, item["columns"], engine == "influx")
                    answers[engine] = rows
                    run = {"ms": (time.perf_counter() - begin) * 1000, "rows": len(rows), "http_requests": requests}
                except (BenchError, ValueError, KeyError, TypeError) as error:
                    run = {"ms": (time.perf_counter() - begin) * 1000, "rows": None, "error": client.redact(error)}
                runs[engine].append(run)
            checks.append(assess(item, answers["influx"], answers["lume"], args)
                          if len(answers) == 2 else {"status": "ERROR", "differences": ["Backend error; value check not possible"]})
        summaries = {}
        for engine, samples in runs.items():
            warm = [r["ms"] for r in samples[1:] if "error" not in r]
            summaries[engine] = {"cold_first_ms": samples[0]["ms"] if "error" not in samples[0] else None,
                                 "warm_p50_ms": percentile(warm, .5), "warm_p95_ms": percentile(warm, .95),
                                 "row_counts": [r["rows"] for r in samples], "runs": samples}
        status = next((s for s in ("ERROR", "MISMATCH", "EMPTY", "FIDELITY") if any(c["status"] == s for c in checks)), "PASS")
        maxima = {}
        for check in checks:
            for key, value in check.get("max_observed_difference", {}).items():
                maxima[key] = max(maxima.get(key, 0), value)
        path_sets = [c["path_sets"] for c in checks if "path_sets" in c]
        path_summary = {key: sorted({p for paths in path_sets for p in paths[key]})
                        for key in ("influx_only", "lume_only", "symmetric_difference")}
        results.append({"max_observed_difference": maxima, "path_sets": path_summary,
                        "id": item["id"], "name": item["name"], "sql": item["sql"], "flux": item["flux"],
                        "status": status, "checks": checks, **summaries})
    return results


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--window", type=duration, default=duration("24h"))
    parser.add_argument("--from", dest="start")
    parser.add_argument("--to", dest="stop")
    parser.add_argument("--context", help="Lume vessel URN; inferred only for a single-vessel schema")
    parser.add_argument("--influx-context", help="Override the corresponding Influx context tag")
    parser.add_argument("--table", choices=["telemetry", "telemetry_hr"], default="telemetry")
    parser.add_argument("--depth-path", default=DEPTH)
    parser.add_argument("--sog-path", default=SOG)
    parser.add_argument("--sog-gt", type=float, default=2.0)
    parser.add_argument("--depth-lt", type=float, default=5.0)
    parser.add_argument("--runs", type=int, default=20, help="Total runs: first cold-first, remaining warm")
    parser.add_argument("--abs-tol", type=float, default=.001)
    parser.add_argument("--rel-tol", type=float, default=1e-6)
    parser.add_argument("--time-tol", type=float, default=0, help="Timestamp tolerance in seconds")
    parser.add_argument("--fidelity-rel", type=float, default=.01, help="Maximum relative mean deviation eligible for FIDELITY (default 0.01)")
    parser.add_argument("--timeout", type=float, default=60)
    parser.add_argument("--dry-run", action="store_true", help="Offline query texts; explicit bounds recommended")
    args = parser.parse_args(argv)
    if args.runs < 2 or not all(math.isfinite(v) for v in (args.sog_gt, args.depth_lt, args.abs_tol, args.rel_tol, args.time_tol, args.fidelity_rel, args.timeout)):
        parser.error("Runs must be >= 2; numeric options must be finite")
    if min(args.abs_tol, args.rel_tol, args.time_tol, args.fidelity_rel) < 0 or args.timeout <= 0:
        parser.error("Tolerances must be >= 0 and timeout > 0")
    return args


def main(argv=None, env=None):
    args = parse_args(argv)
    env = dict(os.environ if env is None else env)
    client = Client(env, args.timeout)
    try:
        bucket = env.get("INFLUX_BUCKET", "BUCKET")
        if args.dry_run:
            context = args.context or "vessels.URN"
            names = {p + "@" + a for p in (args.depth_path, args.sog_path) for a in ("mean", "min", "max", "last")}
            names.update({POSITION + ".latitude@last", POSITION + ".longitude@last"})
            mapping = {args.depth_path: args.depth_path + "@last", args.sog_path: args.sog_path + "@mean",
                       POSITION + ".latitude": POSITION + ".latitude@last", POSITION + ".longitude": POSITION + ".longitude@last"}
            args.width = 1 if args.table == "telemetry_hr" else 10
            stop = timestamp(args.stop) if args.stop else dt.datetime.now(UTC)
            start = timestamp(args.start) if args.start else stop - dt.timedelta(seconds=args.window)
            start, stop = snap_window(args.start or start, args.stop or stop, args.width)
            print(client.redact("# Dry run: assumed schema; bounds use now unless supplied. Live mode discovers common data coverage."))
            for item in plans(args, start, stop, names, mapping, bucket, context, args.influx_context or context):
                print(client.redact("\n## " + str(item["id"]) + " " + item["name"] + "\n\n```flux\n" + item["flux"] + "\n```\n\n```sql\n" + item["sql"] + "\n```"))
            return 0
        required = ("INFLUX_URL", "INFLUX_ORG", "INFLUX_BUCKET", "INFLUX_TOKEN", "LUME_URL")
        missing = [k for k in required if not env.get(k)]
        if missing:
            raise BenchError("Missing environment variables: " + ", ".join(missing))
        catalog = client.schema()
        names, mapping = fields(catalog, args.table)
        vessels = sorted({r["vessel"] for r in catalog.get("time_coverage", [])})
        context = args.context or (vessels[0] if len(vessels) == 1 else None)
        if context is None or context not in vessels:
            raise BenchError("Select a known vessel with --context (required for multi-vessel stores)")
        influx_context = args.influx_context or context
        args.width = 1 if args.table == "telemetry_hr" else int(catalog["width_seconds"])
        if args.width <= 0:
            raise BenchError("Invalid store width")
        if args.start and args.stop:
            start, stop = timestamp(args.start), timestamp(args.stop)
            mode = "explicit"
        else:
            first, last = discover(client, args, names, mapping, bucket, context, influx_context)
            start, stop, mode = choose_window(args, first, last)
        requested = {"from": args.start or iso(start), "to": args.stop or iso(stop)}
        start, stop = snap_window(args.start or start, args.stop or stop, args.width)
        report = {"window": {"from": iso(start), "to": iso(stop), "mode": mode, "requested": requested, "snap": "outward to bucket width"},
                  "context": context, "influx_context": influx_context, "table": args.table,
                  "bucket_width_seconds": args.width, "runs": args.runs,
                  "tolerance": {"absolute": args.abs_tol, "relative": args.rel_tol, "time_seconds": args.time_tol, "fidelity_relative": args.fidelity_rel},
                  "semantics": {"raw_depth_column": args.depth_path + "@last" if args.depth_path + "@last" in names else mapping.get(args.depth_path),
                                "sources": "Influx merges context/source series; Lume retains preferred-source bucket values",
                                "counts": "native raw points plus strict distinct-bucket counts versus retained populated buckets; all observed paths reported",
                                "position": "Influx minimum sample time with nearest paired lat/lon inside its bucket; Lume retained minimum bucket and bucket position",
                                "mean": "Influx raw sample mean versus Lume mean of populated bucket means",
                                "timing": "first request is cold-first, not cache-evicted; timed response decode + Lume pagination; discovery excluded"},
                  "queries": benchmark(client, args, start, stop, names, mapping, bucket, context, influx_context)}
        print(client.redact("| Query | Influx cold / p50 / p95 ms | Lume cold / p50 / p95 ms | Rows I / L (first) | Check |\n|---|---:|---:|---:|---|"))
        def timings(summary):
            return " / ".join("—" if summary[k] is None else format(summary[k], ".2f") for k in ("cold_first_ms", "warm_p50_ms", "warm_p95_ms"))
        for item in report["queries"]:
            print(client.redact("| " + str(item["id"]) + " " + item["name"] + " | " + timings(item["influx"]) + " | " + timings(item["lume"]) + " | " + str(item["influx"]["row_counts"][0]) + " / " + str(item["lume"]["row_counts"][0]) + " | " + item["status"] + " |"))
        print(client.redact("\nCold means first timed request, not evicted server/OS caches. JSON includes every run, errors and mismatch details.\n\n```json\n" + json.dumps(report, indent=2, allow_nan=False) + "\n```"))
        return 0 if all(q["status"] in ("PASS", "FIDELITY") for q in report["queries"]) else 1
    except (BenchError, ValueError, KeyError, TypeError, OverflowError) as error:
        print(client.redact("Benchmark failed: " + str(error)), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())

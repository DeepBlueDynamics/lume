"""Build opt-in v4, compare resident MCP bytes, and measure fresh-process opens."""
import argparse
import csv
import hashlib
import http.client
import json
import os
from pathlib import Path
import resource
import socket
import statistics
import subprocess
import time


def timings(stderr):
    return [json.loads(line[len("LUME_TIMING "):]) for line in stderr.splitlines()
            if line.startswith("LUME_TIMING ")]


def mcp(port, db, query):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=120)
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": "lume_search", "arguments": {
                          "db": str(db), "query": query, "limit": 100, "alpha": 0, "graph": 0}}})
    try:
        connection.request("POST", "/mcp", body, {"Content-Type": "application/json"})
        response = connection.getresponse()
        raw = response.read()
        if response.status != 200:
            raise RuntimeError("MCP HTTP status " + str(response.status))
        reply = json.loads(raw)
        if "error" in reply or reply.get("result", {}).get("isError"):
            raise RuntimeError("MCP query error: " + str(reply))
        return raw
    finally:
        connection.close()


def resident(binary, db, queries, env, log, passes):
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    with log.open("wb") as out:
        child = subprocess.Popen([str(binary), "serve", "--bind", "127.0.0.1",
                                  "--port", str(port)], env=env, stdout=out, stderr=out)
        try:
            deadline = time.monotonic() + 30
            while True:
                if child.poll() is not None:
                    raise RuntimeError("Server exited during startup")
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.5):
                        break
                except OSError:
                    if time.monotonic() > deadline:
                        raise RuntimeError("Server startup timed out")
                    time.sleep(.05)
            replies, samples = {}, []
            for iteration in range(passes + 1):
                for qid, query in queries:
                    started = time.perf_counter()
                    raw = mcp(port, db, query)
                    elapsed = (time.perf_counter() - started) * 1000
                    if qid in replies and replies[qid] != raw:
                        raise RuntimeError("Nondeterministic reply: " + qid)
                    replies[qid] = raw
                    if iteration:
                        samples.append({"qid": qid, "pass": iteration, "ms": elapsed})
            return replies, samples
        finally:
            child.terminate()
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, required=True)
    p.add_argument("--source", type=Path, default=Path("/indexes/trec-covid-files"))
    p.add_argument("--existing", type=Path, default=Path("/indexes/hot-path/trec-covid/stemmed"))
    p.add_argument("--new-db", type=Path, default=Path("/measuredb/trec-v4"))
    p.add_argument("--queries", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--query-db", type=Path, default=Path("/measuredb/query-index"))
    p.add_argument("--passes", type=int, default=3)
    a = p.parse_args()
    if a.query_db.exists() or a.query_db.is_symlink():
        raise ValueError("Refusing to overwrite the common query-path alias")
    if a.new_db.exists() or a.passes < 1:
        raise ValueError("Require a fresh output index and positive passes")
    a.output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, LUME_STEM="1", LUME_QUERY_INVERSION="0")
    for name in ("LUME_INDEX_FORMAT", "LUME_EMBED_MODEL", "LUME_EMBED_DIMENSIONS"):
        env.pop(name, None)
    started = time.perf_counter()
    with (a.output / "index.stdout.log").open("wb") as out, (a.output / "index.stderr.log").open("wb") as err:
        subprocess.run([str(a.binary), "index", "-f", str(a.source), "--db", str(a.new_db)],
                       env=dict(env, LUME_INDEX_FORMAT="4", LUME_TIMING="1"),
                       stdout=out, stderr=err, check=True)
    build = {"seconds": time.perf_counter() - started,
             "peak_child_rss_bytes": resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss * 1024,
             "timings": timings((a.output / "index.stderr.log").read_text())}
    with a.queries.open(encoding="utf-8") as stream:
        queries = list(csv.reader(stream, delimiter="\t"))
    report = {"binary_sha256": hashlib.sha256(a.binary.read_bytes()).hexdigest(),
              "toolchain": "rustc 1.96 release thin LTO CGU1",
              "storage": "Linux Docker volumes; frozen source and existing JSON index",
              "cold_definition": "fresh processes, page-cache primed, no OS cache flush",
              "build": build, "hot": [], "opens": [], "files": []}
    if not queries:
        raise ValueError("Empty query set")
    preflight = []
    for label, db in (("json", a.existing), ("v4", a.new_db)):
        a.query_db.symlink_to(db, target_is_directory=True)
        try:
            reply, _ = resident(a.binary, a.query_db, queries[:1],
                                dict(env, LUME_COORD_FLOOR="1.0", LUME_TIMING="0"),
                                a.output / (label + "-preflight.server.log"), 0)
        finally:
            a.query_db.unlink()
        raw = reply[queries[0][0]]
        preflight.append(raw)
        (a.output / (label + "-preflight.reply.json")).write_bytes(raw)
    if preflight[0] != preflight[1]:
        raise RuntimeError("Preflight reply/header byte mismatch; refusing full timing run")
    report["preflight_byte_parity"] = True
    # Both coordination profiles use the SAME frozen stemmed corpus/index pair.
    # Unstemmed profile parity remains covered by core score-bit tests.
    for floor in ("0.5", "1.0"):
        replies = []
        for label, db in (("json", a.existing), ("v4", a.new_db)):
            a.query_db.symlink_to(db, target_is_directory=True)
            try:
                raw, samples = resident(a.binary, a.query_db, queries,
                                    dict(env, LUME_COORD_FLOOR=floor, LUME_TIMING="0"),
                                        a.output / (label + "-" + floor + ".server.log"), a.passes)
            finally:
                a.query_db.unlink()
            report["hot"].append({"format": label, "floor": floor, "reply_sha256": {
                qid: hashlib.sha256(raw[qid]).hexdigest() for qid, _ in queries}})
            replies.append(raw)
            values = sorted(row["ms"] for row in samples)
            report["hot"].append({"format": label, "floor": floor,
                                  "p50_ms": statistics.median(values),
                                  "p95_ms": values[min(len(values)-1, int(.95 * len(values)))],
                                  "samples": samples})
            (a.output / (label + "-" + floor + ".replies.json")).write_text(
                json.dumps({key: value.decode() for key, value in raw.items()}) + "\n")
        if replies[0] != replies[1]:
            misses = [qid for qid, _ in queries if replies[0][qid] != replies[1][qid]]
            (a.output / "parity-misses.json").write_text(json.dumps(misses) + "\n")
            raise RuntimeError("Hot reply byte parity failed: " + str(misses))
    for label, db in (("json", a.existing), ("v4", a.new_db)):
        cmd = [str(a.binary), "search", "--db", str(db), "--alpha", "0", "--graph", "0",
               "coronavirus vaccine"]
        baseline = subprocess.run(cmd, env=dict(env, LUME_TIMING="0"), capture_output=True, check=True)
        for iteration in range(3):
            started = time.perf_counter()
            result = subprocess.run(cmd, env=dict(env, LUME_TIMING="1"), capture_output=True, check=True)
            elapsed = time.perf_counter() - started
            if result.stdout != baseline.stdout:
                raise RuntimeError("Timing changed CLI stdout")
            stages = timings(result.stderr.decode())
            if not any(row["phase"] == "open.total" for row in stages):
                raise RuntimeError("Missing cold-open timing")
            report["opens"].append({"format": label, "iteration": iteration,
                                    "process_seconds": elapsed, "timings": stages})
    for label, db in (("json", a.existing), ("v4", a.new_db)):
        files = [{"path": str(path.relative_to(db)), "bytes": path.stat().st_size}
                 for path in sorted(db.rglob("*")) if path.is_file()]
        report["files"].append({"format": label, "total_bytes": sum(row["bytes"] for row in files),
                                "files": files})
    report["reply_byte_parity"] = True
    (a.output / "summary.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: report[key] for key in ("binary_sha256", "reply_byte_parity", "files")},
                     indent=2), flush=True)


if __name__ == "__main__":
    main()

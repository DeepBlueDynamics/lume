#!/usr/bin/env python3
"""Release fetch, document staging, measured index build, and MCP benchmark."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import re
import statistics
import subprocess
import tarfile
import threading
import time
import urllib.request

RELEASE = "v0.12.2"
ASSET = "lume-" + RELEASE + "-x86_64-unknown-linux-gnu.tar.gz"


def fetch(root):
    dest = root / "bin"
    dest.mkdir(parents=True, exist_ok=True)
    base = "https://github.com/DeepBlueDynamics/lume/releases/download/" + RELEASE + "/"
    checksum = urllib.request.urlopen(base + "SHA256SUMS", timeout=120).read().decode()
    matches = [line.split()[0] for line in checksum.splitlines() if line.split()[-1].lstrip("*") == ASSET]
    if len(matches) != 1:
        raise ValueError("missing/duplicate release checksum")
    archive = dest / ASSET
    with urllib.request.urlopen(base + ASSET, timeout=120) as source, archive.open("wb") as out:
        while chunk := source.read(1024 * 1024):
            out.write(chunk)
    h = hashlib.sha256()
    with archive.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            h.update(chunk)
    if h.hexdigest() != matches[0]:
        raise ValueError("release checksum mismatch")
    with tarfile.open(archive) as tar:
        members = [m for m in tar.getmembers() if m.isfile() and m.name.endswith("/lume")]
        if len(members) != 1:
            raise ValueError("ambiguous binary archive")
        with tar.extractfile(members[0]) as source, (dest / "lume-released").open("wb") as out:
            while chunk := source.read(1024 * 1024):
                out.write(chunk)
    (dest / "lume-released").chmod(0o755)
    (dest / "release.json").write_text(json.dumps({"version": RELEASE, "asset": ASSET,
        "archive_sha256": h.hexdigest(), "source_commit": "5d88268"}, indent=2) + "\n")
    print("Release SHA256 verified:", h.hexdigest(), flush=True)


def files(root, dataset):
    folder = root / dataset / "files"
    folder.mkdir(exist_ok=True)
    count = 0
    with (root / dataset / "docs.jsonl").open(encoding="utf-8") as source:
        for line in source:
            row = json.loads(line)
            docid = row["id"]
            if not re.fullmatch(r"[A-Za-z0-9_.-]+", docid) or docid in (".", ".."):
                raise ValueError("unsafe filename id")
            (folder / (docid + ".txt")).write_text(row["text"], encoding="utf-8")
            count += 1
    print("Staged documents:", count, flush=True)


def index(root, dataset, binary):
    import resource
    index_dir = root / dataset / "lume-index"
    if index_dir.exists():
        raise ValueError("index already exists: do not accidentally measure an update")
    start = time.perf_counter()
    subprocess.run([str(binary), "index", str(root / dataset / "files"), "--db", str(index_dir)], check=True)
    elapsed = time.perf_counter() - start
    size = sum(p.stat().st_size for p in index_dir.rglob("*") if p.is_file())
    row = {"index_seconds": elapsed, "index_bytes": size,
           "peak_rss_bytes": resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss * 1024,
           "idle_rss_bytes": None, "binary": str(binary), "cpu_limit": 8, "memory_limit_bytes": 8 * 1024**3}
    runs = root / "runs"
    runs.mkdir(exist_ok=True)
    (runs / ("lume-" + dataset + ".build.json")).write_text(json.dumps(row, indent=2) + "\n")
    print("BUILD", json.dumps(row), flush=True)


HIT = re.compile(r"^\[\d+\] Score: ([-+0-9.eE]+).*? \(File: (.*), Line: \d+\)$", re.MULTILINE)


def serve(root, dataset, binary, variant):
    token_path = root / "http-token.txt"
    if not token_path.exists():
        import secrets
        token_path.write_text(secrets.token_hex(32))
        token_path.chmod(0o600)
    child = subprocess.Popen([str(binary), "serve", "--bind", "0.0.0.0", "--port", "5863",
                              "--http-token-file", str(token_path)])
    marker = root / "runs" / (variant + "-" + dataset + ".warm")
    idle = marker.with_suffix(".idle.json")
    marker.unlink(missing_ok=True)
    idle.unlink(missing_ok=True)
    try:
        while child.poll() is None:
            if marker.exists() and not idle.exists():
                status = Path("/proc/" + str(child.pid) + "/status").read_text()
                rss = int(re.search(r"^VmRSS:\s+(\d+)", status, re.MULTILINE).group(1)) * 1024
                idle.write_text(json.dumps({"idle_rss_bytes": rss}) + "\n")
                print("IDLE_RSS_BYTES", rss, flush=True)
            time.sleep(.1)
    finally:
        if child.poll() is None:
            child.terminate()
        child.wait()
    if child.returncode:
        raise SystemExit(child.returncode)


def document_hits(text):
    scores = {}
    for match in HIT.finditer(text):
        score = float(match.group(1))
        docid = match.group(2).replace("\\", "/").rsplit("/", 1)[-1]
        if not docid.endswith(".txt") or not math.isfinite(score):
            raise ValueError("unexpected hit")
        docid = docid[:-4]
        scores[docid] = max(scores.get(docid, -math.inf), score)
    if not scores and "No hits found." not in text:
        raise ValueError("unrecognized MCP search output")
    return sorted(scores.items(), key=lambda pair: (-pair[1], pair[0]))[:100]


def request(url, db, query, graph, token):
    # Ask for enough sections to collapse to 100 documents. Increasing the
    # limit does not change ranking; the server returns its own section order.
    limit = 100
    while True:
        payload = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "lume_search", "arguments": {"query": query, "db": db,
                "limit": limit, "alpha": 0, "graph": graph}}}
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        req = urllib.request.Request(url, data=json.dumps(payload).encode(), headers=headers)
        with urllib.request.urlopen(req, timeout=300) as response:
            reply = json.load(response)
        if "error" in reply or reply.get("result", {}).get("isError"):
            raise ValueError("MCP search error: " + str(reply))
        text = "\n".join(c["text"] for c in reply["result"]["content"] if c["type"] == "text")
        hits = document_hits(text)
        section_count = len(HIT.findall(text))
        if len(hits) >= 100 or section_count < limit:
            return hits
        limit *= 2
        if limit > 65536:
            raise ValueError("cannot obtain 100 document hits within section cap")


def benchmark(root, dataset, variant, url, db, token):
    runs = root / "runs"
    runs.mkdir(exist_ok=True)
    with (root / dataset / "queries.tsv").open(encoding="utf-8") as source:
        queries = list(csv.reader(source, delimiter="\t"))
    for mode, graph in (("bm25", 0), ("default", .4)):
        name = "lume-" + variant + "-" + mode + "-" + dataset
        samples, results = {}, {}
        for pass_no in range(4):
            for qid, query in queries:
                start = time.perf_counter()
                hits = request(url, db, query, graph, token)
                ms = (time.perf_counter() - start) * 1000
                if pass_no:
                    if qid in results and results[qid] != hits:
                        raise ValueError("non-deterministic ranking: " + qid)
                    samples.setdefault(qid, []).append(ms)
                    results[qid] = hits
            print(name, "pass", pass_no, "complete", flush=True)
            if pass_no == 0:
                marker = runs / (variant + "-" + dataset + ".warm")
                marker.write_text("warmup complete")
                idle = marker.with_suffix(".idle.json")
                deadline_idle = time.monotonic() + 10
                while not idle.exists() and time.monotonic() < deadline_idle:
                    time.sleep(.1)
                if not idle.exists():
                    raise ValueError("server idle RSS was not captured")
                build_path = runs / ("lume-" + dataset + ".build.json")
                build = json.loads(build_path.read_text())
                build.update(json.loads(idle.read_text()))
                (runs / ("lume-" + variant + "-" + dataset + ".build.json")).write_text(json.dumps(build, indent=2) + "\n")
        with (runs / (name + ".trec")).open("w", encoding="utf-8", newline="\n") as out:
            # Same tag for both versions, so entire TREC files can be compared.
            for qid, _ in queries:
                for rank, (docid, score) in enumerate(results[qid], 1):
                    out.write(f"{qid} Q0 {docid} {rank} {score:.4f} lume\n")
        with (runs / (name + ".lat.jsonl")).open("w", encoding="utf-8", newline="\n") as out:
            for qid, _ in queries:
                out.write(json.dumps({"qid": qid, "ms": statistics.median(samples[qid])}) + "\n")
        deadline = time.perf_counter() + 60
        counter, errors = 0, 0
        lock = threading.Lock()
        def worker(number):
            nonlocal counter, errors
            i = number
            while time.perf_counter() < deadline:
                try:
                    request(url, db, queries[i % len(queries)][1], graph, token)
                    with lock:
                        counter += 1
                except Exception:
                    with lock:
                        errors += 1
                i += 8
        start = time.perf_counter()
        with ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(worker, range(8)))
        elapsed = time.perf_counter() - start
        throughput = {"qps": counter / elapsed, "seconds": elapsed, "concurrency": 8,
                      "requests": counter, "errors": errors}
        (runs / (name + ".throughput.json")).write_text(json.dumps(throughput, indent=2) + "\n")
        print(name, json.dumps(throughput), flush=True)
        if errors:
            raise ValueError("throughput errors; run is not a clean comparison")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("stage", choices=["fetch", "files", "index", "serve", "query"])
    p.add_argument("--root", type=Path, required=True)
    p.add_argument("--dataset", choices=["scifact", "trec-covid"], default="scifact")
    p.add_argument("--binary", type=Path)
    p.add_argument("--variant", choices=["released", "resident"], default="released")
    p.add_argument("--url", default="http://lume-engine:5863/mcp")
    p.add_argument("--db")
    p.add_argument("--token-file", type=Path)
    args = p.parse_args()
    if args.stage == "fetch":
        fetch(args.root)
    elif args.stage == "files":
        files(args.root, args.dataset)
    elif args.stage == "index":
        index(args.root, args.dataset, args.binary or args.root / "bin/lume-released")
    elif args.stage == "serve":
        serve(args.root, args.dataset, args.binary or args.root / "bin/lume-released", args.variant)
    else:
        token = args.token_file.read_text().strip() if args.token_file else ""
        benchmark(args.root, args.dataset, args.variant, args.url,
                  args.db or str(args.root / args.dataset / "lume-index"), token)


if __name__ == "__main__":
    main()

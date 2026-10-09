#!/usr/bin/env python3
"""Run Luxir benchmark for BEIR datasets (SciFact, TREC-COVID).

Uses shared http_runner.py (JsonClient, throughput) over Docker bridge network.
Produces:
- <root>/runs/luxir-<mode>-<ds>.trec
- <root>/runs/luxir-<mode>-<ds>.lat.jsonl
- <root>/runs/luxir-<ds>.build.json
- <root>/runs/luxir-<mode>-<ds>.throughput.json
"""
import argparse
import csv
import json
import math
import os
from pathlib import Path
import statistics
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from http_runner import JsonClient, throughput


def http_post(url, data=None, timeout=60, retries=3):
    body = json.dumps(data).encode("utf-8") if data is not None else None
    headers = {"Content-Type": "application/json"} if data is not None else {}
    for attempt in range(retries):
        try:
            req = urllib.request.Request(url, data=body, headers=headers, method="POST")
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                res = json.loads(resp.read().decode("utf-8"))
                if isinstance(res, dict) and "error" in res:
                    raise RuntimeError(f"Luxir error: {res['error']}")
                return resp.status, res
        except (urllib.error.URLError, TimeoutError, ConnectionError, OSError):
            if attempt == retries - 1:
                raise
            time.sleep(0.2)


def http_get(url, timeout=30):
    req = urllib.request.Request(url, method="GET")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.status, json.loads(resp.read().decode("utf-8"))


class MemoryMonitor:
    def __init__(self, rss_path):
        self.rss_path = Path(rss_path)
        self.stop_event = threading.Event()
        self.peak_rss = 0
        self.thread = None

    def _loop(self):
        while not self.stop_event.is_set():
            try:
                if self.rss_path.exists():
                    data = json.loads(self.rss_path.read_text(encoding="utf-8"))
                    curr = data.get("current_rss_bytes", 0)
                    if curr > self.peak_rss:
                        self.peak_rss = curr
            except Exception:
                pass
            time.sleep(0.05)

    def start(self):
        if self.rss_path.exists():
            try:
                data = json.loads(self.rss_path.read_text(encoding="utf-8"))
                self.peak_rss = data.get("current_rss_bytes", 0)
            except Exception:
                pass
        self.stop_event.clear()
        self.thread = threading.Thread(target=self._loop, daemon=True)
        self.thread.start()

    def stop(self):
        self.stop_event.set()
        if self.thread:
            self.thread.join(timeout=2.0)
        idle_rss = 0
        if self.rss_path.exists():
            try:
                data = json.loads(self.rss_path.read_text(encoding="utf-8"))
                idle_rss = data.get("current_rss_bytes", 0)
            except Exception:
                pass
        return self.peak_rss, idle_rss


class LuxirAdmin:
    def __init__(self, endpoint="http://luxir-bench:9400", embed_endpoint="http://host.docker.internal:8085/embed"):
        self.endpoint = endpoint.rstrip("/")
        self.embed_endpoint = embed_endpoint

    def create_collection(self, name, mode="bm25"):
        try:
            http_post(f"{self.endpoint}/collections/_delete", {"name": name})
        except Exception:
            pass

        http_post(f"{self.endpoint}/collections/_create", {"name": name})

        fields = {"text": {"parent": "_t"}}
        if mode == "hybrid":
            fields["embedding_v"] = {"type": "vector", "dims": 768, "metric": "cosine"}

        http_post(f"{self.endpoint}/collections/{name}/_schema", {"fields": fields})

    def update_docs(self, name, docs, commit=False):
        url = f"{self.endpoint}/collections/{name}/_update"
        if commit:
            url += "?commit=true"
        status, resp = http_post(url, {"docs": docs}, timeout=120)
        return resp

    def embed_texts(self, texts, batch_size=64):
        vectors = []
        for i in range(0, len(texts), batch_size):
            batch = texts[i:i + batch_size]
            status, resp = http_post(self.embed_endpoint, {"texts": batch}, timeout=60)
            vectors.extend(resp["vectors"])
        return vectors


def get_dir_size(path):
    total = 0
    p = Path(path)
    if p.exists():
        for f in p.rglob("*"):
            if f.is_file():
                total += f.stat().st_size
    return total


def index_dataset(admin, dataset_dir, collection, mode, mem_monitor, luxir_data_dir, batch_size=500):
    docs_file = Path(dataset_dir) / "docs.jsonl"
    print(f"Reading docs from {docs_file}...")

    all_docs = []
    with docs_file.open(encoding="utf-8") as f:
        for line in f:
            if line.strip():
                all_docs.append(json.loads(line))

    print(f"Loaded {len(all_docs)} documents.")

    admin.create_collection(collection, mode=mode)

    mem_monitor.start()
    t0 = time.perf_counter()

    for i in range(0, len(all_docs), batch_size):
        batch = all_docs[i:i + batch_size]
        is_last = (i + batch_size >= len(all_docs))

        if mode == "hybrid":
            texts = [d["text"] for d in batch]
            vecs = admin.embed_texts(texts)
            doc_batch = [
                {"id": d["id"], "text": d["text"], "embedding_v": v}
                for d, v in zip(batch, vecs)
            ]
        else:
            doc_batch = [{"id": d["id"], "text": d["text"]} for d in batch]

        admin.update_docs(collection, doc_batch, commit=is_last)
        if (i // batch_size) % 10 == 0 or is_last:
            print(f"Indexed {min(i + len(batch), len(all_docs))}/{len(all_docs)} docs...")

    t1 = time.perf_counter()
    wall_clock_s = round(t1 - t0, 3)

    time.sleep(3.0)

    peak_rss, idle_rss = mem_monitor.stop()

    coll_dir = Path(luxir_data_dir) / "c" / collection
    index_bytes = get_dir_size(coll_dir)
    try:
        st_code, st = http_get(f"{admin.endpoint}/collections/{collection}/_stats")
        for c in st.get("collections", []):
            if c.get("name") == collection:
                b = c.get("totals", {}).get("bytes", 0)
                if b > 0:
                    index_bytes = b
                break
        if not index_bytes:
            b = st.get("totals", {}).get("bytes", 0)
            if b > 0:
                index_bytes = b
    except Exception:
        pass

    build_stats = {
        "wall_clock_s": wall_clock_s,
        "index_bytes": index_bytes,
        "peak_rss_bytes": peak_rss,
        "idle_rss_bytes": idle_rss
    }
    print(f"Build complete in {wall_clock_s}s: {index_bytes} bytes, peak RSS {peak_rss}, idle RSS {idle_rss}")
    return build_stats


def read_queries(queries_path, qrels_path):
    test_qids = set()
    with Path(qrels_path).open(encoding="utf-8") as f:
        for qid, _, _ in csv.reader(f, delimiter="\t"):
            test_qids.add(qid)

    queries = []
    with Path(queries_path).open(encoding="utf-8") as f:
        for qid, text in csv.reader(f, delimiter="\t"):
            if qid in test_qids:
                queries.append((qid, text))
    return queries


def build_payload(mode, text, vector=None, limit=100, rrf_k=60):
    if mode == "hybrid":
        return {
            "ops": {
                "hybrid": {
                    "fusion": {
                        "sources": {
                            "lexical": {
                                "query": {"match": {"text": text}},
                                "limit": limit
                            },
                            "semantic": {
                                "query": {"knn": {"field": "embedding_v", "query": vector, "k": limit}},
                                "limit": limit
                            }
                        },
                        "rrf": {"k": rrf_k},
                        "limit": limit,
                        "get_scores": True,
                        "fields": ["id"]
                    }
                }
            }
        }
    else:
        return {
            "query": {"match": {"text": text}},
            "limit": limit,
            "get_scores": True,
            "fields": ["id"]
        }


def extract_hits(reply, mode):
    if "error" in reply:
        raise ValueError(f"Luxir error: {reply['error']}")
    if mode == "hybrid":
        docs = reply.get("ops", {}).get("hybrid", {}).get("docs", [])
    else:
        docs = reply.get("docs", [])
    # Sort descending by score, take top 100
    sorted_docs = sorted(docs, key=lambda d: d.get("_score_", 0.0), reverse=True)[:100]
    return [(d["id"], float(d.get("_score_", 0.0))) for d in sorted_docs]


def evaluate_queries(endpoint, collection, mode, queries, query_vecs):
    print(f"Running queries evaluation: {len(queries)} test queries in mode={mode}...")
    search_url = f"{endpoint}/collections/{collection}/_search"
    client = JsonClient(search_url)

    def do_search(qid, text, cl):
        vec = query_vecs.get(qid) if mode == "hybrid" else None
        payload = build_payload(mode, text, vector=vec, limit=100)
        reply = cl.post(payload)
        return extract_hits(reply, mode)

    # Warmup pass (1 complete pass)
    print("Warmup pass...")
    for qid, text in queries:
        do_search(qid, text, client)

    # 3 timed passes
    times_per_query = {qid: [] for qid, _ in queries}
    last_results = {}

    for p in range(3):
        print(f"Timed pass {p + 1}/3...")
        for qid, text in queries:
            t0 = time.perf_counter()
            hits = do_search(qid, text, client)
            ms = (time.perf_counter() - t0) * 1000.0
            times_per_query[qid].append(ms)
            if p == 2:
                last_results[qid] = hits

    client.close()

    latencies = {qid: statistics.median(times_per_query[qid]) for qid, _ in queries}

    trec_rows = []
    tag = f"luxir-{mode}"
    for qid, _ in queries:
        hits = last_results.get(qid, [])
        for rank, (docid, score) in enumerate(hits, start=1):
            trec_rows.append((qid, "Q0", docid, rank, score, tag))

    return latencies, trec_rows


def run_throughput_suite(endpoint, collection, mode, queries, query_vecs, seconds=60):
    print(f"Running throughput test with persistent HTTP pooling on {endpoint}...")
    search_url = f"{endpoint}/collections/{collection}/_search"

    def operation(worker_client, query_item):
        qid, text = query_item
        vec = query_vecs.get(qid) if mode == "hybrid" else None
        payload = build_payload(mode, text, vector=vec, limit=100)
        reply = worker_client.post(payload)
        hits = extract_hits(reply, mode)
        return hits

    measured = [
        throughput(
            client_factory=lambda: JsonClient(search_url),
            queries=queries,
            operation=operation,
            workers=w,
            seconds=seconds
        )
        for w in (8, 16)
    ]

    row = {
        **measured[0],
        "worker_scaling": measured,
        "qps_gain_16_over_8": measured[1]["qps"] / measured[0]["qps"] - 1 if measured[0]["qps"] > 0 else 0.0
    }
    print(f"Throughput complete: {row['qps']:.2f} QPS (8 workers), {measured[1]['qps']:.2f} QPS (16 workers), scaling gain: {row['qps_gain_16_over_8']*100:.1f}%")
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dataset", required=True, choices=["scifact", "trec-covid"])
    parser.add_argument("--mode", required=True, choices=["bm25", "hybrid"])
    parser.add_argument("--root", type=Path, default=Path("/workspace/lume/.lanes/data/luxir-bench"))
    parser.add_argument("--endpoint", default="http://luxir-bench:9400")
    parser.add_argument("--embed-endpoint", default="http://host.docker.internal:8085/embed")
    parser.add_argument("--skip-build", action="store_true", help="Skip index build if collection already indexed")
    parser.add_argument("--skip-throughput", action="store_true", help="Skip throughput suite")
    parser.add_argument("--batch-size", type=int, default=500)
    parser.add_argument("--throughput-duration", type=float, default=60.0)
    args = parser.parse_args()

    admin = LuxirAdmin(endpoint=args.endpoint, embed_endpoint=args.embed_endpoint)
    dataset_dir = args.root / args.dataset
    luxir_data_dir = args.root / "luxir_data"
    runs_dir = args.root / "runs"
    runs_dir.mkdir(parents=True, exist_ok=True)

    collection = f"{args.dataset.replace('-', '_')}_{args.mode}"
    engine = "luxir"
    run_name = f"{engine}-{args.mode}-{args.dataset}"

    mem_monitor = MemoryMonitor(args.root / "rss.json")

    # 1. Indexing
    build_json_path = runs_dir / f"{engine}-{args.dataset}.build.json"
    if not args.skip_build:
        build_stats = index_dataset(
            admin=admin,
            dataset_dir=dataset_dir,
            collection=collection,
            mode=args.mode,
            mem_monitor=mem_monitor,
            luxir_data_dir=luxir_data_dir,
            batch_size=args.batch_size
        )
        build_json_path.write_text(json.dumps(build_stats, indent=2) + "\n", encoding="utf-8")
        print(f"Saved build stats to {build_json_path}")
    else:
        print(f"Skipping index build; using existing collection {collection}")

    # 2. Queries & Latency
    queries = read_queries(dataset_dir / "queries.tsv", dataset_dir / "qrels.tsv")
    query_vecs = {}
    if args.mode == "hybrid":
        print(f"Embedding {len(queries)} test queries with Shivvr...")
        texts = [text for _, text in queries]
        vecs = admin.embed_texts(texts)
        for (qid, _), v in zip(queries, vecs):
            query_vecs[qid] = v

    latencies, trec_rows = evaluate_queries(args.endpoint, collection, args.mode, queries, query_vecs)

    trec_path = runs_dir / f"{run_name}.trec"
    with trec_path.open("w", encoding="utf-8") as f:
        for qid, q0, docid, rank, score, tag in trec_rows:
            f.write(f"{qid} {q0} {docid} {rank} {score:.6f} {tag}\n")
    print(f"Wrote {len(trec_rows)} TREC rows to {trec_path}")

    lat_path = runs_dir / f"{run_name}.lat.jsonl"
    with lat_path.open("w", encoding="utf-8") as f:
        for qid, ms in latencies.items():
            f.write(json.dumps({"qid": qid, "ms": round(ms, 3)}) + "\n")
    print(f"Wrote {len(latencies)} query latencies to {lat_path}")

    # 3. Throughput
    throughput_path = runs_dir / f"{run_name}.throughput.json"
    if not args.skip_throughput:
        throughput_stats = run_throughput_suite(
            endpoint=args.endpoint,
            collection=collection,
            mode=args.mode,
            queries=queries,
            query_vecs=query_vecs,
            seconds=args.throughput_duration
        )
        throughput_path.write_text(json.dumps(throughput_stats, indent=2) + "\n", encoding="utf-8")
        print(f"Saved throughput stats to {throughput_path}")


if __name__ == "__main__":
    main()

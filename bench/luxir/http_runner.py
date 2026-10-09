"""Shared stdlib HTTP transport and concurrent wall-clock throughput measurement."""
from concurrent.futures import ThreadPoolExecutor
import http.client
import json
import socket
import threading
import time
from urllib.parse import urlsplit


class JsonClient:
    """One connection per worker; responses are fully drained for reuse."""
    def __init__(self, url, headers=None, timeout=300):
        self.url = urlsplit(url)
        if self.url.scheme not in ("http", "https"):
            raise ValueError("HTTP(S) URL required")
        self.headers = {"Content-Type": "application/json", **(headers or {})}
        self.timeout = timeout
        self.connection = None

    def close(self):
        if self.connection:
            self.connection.close()
            self.connection = None

    def post(self, payload):
        if self.connection is None:
            cls = http.client.HTTPSConnection if self.url.scheme == "https" else http.client.HTTPConnection
            self.connection = cls(self.url.hostname, self.url.port, timeout=self.timeout)
            self.connection.connect()
            self.connection.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        path = self.url.path or "/"
        if self.url.query:
            path += "?" + self.url.query
        try:
            self.connection.request("POST", path, json.dumps(payload).encode(), self.headers)
            response = self.connection.getresponse()
            body = response.read()
            status = response.status
            closes = response.will_close
            if closes:
                self.close()
            if not 200 <= status < 300:
                raise ValueError("HTTP status " + str(status))
            return json.loads(body)
        except Exception:
            self.close()
            raise


def throughput(client_factory, queries, operation, workers=8, seconds=60):
    """operation(client, query) must validate the response just as latency does."""
    ready = threading.Barrier(workers + 1)
    start_at = [0.0]
    finish_at = [0.0]

    def worker(number):
        client = client_factory()
        count, errors = 0, 0
        ready.wait()
        index = number
        try:
            while time.perf_counter() < finish_at[0]:
                try:
                    operation(client, queries[index % len(queries)])
                    count += 1
                except Exception:
                    errors += 1
                index += workers
        finally:
            client.close()
        return count, errors

    # Release workers together. No shared lock in the request hot path.
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(worker, i) for i in range(workers)]
        cpu_start = time.process_time()
        start_at[0] = time.perf_counter()
        finish_at[0] = start_at[0] + seconds
        ready.wait()
        results = [f.result() for f in futures]
        elapsed = time.perf_counter() - start_at[0]
        cpu_seconds = time.process_time() - cpu_start
    count = sum(r[0] for r in results)
    return {"qps": count / elapsed, "seconds": elapsed, "concurrency": workers,
            "requests": count, "errors": sum(r[1] for r in results),
            "driver_cpu_seconds": cpu_seconds,
            "driver_cpu_percent_one_core": 100 * cpu_seconds / elapsed,
            "transport": "persistent HTTP connection per worker; TCP_NODELAY",
            "includes_inflight_drain": True}

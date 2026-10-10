"""Foreground H1 engine with an instrumented local Shivvr proxy."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import urllib.error
import urllib.request

p = argparse.ArgumentParser()
p.add_argument("--root", type=Path, required=True)
p.add_argument("--binary", required=True)
a = p.parse_args()
events = a.root / "runs/h1-shivvr-calls.jsonl"
events.write_text("")
lock = threading.Lock()

class Proxy(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def forward(self):
        started = time.perf_counter()
        data = self.rfile.read(int(self.headers.get("Content-Length", 0))) or None
        headers = {k: v for k, v in self.headers.items()
                   if k.lower() not in ("host", "connection", "content-length")}
        req = urllib.request.Request("http://host.docker.internal:8085" + self.path,
                                     data=data, headers=headers, method=self.command)
        status = 502
        try:
            try:
                response = urllib.request.urlopen(req, timeout=65)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                status = response.status
                body = response.read()
                content_type = response.headers.get("Content-Type", "application/json")
        except OSError:
            body = b'{"error":"Shivvr proxy upstream failed"}'
            content_type = "application/json"
        with lock:
            with events.open("a") as output:
                output.write(json.dumps({"method": self.command,
                    "route": self.path.split("?")[0], "status": status,
                    "ms": (time.perf_counter() - started) * 1000}) + "\n")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = forward
    do_POST = forward
    do_DELETE = forward

proxy = ThreadingHTTPServer(("127.0.0.1", 8086), Proxy)
thread = threading.Thread(target=proxy.serve_forever, daemon=True)
thread.start()
env = dict(os.environ, SHIVVR_BASE_URL="http://127.0.0.1:8086",
           LUME_STEM="1", LUME_COORD_FLOOR="1.0", LUME_QUERY_INVERSION="0")
child = subprocess.Popen([a.binary, "serve", "--bind", "0.0.0.0", "--port", "5863",
                          "--http-token-file", str(a.root / "http-token.txt")], env=env)
print("H1 engine + counting proxy ready", flush=True)
try:
    raise SystemExit(child.wait())
finally:
    if child.poll() is None:
        child.terminate()
        child.wait()
    proxy.shutdown()
    proxy.server_close()

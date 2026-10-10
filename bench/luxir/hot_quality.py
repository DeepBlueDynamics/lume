"""Four quality-only parity checks in a foreground supervisor, no timing claims."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.request
from run_quality import collect

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--step", required=True)
args = parser.parse_args()
token = (args.root / "http-token.txt").read_text().strip()
url = "http://127.0.0.1:5863/mcp"
for profile in ("off", "f3-f2"):
    for dataset in ("scifact", "trec-covid"):
        label = args.step + "-" + profile + "-quality"
        db = ("/indexes/scifact" if dataset == "scifact" else "/indexes/trec-covid/lume-index")
        baseline = "lume-resident-bm25-" + dataset + ".trec"
        if profile == "f3-f2":
            db = "/indexes/hot-path/" + dataset + "/stemmed"
            baseline = "lume-f3-f2-r2-bm25-" + dataset + ".trec"
        env = dict(os.environ, LUME_COORD_FLOOR="0.5" if profile == "off" else "1.0")
        log_path = args.root / "runs" / (label + "-" + dataset + ".server.log")
        with log_path.open("wb") as logging:
            child = subprocess.Popen([str(args.binary), "serve", "--bind", "127.0.0.1",
                "--port", "5863", "--http-token-file", str(args.root / "http-token.txt")],
                env=env, stdout=logging, stderr=logging)
            try:
                deadline = time.monotonic() + 60
                while True:
                    if child.poll() is not None:
                        raise RuntimeError("quality server exited")
                    try:
                        req = urllib.request.Request(url, data=b'{"jsonrpc":"2.0","id":1,"method":"tools/list"}',
                            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
                        with urllib.request.urlopen(req, timeout=2) as reply:
                            json.load(reply)
                        break
                    except OSError:
                        if time.monotonic() > deadline:
                            raise RuntimeError("quality server failed to become ready")
                        time.sleep(.1)
                collect(args.root, dataset, label, db, url, {"profile": profile, "quality_only": True})
                actual = (args.root / "runs" / ("lume-" + label + "-bm25-" + dataset + ".trec")).read_bytes()
                expected = (args.root / "runs" / baseline).read_bytes()
                if actual != expected:
                    raise RuntimeError("byte parity failed: " + profile + " " + dataset)
                print("BYTE PARITY PASS:", profile, dataset, flush=True)
            finally:
                if child.poll() is None:
                    child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
print("All four quality parity checks passed.", flush=True)

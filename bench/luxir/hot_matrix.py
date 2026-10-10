"""Drive sequential old/new warm MCP runs outside the engine's resource cap."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import time
import urllib.request
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--step", required=True)
parser.add_argument("--binary", required=True)
args = parser.parse_args()
root = args.root
token = (root / "http-token.txt").read_text().strip()
for profile in ("off", "f3-f2"):
    for dataset in ("scifact", "trec-covid"):
        label = args.step + "-" + profile
        job = uuid.uuid4().hex
        command = {"id": job, "binary": args.binary, "profile": profile,
                   "label": label, "dataset": dataset}
        control = root / "runs/hot-session-command.json"
        staged = control.with_suffix(".part")
        staged.write_text(json.dumps(command) + "\n")
        staged.replace(control)
        deadline = time.monotonic() + 90
        while True:
            try:
                ready = json.loads((root / "runs/hot-session-ready.json").read_text())
                if ready.get("id") == job:
                    request = urllib.request.Request("http://lume-engine:5863/mcp",
                        data=b'{"jsonrpc":"2.0","id":1,"method":"tools/list"}',
                        headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
                    with urllib.request.urlopen(request, timeout=2) as reply:
                        json.load(reply)
                    break
            except (OSError, json.JSONDecodeError):
                pass
            if time.monotonic() > deadline:
                raise RuntimeError("server startup timeout")
            time.sleep(.1)
        db = "/indexes/scifact" if dataset == "scifact" else "/indexes/trec-covid/lume-index"
        baseline = "lume-resident-bm25-" + dataset + ".trec"
        if profile == "f3-f2":
            db = "/indexes/hot-path/" + dataset + "/stemmed"
            baseline = "lume-f3-f2-r2-bm25-" + dataset + ".trec"
        subprocess.run([sys.executable, "hot_measure.py", "--root", str(root),
                        "--label", label, "--dataset", dataset, "--db", db,
                        "--baseline", baseline], check=True)
print("Matrix DONE:", args.step, flush=True)

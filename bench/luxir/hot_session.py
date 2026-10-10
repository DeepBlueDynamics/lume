"""Visible foreground engine supervisor; clients select one job at a time."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
args = parser.parse_args()
root = args.root
control = root / "runs/hot-session-command.json"
ready = root / "runs/hot-session-ready.json"
ready.unlink(missing_ok=True)
control.unlink(missing_ok=True)
child = None
current = None

def stop():
    global child
    if child is not None and child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
    child = None

print("Foreground hot-path supervisor ready.", flush=True)
try:
    while True:
        try:
            command = json.loads(control.read_text())
        except (OSError, json.JSONDecodeError):
            time.sleep(.1)
            continue
        if command.get("stop"):
            break
        if command["id"] != current:
            stop()
            current = command["id"]
            env = dict(os.environ, LUME_COORD_FLOOR="0.5" if command["profile"] == "off" else "1.0",
                       LUME_STEM="0" if command["profile"] == "off" else "1")
            label, dataset = command["label"], command["dataset"]
            marker = root / "runs" / (label + "-" + dataset + ".warm")
            idle = marker.with_suffix(".idle.json")
            marker.unlink(missing_ok=True)
            idle.unlink(missing_ok=True)
            child = subprocess.Popen([command["binary"], "serve", "--bind", "0.0.0.0",
                "--port", "5863", "--http-token-file", str(root / "http-token.txt")], env=env)
            ready.write_text(json.dumps({"id": current, "pid": child.pid}) + "\n")
            print("START engine:", label, dataset, flush=True)
        if child.poll() is not None:
            raise RuntimeError("benchmark server exited " + str(child.returncode))
        if marker.exists() and not idle.exists():
            status = Path("/proc/" + str(child.pid) + "/status").read_text()
            rss = int(re.search(r"^VmRSS:\s+(\d+)", status, re.M)[1]) * 1024
            peak = int(re.search(r"^VmHWM:\s+(\d+)", status, re.M)[1]) * 1024
            idle.write_text(json.dumps({"idle_rss_bytes": rss, "warmup_peak_server_rss_bytes": peak}) + "\n")
            print("IDLE_RSS_BYTES", rss, flush=True)
        time.sleep(.1)
finally:
    stop()
print("DONE foreground supervisor.", flush=True)

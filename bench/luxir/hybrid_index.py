"""Build the existing semantic index; record embedding-inclusive time and logs."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time

p = argparse.ArgumentParser()
p.add_argument("--root", type=Path, required=True)
p.add_argument("--binary", required=True)
p.add_argument("--db", type=Path, required=True)
a = p.parse_args()
if a.db.exists():
    raise SystemExit("Refusing to update an existing benchmark index")
env = dict(os.environ, SHIVVR_BASE_URL="http://host.docker.internal:8085",
           LUME_STEM="1", LUME_COORD_FLOOR="1.0", LUME_QUERY_INVERSION="0")
log = a.root / "hot-path/h1-index.log"
started = time.perf_counter()
with log.open("wb") as output:
    result = subprocess.run([a.binary, "index", str(a.root / "scifact/files"),
                             "-s", "--db", str(a.db)], env=env,
                            stdout=output, stderr=subprocess.STDOUT)
text = log.read_text(errors="replace")
state = json.loads((a.db / "state.json").read_text()) if (a.db / "state.json").exists() else {}
chunks = re.findall(r"Semantic ingest complete: (\d+) chunks", text)
row = {"seconds_including_embedding": time.perf_counter() - started,
       "returncode": result.returncode, "semantic_session_present": bool(state.get("semantic_session_id")),
       "embedded_chunks": int(chunks[-1]) if chunks else None,
       "errors": [line for line in text.splitlines() if re.search(r"error|failed", line, re.I)],
       "model": "GTR-T5", "dimensions": 768, "db": str(a.db),
       "endpoint_workaround": "SHIVVR_BASE_URL; option shivvr_url is ignored (H2 fix queued)"}
(a.root / "runs/lume-h1-scifact.build.json").write_text(json.dumps(row, indent=2) + "\n")
print(json.dumps(row, indent=2), flush=True)
if result.returncode or not row["semantic_session_present"]:
    raise SystemExit("Semantic index failed; no lexical fallback benchmark")

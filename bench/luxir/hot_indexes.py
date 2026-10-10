"""Build shared stemmed indexes for old/new binary parity; never timed as queries."""
import argparse
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
args = parser.parse_args()
for dataset in ("scifact", "trec-covid"):
    output = Path("/indexes/hot-path") / dataset / "stemmed"
    if output.exists():
        raise RuntimeError("refusing existing index " + str(output))
    source = args.root / dataset / "files"
    if dataset == "trec-covid":
        source = Path("/indexes/trec-covid-files")
    subprocess.run([str(args.root / "bin/lume-relevance-r2"), "index", str(source),
                    "--db", str(output)], check=True)
    print("Built frozen F3 index:", output, flush=True)

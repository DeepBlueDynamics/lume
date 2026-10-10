"""Foreground supervisor for one hot-path benchmark engine."""
import argparse
from pathlib import Path
from run_lume import serve

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--label", required=True)
parser.add_argument("--dataset", required=True)
args = parser.parse_args()
serve(args.root, args.dataset, args.binary, args.label)

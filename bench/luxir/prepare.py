#!/usr/bin/env python3
"""Normalize BEIR test datasets; stdlib only. Outputs match the shared contract."""
import argparse
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import urllib.request
import zipfile

DATASETS = ("scifact", "trec-covid")
SOURCE = "https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/{}.zip"
DEFAULT_ROOT = Path(__file__).resolve().parents[2] / ".lanes/data/luxir-bench"
# A lane checkout sits below .lanes/w4; the shared directory is outside that lane.
if DEFAULT_ROOT.parent.parent.name == ".lanes":
    DEFAULT_ROOT = Path(__file__).resolve().parents[4] / ".lanes/data/luxir-bench"


def clean_tsv(text):
    return " ".join(str(text).split())


def prepare(dataset, root):
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    archive = root / (dataset + ".zip")
    if not archive.exists():
        partial = archive.with_suffix(".zip.part")
        try:
            with urllib.request.urlopen(SOURCE.format(dataset), timeout=120) as response, partial.open("wb") as out:
                total = 0
                while chunk := response.read(1024 * 1024):
                    total += len(chunk)
                    if total > 2 * 1024**3:
                        raise ValueError("archive exceeds 2 GiB download cap")
                    out.write(chunk)
            os.replace(partial, archive)
        finally:
            partial.unlink(missing_ok=True)
    dest = root / dataset
    dest.mkdir(exist_ok=True)
    with zipfile.ZipFile(archive) as z:
        def member(suffix):
            matches = [n for n in z.namelist() if n == suffix or n.endswith("/" + suffix)]
            if len(matches) != 1:
                raise ValueError("expected one archive member: " + suffix)
            return matches[0]
        # Read named members only; never extract archive paths.
        with z.open(member("qrels/test.tsv")) as stream:
            rows = csv.reader(io.TextIOWrapper(stream, encoding="utf-8"), delimiter="\t")
            header = next(rows)
            if header != ["query-id", "corpus-id", "score"]:
                raise ValueError("unexpected qrels header")
            qrels = [(q, d, int(g)) for q, d, g in rows]
        test_ids = {q for q, _, _ in qrels}
        queries = {}
        with z.open(member("queries.jsonl")) as stream:
            for line in stream:
                row = json.loads(line)
                qid = str(row["_id"])
                if qid in test_ids:
                    if qid in queries:
                        raise ValueError("duplicate query: " + qid)
                    queries[qid] = clean_tsv(row["text"])
        if queries.keys() != test_ids:
            raise ValueError("qrels contain missing queries")
        temp = dest / "docs.jsonl.part"
        count = 0
        ids = set()
        with z.open(member("corpus.jsonl")) as stream, temp.open("w", encoding="utf-8", newline="\n") as out:
            for line in stream:
                row = json.loads(line)
                docid = str(row["_id"])
                if any(c.isspace() for c in docid) or docid in ids:
                    raise ValueError("invalid/duplicate document id: " + docid)
                ids.add(docid)
                text = str(row.get("title") or "") + "\n\n" + str(row.get("text") or "")
                out.write(json.dumps({"id": docid, "text": text}, ensure_ascii=False) + "\n")
                count += 1
        if any(d not in ids for _, d, _ in qrels):
            raise ValueError("qrels contain missing documents")
        os.replace(temp, dest / "docs.jsonl")
    for name, rows in [
        ("queries.tsv", [(q, queries[q]) for q in sorted(queries)]),
        ("qrels.tsv", sorted(qrels)),
    ]:
        temp = dest / (name + ".part")
        with temp.open("w", encoding="utf-8", newline="") as out:
            writer = csv.writer(out, delimiter="\t", lineterminator="\n")
            writer.writerows(rows)
        os.replace(temp, dest / name)
    h = hashlib.sha256()
    with archive.open("rb") as f:
        while chunk := f.read(1024 * 1024):
            h.update(chunk)
    manifest = {"dataset": dataset, "source": SOURCE.format(dataset), "archive_sha256": h.hexdigest(),
                "documents": count, "test_queries": len(queries), "qrels": len(qrels)}
    (dest / "prepare.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest), flush=True)
    return manifest


def metadata(dataset, root):
    """Synthetic capability fields; never asserted to be real paper metadata."""
    import datetime
    import random
    rng = random.Random(42)
    directory = Path(root) / dataset
    temporary = directory / "docs_meta.jsonl.part"
    categories = ["biology", "medicine", "chemistry", "physics",
                  "computing", "ecology", "psychology", "engineering"]
    tags = ["tag_" + str(i).zfill(2) for i in range(20)]
    count = 0
    with (directory / "docs.jsonl").open(encoding="utf-8") as source, temporary.open(
            "w", encoding="utf-8", newline="\n") as output:
        for line in source:
            row = json.loads(line)
            year = rng.randint(1990, 2024)
            start = datetime.datetime(year, 1, 1, tzinfo=datetime.timezone.utc)
            end = start.replace(year=year + 1)
            published = start + datetime.timedelta(seconds=rng.randrange(int((end - start).total_seconds())))
            row.update(year=year, category=rng.choice(categories),
                       n_cites=rng.randint(0, 1000),
                       published_at=published.isoformat().replace("+00:00", "Z"),
                       tags=rng.sample(tags, rng.randint(1, 3)))
            output.write(json.dumps(row, ensure_ascii=False) + "\n")
            count += 1
    os.replace(temporary, directory / "docs_meta.jsonl")
    return {"documents": count, "seed": 42, "synthetic": True,
            "categories": categories, "tags": tags}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    p.add_argument("--datasets", nargs="+", choices=DATASETS, default=list(DATASETS))
    p.add_argument("--with-meta", action="store_true", help="also emit deterministic synthetic capability metadata (seed 42)")
    args = p.parse_args()
    for dataset in args.datasets:
        prepare(dataset, args.root)
        if args.with_meta:
            manifest = metadata(dataset, args.root)
            (args.root / dataset / "metadata.json").write_text(json.dumps(manifest, indent=2) + "\n")
            print(json.dumps(manifest), flush=True)


if __name__ == "__main__":
    main()

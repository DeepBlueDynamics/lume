#!/usr/bin/env python3
"""Read the shared Luxir/Lume embedding cache.

The file is JSONL, one {"id", "vector"} object per line. Its name is
<model>-<dims>-<dataset>-<docs|queries>.jsonl. run_lume.py can import
load_embedding_cache without importing the Luxir driver.
"""
import json
from pathlib import Path


MODELS = ("embeddinggemma-2", "gtr-t5-base")
KINDS = ("docs", "queries")


def parse_cache_name(path):
    """Return (model, dims, dataset, kind) taken from the cache filename."""
    name = Path(path).name
    if not name.endswith(".jsonl"):
        raise ValueError(f"embedding cache must be a .jsonl file: {name}")
    stem = name[: -len(".jsonl")]
    kind = None
    for candidate in KINDS:
        suffix = "-" + candidate
        if stem.endswith(suffix):
            kind = candidate
            stem = stem[: -len(suffix)]
            break
    if kind is None:
        raise ValueError(f"embedding cache filename must end with -docs or -queries: {name}")
    model = None
    for candidate in MODELS:
        prefix = candidate + "-"
        if stem.startswith(prefix):
            model = candidate
            stem = stem[len(prefix):]
            break
    if model is None:
        raise ValueError(f"embedding cache filename has an unknown model: {name}")
    dims_text, sep, dataset = stem.partition("-")
    if sep == "" or dataset == "" or not dims_text.isdigit():
        raise ValueError(
            f"embedding cache filename must be <model>-<dims>-<dataset>-<docs|queries>.jsonl: {name}"
        )
    return model, int(dims_text), dataset, kind


def load_embedding_cache(path, expected_ids, model=None, dims=None):
    """Load {id: vector} from a JSONL cache.

    Dims and model come from the filename. Every vector must have that many
    components. expected_ids must all be present; a missing id is a partial
    cache and raises ValueError. Pass model or dims to require a specific file.
    """
    path = Path(path)
    parsed_model, parsed_dims, dataset, kind = parse_cache_name(path)
    if model is not None and parsed_model != model:
        raise ValueError(
            f"embedding cache {path.name} model is {parsed_model}, expected {model}"
        )
    if dims is not None and parsed_dims != int(dims):
        raise ValueError(
            f"embedding cache {path.name} dims are {parsed_dims}, expected {int(dims)}"
        )
    if not path.is_file():
        raise ValueError(f"embedding cache not found: {path}")

    found = {}
    with path.open(encoding="utf-8") as source:
        for number, line in enumerate(source, start=1):
            text = line.strip()
            if not text:
                continue
            try:
                row = json.loads(text)
                doc_id = row["id"]
                vector = row["vector"]
            except (json.JSONDecodeError, KeyError, TypeError) as exc:
                raise ValueError(
                    f"embedding cache {path.name} line {number} is not an id/vector object"
                ) from exc
            if not isinstance(doc_id, str) or doc_id == "":
                raise ValueError(f"embedding cache {path.name} line {number} id must be a non-empty string")
            if not isinstance(vector, list) or len(vector) != parsed_dims:
                length = len(vector) if isinstance(vector, list) else "not a list"
                raise ValueError(
                    f"embedding cache {path.name} id {doc_id} vector length {length} "
                    f"!= dims {parsed_dims} from filename"
                )
            if doc_id in found:
                raise ValueError(f"embedding cache {path.name} has a duplicate id {doc_id}")
            found[doc_id] = vector

    expected = [str(doc_id) for doc_id in expected_ids]
    missing = [doc_id for doc_id in expected if doc_id not in found]
    if missing:
        shown = ", ".join(missing[:8])
        if len(missing) > 8:
            shown += f", ... ({len(missing)} total)"
        raise ValueError(
            f"partial embedding cache {path.name} ({parsed_model}, dims {parsed_dims}, "
            f"{dataset} {kind}): missing {len(missing)} of {len(expected)} ids ({shown})"
        )
    return found

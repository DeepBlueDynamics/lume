"""Compare Stage 0b/0.5 stored values, ignoring JSON object key order.

Spelling assigns vocabulary IDs from a HashSet. Its semantic comparison remaps
bitmap IDs and length-array entries back to words; it never drops a field.
state.db_dir is normalized only after checking it equals each supplied index.
"""
import argparse
import gc
import hashlib
import json
from pathlib import Path


def bitmap_ids(bitmap):
    result = []
    for high, container in bitmap["containers"].items():
        base = int(high) << 16
        if set(container) == {"Array"}:
            result.extend(base + value for value in container["Array"])
        elif set(container) == {"Bitmap"}:
            if len(container["Bitmap"]) != 1024:
                raise ValueError("Invalid spelling bitmap length")
            for offset, bits in enumerate(container["Bitmap"]):
                while bits:
                    low = bits & -bits
                    result.append(base + offset * 64 + low.bit_length() - 1)
                    bits ^= low
        else:
            raise ValueError("Unknown spelling container")
    if len(result) != len(set(result)):
        raise ValueError("Duplicate spelling bitmap IDs")
    return result


def spelling_values(value):
    words = value["unique_words"]
    if len(words) != len(set(words)) or len(words) != value["num_words"]:
        raise ValueError("Invalid spelling vocabulary")
    if len(value["word_lens"]) != len(words) or set(value["vocab_set"]) != set(words):
        raise ValueError("Inconsistent spelling vocabulary")
    result = dict(value)
    result["unique_words"] = sorted(words)
    result["vocab_set"] = sorted(value["vocab_set"])
    result["word_lens"] = dict(zip(words, value["word_lens"]))
    result["trigram_postings"] = {
        trigram: sorted(words[index] for index in bitmap_ids(bitmap))
        for trigram, bitmap in value["trigram_postings"].items()
    }
    return result


def canonical_digest(value):
    digest = hashlib.sha256()
    encoder = json.JSONEncoder(sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False)
    for chunk in encoder.iterencode(value):
        digest.update(chunk.encode("utf-8"))
    return digest.hexdigest()


def index_digest(index, name):
    with (index / name).open(encoding="utf-8") as handle:
        value = json.load(handle)
    if name == "state.json":
        if value["db_dir"] != str(index):
            raise ValueError("state.db_dir differs from the supplied index path")
        value["db_dir"] = "<compared-index>"
    elif name == "spelling.json":
        value = spelling_values(value)
    digest = canonical_digest(value)
    del value
    gc.collect()
    return digest


def compare(before, after):
    rows = []
    for name in ("bm25.json", "state.json", "spelling.json"):
        left = index_digest(before, name)
        right = index_digest(after, name)
        rows.append({"file": name, "before_sha256": left, "after_sha256": right,
                     "equal": left == right,
                     "comparison": "word-keyed spelling semantics" if name == "spelling.json"
                     else "canonical JSON values (state.db_dir normalized)"})
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    rows = compare(args.before, args.after)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({"checks": rows}, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(rows, indent=2), flush=True)
    if not all(row["equal"] for row in rows):
        raise SystemExit("Stored-value parity FAILED")


if __name__ == "__main__":
    main()

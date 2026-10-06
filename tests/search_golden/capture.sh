#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

LUME_BIN="${1:-./target/debug/lume}"
MODE="${2:-verify}"

if [[ ! -x "$LUME_BIN" ]]; then
    echo "Error: Binary not found or not executable: $LUME_BIN" >&2
    exit 1
fi

SCRATCH_DB="$SCRIPT_DIR/.scratch_db"
DICT="$SCRIPT_DIR/dict.csv"
CORPUS="docs/monte_cristo"

# Ensure scratch index exists
if [[ ! -f "$SCRATCH_DB/state.json" ]]; then
    echo "==> Indexing corpus into $SCRATCH_DB..."
    "$LUME_BIN" index "$CORPUS" --db "$SCRATCH_DB" --tag-dict "$DICT" -f
fi

# Query definitions: name | args...
QUERIES=(
    "01_lexical|search|captain|--db|$SCRATCH_DB"
    "02_limit|search|treasure|--db|$SCRATCH_DB|-l|3"
    "03_spell_check|search|captan|--db|$SCRATCH_DB|-c"
    "04_graph_boost|search|Dantes Mercedes|--db|$SCRATCH_DB|-g|0.6"
    "05_scoring_jaccard|search|Dantes Mercedes|--db|$SCRATCH_DB|-g|0.6|--scoring|jaccard"
    "06_graph_zero|search|Villefort|--db|$SCRATCH_DB|-g|0"
    "07_no_hit|search|xyznonexistentword12345|--db|$SCRATCH_DB"
    "08_spell_limit|search|Morrel Pharon|--db|$SCRATCH_DB|-c|-l|5"
    "09_graph_default|search|Danglars Fernand|--db|$SCRATCH_DB"
    "10_limit_single|search|Monte Cristo|--db|$SCRATCH_DB|-l|1"
)

# Normalize non-deterministic elapsed microsecond times in BM25 stderr
normalize_stderr() {
    sed -E 's/sections in [0-9.]+.*s/sections in <ELAPSED>/'
}

FAILURES=0

for q in "${QUERIES[@]}"; do
    IFS='|' read -r -a PARTS <<< "$q"
    NAME="${PARTS[0]}"
    ARGS=("${PARTS[@]:1}")

    if [[ "$MODE" == "capture" ]]; then
        echo "Capturing $NAME..."
        "$LUME_BIN" "${ARGS[@]}" > "$SCRIPT_DIR/$NAME.stdout" 2> >(normalize_stderr > "$SCRIPT_DIR/$NAME.stderr") || true
        # Wait a moment for process substitution
        sleep 0.1
    else
        TMP_OUT="$(mktemp)"
        TMP_ERR="$(mktemp)"
        "$LUME_BIN" "${ARGS[@]}" > "$TMP_OUT" 2> >(normalize_stderr > "$TMP_ERR") || true
        sleep 0.1

        if ! diff -u "$SCRIPT_DIR/$NAME.stdout" "$TMP_OUT" > /dev/null; then
            echo "FAIL: $NAME stdout differs!" >&2
            diff -u "$SCRIPT_DIR/$NAME.stdout" "$TMP_OUT" >&2 || true
            FAILURES=$((FAILURES + 1))
        fi

        if ! diff -u "$SCRIPT_DIR/$NAME.stderr" "$TMP_ERR" > /dev/null; then
            echo "FAIL: $NAME stderr differs!" >&2
            diff -u "$SCRIPT_DIR/$NAME.stderr" "$TMP_ERR" >&2 || true
            FAILURES=$((FAILURES + 1))
        fi

        rm -f "$TMP_OUT" "$TMP_ERR"
        if [[ $FAILURES -eq 0 ]]; then
            echo "PASS: $NAME"
        fi
    fi
done

if [[ "$MODE" == "capture" ]]; then
    echo "Golden outputs captured successfully in $SCRIPT_DIR."
elif [[ $FAILURES -eq 0 ]]; then
    echo "All ${#QUERIES[@]} golden queries matched byte-for-byte."
else
    echo "Golden verification failed with $FAILURES differences." >&2
    exit 1
fi

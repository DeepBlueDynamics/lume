#!/usr/bin/env bash
#
# pi_ingest_run.sh
# 
# Sample a live `lume ti ingest` process on a Raspberry Pi every 10s (default: 60 min).
# Collects:
#   - Process RSS (kB) and CPU% (/proc/<pid>/status and /proc/<pid>/stat)
#   - records_ingested, ingest_blocked, apply_failures (<store>/ingest_status.json)
#   - WAL and shard directory sizes (bytes)
#   - CPU temperature (°C) via vcgencmd or /sys/class/thermal
#   - Raspberry Pi throttling bitmask via vcgencmd get_throttled
#
# Generates:
#   - CSV file with timestamped samples
#   - Markdown summary report via summarize_ingest.py
#
# Safety / Guardrails:
#   - Read-only: never restarts services or writes to the store directory.
#   - Auto-discovers PID with bracket trick `pgrep -f '[l]ume ti ingest'`
#

set -euo pipefail

DURATION=3600
INTERVAL=10
PID=""
STORE_DIR=""
OUT_CSV=""
OUT_SUMMARY=""

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  -d, --duration SECONDS   Benchmark duration in seconds (default: 3600 = 60 min)
  -i, --interval SECONDS   Sampling interval in seconds (default: 10)
  -p, --pid PID            PID of lume ti ingest process (default: auto-detect)
  -s, --store PATH         Path to store directory (default: auto-detect from /proc/<pid>/cmdline)
  -o, --out PATH           Path to output CSV file (default: pi_ingest_<timestamp>.csv)
      --summary PATH       Path to output Markdown summary (default: <out>.md)
  -h, --help               Show this help message and exit

Environment Variables:
  TI_STORE_ROOT            Default store directory if --store is omitted
  BENCH_DURATION           Default duration in seconds
  BENCH_INTERVAL           Default interval in seconds
EOF
    exit 0
}

# Parse environment variables
if [ -n "${BENCH_DURATION:-}" ]; then
    DURATION="$BENCH_DURATION"
fi
if [ -n "${BENCH_INTERVAL:-}" ]; then
    INTERVAL="$BENCH_INTERVAL"
fi
if [ -n "${TI_STORE_ROOT:-}" ]; then
    STORE_DIR="$TI_STORE_ROOT"
fi

# Parse CLI arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        -d|--duration)
            DURATION="$2"
            shift 2
            ;;
        -i|--interval)
            INTERVAL="$2"
            shift 2
            ;;
        -p|--pid)
            PID="$2"
            shift 2
            ;;
        -s|--store)
            STORE_DIR="$2"
            shift 2
            ;;
        -o|--out)
            OUT_CSV="$2"
            shift 2
            ;;
        --summary)
            OUT_SUMMARY="$2"
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage
            ;;
    esac
done

# 1. Find PID if not provided
if [ -z "$PID" ]; then
    # Use bracket trick '[l]ume ti ingest' so pgrep does not match itself
    PID=$(pgrep -f '[l]ume ti ingest' | head -n 1 || true)
fi

if [ -z "$PID" ]; then
    echo "Error: No running 'lume ti ingest' process found." >&2
    echo "Please ensure the process is running or supply the PID via --pid <PID>." >&2
    exit 1
fi

if [ ! -d "/proc/$PID" ]; then
    echo "Error: Process with PID $PID (/proc/$PID) does not exist." >&2
    exit 1
fi

# 2. Find store directory if not provided
if [ -z "$STORE_DIR" ]; then
    if [ -r "/proc/$PID/cmdline" ]; then
        # Parse --store parameter from cmdline null-separated tokens
        STORE_DIR=$(tr '\0' '\n' < "/proc/$PID/cmdline" 2>/dev/null | awk '$0=="--store"{getline; print; exit} /^--store=/{sub(/^--store=/, ""); print; exit}' || true)
    fi
fi

if [ -n "$STORE_DIR" ]; then
    echo "Monitored PID: $PID" >&2
    echo "Store Directory: $STORE_DIR" >&2
else
    echo "Monitored PID: $PID" >&2
    echo "Warning: Store directory could not be auto-detected. Ingest counters and WAL/shard sizes may be 0." >&2
fi

# 3. Determine output file paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TIMESTAMP_SUFFIX=$(date +"%Y%m%d_%H%M%S")

if [ -z "$OUT_CSV" ]; then
    OUT_CSV="${SCRIPT_DIR}/pi_ingest_${TIMESTAMP_SUFFIX}.csv"
fi

if [ -z "$OUT_SUMMARY" ]; then
    if [[ "$OUT_CSV" == *.csv ]]; then
        OUT_SUMMARY="${OUT_CSV%.csv}.md"
    else
        OUT_SUMMARY="${OUT_CSV}.md"
    fi
fi

# Guardrail: Never write output inside the store directory (strict read-only on store)
if [ -n "$STORE_DIR" ] && [ -d "$STORE_DIR" ]; then
    ABS_STORE=$(cd "$STORE_DIR" 2>/dev/null && pwd -P || echo "$STORE_DIR")
    OUT_DIR="$(dirname "$OUT_CSV")"
    mkdir -p "$OUT_DIR"
    ABS_OUT_DIR=$(cd "$OUT_DIR" 2>/dev/null && pwd -P || echo "$OUT_DIR")
    if [[ "$ABS_OUT_DIR" == "$ABS_STORE"* ]]; then
        echo "Error: Output path ($OUT_CSV) must not be inside the store directory ($STORE_DIR)." >&2
        echo "The store must remain strictly read-only." >&2
        exit 1
    fi
fi

mkdir -p "$(dirname "$OUT_CSV")"
mkdir -p "$(dirname "$OUT_SUMMARY")"

echo "Output CSV: $OUT_CSV" >&2
echo "Output Summary: $OUT_SUMMARY" >&2
echo "Duration: ${DURATION}s (${DURATION}s / ${INTERVAL}s interval = $(( DURATION / INTERVAL )) samples)" >&2

# 4. Helper functions to read system metrics
CLK_TCK=$(getconf CLK_TCK 2>/dev/null || echo 100)

get_cpu_ticks() {
    local target_pid="$1"
    if [ -r "/proc/$target_pid/stat" ]; then
        local tail
        tail=$(cut -d')' -f2- "/proc/$target_pid/stat" 2>/dev/null || true)
        local utime stime
        utime=$(echo "$tail" | awk '{print $12}')
        stime=$(echo "$tail" | awk '{print $13}')
        if [ -n "$utime" ] && [ -n "$stime" ]; then
            echo $(( utime + stime ))
            return
        fi
    fi
    echo 0
}

get_rss_kb() {
    local target_pid="$1"
    if [ -r "/proc/$target_pid/status" ]; then
        local rss
        rss=$(grep -m 1 '^VmRSS:' "/proc/$target_pid/status" 2>/dev/null | awk '{print $2}' || true)
        if [ -n "$rss" ]; then
            echo "$rss"
            return
        fi
    fi
    echo 0
}

get_status_field() {
    local field="$1"
    local status_file="${STORE_DIR}/ingest_status.json"
    if [ -n "$STORE_DIR" ] && [ -r "$status_file" ]; then
        local val
        val=$(sed -n "s/.*\"${field}\"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p" "$status_file" 2>/dev/null | head -n 1 || true)
        if [ -n "$val" ]; then
            echo "$val"
            return
        fi
    fi
    echo 0
}

get_dir_bytes() {
    local target_dir="$1"
    if [ -d "$target_dir" ]; then
        local b
        b=$(du -sb "$target_dir" 2>/dev/null | awk '{print $1}' || true)
        if [ -n "$b" ]; then
            echo "$b"
            return
        fi
        local kb
        kb=$(du -sk "$target_dir" 2>/dev/null | awk '{print $1}' || true)
        if [ -n "$kb" ]; then
            echo $(( kb * 1024 ))
            return
        fi
    fi
    echo 0
}

get_temperature() {
    if command -v vcgencmd >/dev/null 2>&1; then
        local raw
        raw=$(vcgencmd measure_temp 2>/dev/null || true)
        if [[ "$raw" =~ temp=([0-9.]+) ]]; then
            echo "${BASH_REMATCH[1]}"
            return
        fi
    fi
    if [ -r "/sys/class/thermal/thermal_zone0/temp" ]; then
        local milli
        milli=$(cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null || true)
        if [ -n "$milli" ] && [ "$milli" -gt 0 ] 2>/dev/null; then
            awk "BEGIN {printf \"%.1f\", $milli / 1000.0}"
            return
        fi
    fi
    echo "0.0"
}

get_throttled() {
    if command -v vcgencmd >/dev/null 2>&1; then
        local raw
        raw=$(vcgencmd get_throttled 2>/dev/null || true)
        if [[ "$raw" =~ throttled=(0x[0-9a-fA-F]+|[0-9]+) ]]; then
            echo "${BASH_REMATCH[1]}"
            return
        fi
    fi
    echo "0x0"
}

# 5. Initialize CSV with header
echo "timestamp,elapsed_sec,pid,rss_kb,cpu_pct,records_ingested,rows_per_sec,wal_bytes,shard_bytes,temp_c,ingest_blocked,apply_failures,throttled" > "$OUT_CSV"

INTERRUPTED=0
trap 'INTERRUPTED=1; echo ""; echo "[pi_ingest_run] Signal caught; concluding benchmark and summarizing..." >&2' INT TERM

START_TIME=$(date +%s)
LAST_TIME=$START_TIME
LAST_TICKS=$(get_cpu_ticks "$PID")
LAST_RECORDS=$(get_status_field "records_ingested")
SAMPLE_COUNT=0

echo "[pi_ingest_run] Starting benchmark collection loop..." >&2

while [ "$INTERRUPTED" -eq 0 ]; do
    NOW_TIME=$(date +%s)
    ELAPSED=$(( NOW_TIME - START_TIME ))

    # Check if target process is still alive
    if [ ! -d "/proc/$PID" ]; then
        echo "[pi_ingest_run] Monitored process PID $PID exited. Stopping benchmark." >&2
        break
    fi

    TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
    RSS_KB=$(get_rss_kb "$PID")
    CUR_TICKS=$(get_cpu_ticks "$PID")
    RECORDS=$(get_status_field "records_ingested")
    INGEST_BLOCKED=$(get_status_field "ingest_blocked")
    APPLY_FAILURES=$(get_status_field "apply_failures")
    TEMP_C=$(get_temperature)
    THROTTLED=$(get_throttled)

    WAL_BYTES=0
    SHARD_BYTES=0
    if [ -n "$STORE_DIR" ]; then
        WAL_BYTES=$(get_dir_bytes "${STORE_DIR}/wal")
        SHARD_BYTES=$(get_dir_bytes "${STORE_DIR}/shards")
    fi

    DELTA_SEC=$(( NOW_TIME - LAST_TIME ))
    if [ "$SAMPLE_COUNT" -eq 0 ] || [ "$DELTA_SEC" -le 0 ]; then
        CPU_PCT="0.0"
        ROWS_PER_SEC="0.0"
    else
        DELTA_TICKS=$(( CUR_TICKS - LAST_TICKS ))
        CPU_PCT=$(awk -v dt="$DELTA_TICKS" -v clk="$CLK_TCK" -v ds="$DELTA_SEC" 'BEGIN {
            if (ds > 0 && clk > 0) printf "%.1f", ((dt / clk) / ds) * 100.0;
            else printf "0.0";
        }')
        DELTA_RECORDS=$(( RECORDS - LAST_RECORDS ))
        ROWS_PER_SEC=$(awk -v dr="$DELTA_RECORDS" -v ds="$DELTA_SEC" 'BEGIN {
            if (ds > 0) printf "%.1f", dr / ds;
            else printf "0.0";
        }')
    fi

    # Append to CSV
    echo "${TIMESTAMP},${ELAPSED},${PID},${RSS_KB},${CPU_PCT},${RECORDS},${ROWS_PER_SEC},${WAL_BYTES},${SHARD_BYTES},${TEMP_C},${INGEST_BLOCKED},${APPLY_FAILURES},${THROTTLED}" >> "$OUT_CSV"

    # Status log line to stderr
    RSS_MIB=$(awk -v r="$RSS_KB" 'BEGIN {printf "%.1f", r / 1024.0}')
    printf "[+%5ds / %5ds] PID %s | RSS: %6s MiB | CPU: %5s%% | Ingest: %8d (+%6s r/s) | Temp: %4s°C | Throttle: %s\n" \
        "$ELAPSED" "$DURATION" "$PID" "$RSS_MIB" "$CPU_PCT" "$RECORDS" "$ROWS_PER_SEC" "$TEMP_C" "$THROTTLED" >&2

    LAST_TIME=$NOW_TIME
    LAST_TICKS=$CUR_TICKS
    LAST_RECORDS=$RECORDS
    SAMPLE_COUNT=$(( SAMPLE_COUNT + 1 ))

    if [ "$ELAPSED" -ge "$DURATION" ]; then
        echo "[pi_ingest_run] Duration target reached (${DURATION}s). Benchmark complete." >&2
        break
    fi

    # Sleep in small increments to be responsive to interrupt signals
    SLEEP_TARGET=$(( NOW_TIME + INTERVAL ))
    while [ "$INTERRUPTED" -eq 0 ]; do
        CURR=$(date +%s)
        REMAIN=$(( SLEEP_TARGET - CURR ))
        if [ "$REMAIN" -le 0 ]; then
            break
        fi
        sleep 1
    done
done

echo "[pi_ingest_run] Benchmark collection finished (${SAMPLE_COUNT} samples recorded)." >&2

# 6. Run Python summarizer
SUMMARIZER="${SCRIPT_DIR}/summarize_ingest.py"
if [ -f "$SUMMARIZER" ]; then
    echo "[pi_ingest_run] Running summarizer: python3 $SUMMARIZER $OUT_CSV --output $OUT_SUMMARY" >&2
    python3 "$SUMMARIZER" "$OUT_CSV" --output "$OUT_SUMMARY"
else
    echo "Warning: Summarizer not found at $SUMMARIZER" >&2
fi

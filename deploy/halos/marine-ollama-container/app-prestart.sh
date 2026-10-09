#!/bin/bash
# Ollama app-prestart hook (sourced by the framework prestart, which defines
# RUNTIME_ENV and has loaded env.defaults and env).
#
# OLLAMA_MEMORY_LIMIT=auto sizes the container's memory cap from this machine's RAM:
# 12 % of MemTotal, at least 768 MiB and at most 16 GiB. On an 8 GB Pi 5 that is about
# 1 GiB, enough for the cloud-model gateway. On a 64 GB shore machine it is about
# 7.7 GiB, enough to run local models too. An explicit value such as 4g is left alone.
# runtime.env loads last, so the computed value is the one docker compose sees.
if [ "${OLLAMA_MEMORY_LIMIT:-auto}" = "auto" ]; then
    total_mb=$(awk '/^MemTotal:/ {print int($2 / 1024)}' /proc/meminfo)
    limit_mb=$(( total_mb * 12 / 100 ))
    [ "$limit_mb" -lt 768 ] && limit_mb=768
    [ "$limit_mb" -gt 16384 ] && limit_mb=16384
    echo "OLLAMA_MEMORY_LIMIT=${limit_mb}m" >> "$RUNTIME_ENV"
    echo "Ollama memory limit: ${limit_mb} MiB (auto, ${total_mb} MiB RAM)"
fi

#!/bin/bash
# Grub Crawler app-prestart hook (sourced by the framework prestart, which defines
# RUNTIME_ENV and has loaded env.defaults and env).
#
# GRUB_MEMORY_LIMIT=auto sizes the container's memory cap from this machine's RAM:
# 20 % of MemTotal, at least 1 GiB and at most 4 GiB (1.6 GiB on an 8 GB Pi 5).
# An explicit value such as 2g is left alone. runtime.env loads last, so the
# computed value is the one docker compose sees.
if [ "${GRUB_MEMORY_LIMIT:-auto}" = "auto" ]; then
    total_mb=$(awk '/^MemTotal:/ {print int($2 / 1024)}' /proc/meminfo)
    limit_mb=$(( total_mb * 20 / 100 ))
    [ "$limit_mb" -lt 1024 ] && limit_mb=1024
    [ "$limit_mb" -gt 4096 ] && limit_mb=4096
    echo "GRUB_MEMORY_LIMIT=${limit_mb}m" >> "$RUNTIME_ENV"
    echo "Grub memory limit: ${limit_mb} MiB (auto, ${total_mb} MiB RAM)"
fi

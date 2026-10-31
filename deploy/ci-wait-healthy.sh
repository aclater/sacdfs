#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Waits (up to 120 s) until a container's health check passes; prints its log on failure.
set -u
name=$1
for _ in $(seq 120); do
    state=$(docker inspect -f '{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}' "$name" 2>/dev/null)
    case "$state" in
        "running healthy") echo "$name: healthy"; exit 0 ;;
        running*) ;;
        *) break ;;
    esac
    # Don't wait out the 30 s interval: run the check directly.
    if docker exec "$name" /usr/local/bin/healthcheck.sh >/dev/null 2>&1; then
        echo "$name: health check passes"; exit 0
    fi
    sleep 1
done
echo "$name did not become healthy (state: $state)"
docker logs "$name" 2>&1 | tail -n 100
exit 1

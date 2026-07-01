#!/usr/bin/env bash
# Sweep a tuning knob across fresh kompress containers and load-test each.
#
# For every value, this starts a container with the env override, waits for
# /health, runs loadtest.py against it, then tears it down — so you can compare
# KOMPRESS_MAX_BATCH (or ONNX thread counts) head-to-head on identical load.
#
# Usage:
#   ./sweep.sh                                   # sweep KOMPRESS_MAX_BATCH=1,8,32,128
#   IMAGE=... KNOB=OMP_NUM_THREADS VALUES="1 2 4 8" ./sweep.sh
#
# Env:
#   IMAGE   container image (default: ghcr.io/thediymaker/obleth-gateway/obleth-kompress:latest)
#   KNOB    env var to sweep (default: KOMPRESS_MAX_BATCH)
#   VALUES  space-separated values (default: "1 8 32 128")
#   PORT    host port (default: 8899)
#   ARGS    extra args forwarded to loadtest.py (default: "--concurrency 1,4,8 --requests 100 --sentences 40")
set -euo pipefail

IMAGE="${IMAGE:-ghcr.io/thediymaker/obleth-gateway/obleth-kompress:latest}"
KNOB="${KNOB:-KOMPRESS_MAX_BATCH}"
VALUES="${VALUES:-1 8 32 128}"
PORT="${PORT:-8899}"
ARGS="${ARGS:---concurrency 1,4,8 --requests 100 --sentences 40}"
NAME="kompress-sweep"
HERE="$(cd "$(dirname "$0")" && pwd)"

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

for v in $VALUES; do
  cleanup
  echo "========================================================"
  echo ">>> $KNOB=$v"
  echo "========================================================"
  docker run -d --name "$NAME" -p "${PORT}:8080" -e "${KNOB}=${v}" "$IMAGE" >/dev/null
  # Wait for health (up to 60s).
  for _ in $(seq 1 30); do
    sleep 2
    curl -fsS "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1 && break
  done
  python "${HERE}/loadtest.py" --url "http://127.0.0.1:${PORT}" --docker-name "$NAME" $ARGS
  echo
done

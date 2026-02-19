#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   ./scripts/benchmark.sh
#
# Runs:
#   - reth1, reth2, reth3
#   - explorer
#   - orion-bench1 only
#
# Does not run:
#   - orion-bench2
#   - orion-bench3

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# Hardcoded benchmark settings
NUM_BLOCKS="60"
TXS_PER_BLOCK="100"
BLOCK_DELAY_MS="50"
MAX_BLOCK_BUILD_RETRIES="2"
BLOCK_BUILD_RETRY_DELAY_MS="50"
BENCH_DURATION_SECONDS="60"

echo "Stopping extra benchmark producers..."
docker compose stop orion-bench2 orion-bench3 >/dev/null 2>&1 || true
docker compose rm -f orion-bench2 orion-bench3 >/dev/null 2>&1 || true

echo "Starting three reth nodes + single bench producer..."
NUM_BLOCKS="$NUM_BLOCKS" TXS_PER_BLOCK="$TXS_PER_BLOCK" \
BLOCK_DELAY_MS="$BLOCK_DELAY_MS" \
MAX_BLOCK_BUILD_RETRIES="$MAX_BLOCK_BUILD_RETRIES" \
BLOCK_BUILD_RETRY_DELAY_MS="$BLOCK_BUILD_RETRY_DELAY_MS" \
BENCH1_COMMITTEE_SIZE=1 BENCH1_PEER_ADDRS="" \
docker compose up -d init reth1 reth2 reth3 explorer orion-bench1

echo
echo "Cluster started:"
echo "  Explorer:        http://localhost:8080"
echo "  Bench1 metrics:  http://localhost:9090/metrics"
echo "  Reth RPC:        8545 / 8645 / 8745"
echo "  Load:            NUM_BLOCKS=$NUM_BLOCKS, TXS_PER_BLOCK=$TXS_PER_BLOCK"
echo "  Pacing:          BLOCK_DELAY_MS=$BLOCK_DELAY_MS"
echo "  Retries:         MAX_BLOCK_BUILD_RETRIES=$MAX_BLOCK_BUILD_RETRIES, BLOCK_BUILD_RETRY_DELAY_MS=$BLOCK_BUILD_RETRY_DELAY_MS"
echo "  Auto-stop:       ${BENCH_DURATION_SECONDS}s"
echo
docker compose ps


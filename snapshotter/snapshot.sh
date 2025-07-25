#!/bin/bash
set -euo pipefail

if [ -z "${SNAPSHOT_INTERVAL:-}" ]; then
  echo "[ERROR] SNAPSHOT_INTERVAL environment variable is not set."
  exit 1
fi

if [ -z "${BUCKET:-}" ]; then
  echo "[ERROR] BUCKET environment variable is not set."
  exit 1
fi

if [ -z "${EXTERNAL_RPC:-}" ]; then
  echo "[ERROR] EXTERNAL_RPC environment variable is not set."
  exit 1
fi

AWS_REGION="${AWS_REGION:=eu-central-2}"
AWS_MAX_CONCURRENT_REQUESTS=32
export AWS_REGION
export AWS_MAX_CONCURRENT_REQUESTS

RPC_PORT="${RPC_PORT:=8545}"
DATA_DIR="${DATA_DIR:=/data}"

MAIN_BIN="$1"
shift
MAIN_ARGS=("$@")

start_main_bin() {
  echo "[INFO] Starting: $MAIN_BIN ${MAIN_ARGS[*]}"
  set -m
  "$MAIN_BIN" "${MAIN_ARGS[@]}" & # child in its own PG
  MAIN_PID=$!
  PGID=$MAIN_PID
}

stop_main_bin() {
  echo "[INFO] Stopping PID $MAIN_PID"
  kill -- -"$PGID" 2>/dev/null || true
  wait # waits for ALL still-running children
}

get_block_number() {
  local rpc_url="$1"
  local hex_block=$(curl -s -X POST "$rpc_url" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq -r .result)

  # Fallback if the result is null or jq fails
  if [[ "$hex_block" == "null" || -z "$hex_block" ]]; then
    echo "Error: Could not retrieve block number" >&2
    return 2
  fi

  # Convert hex to decimal
  echo $((hex_block))
}

is_synced() {
  echo "[INFO] Checking sync status..."

  LOCAL_RPC=http://localhost:$RPC_PORT
  LOCAL_BLOCK=$(get_block_number "$LOCAL_RPC")
  EXTERNAL_BLOCK=$(get_block_number "$EXTERNAL_RPC")

  echo "[INFO] Local block number: $LOCAL_BLOCK"
  echo "[INFO] External block number: $EXTERNAL_BLOCK"

  DELTA=$((EXTERNAL_BLOCK - LOCAL_BLOCK))

  if [ "$DELTA" -lt 5 ]; then
    echo "[INFO] Node is synced."
    return 0
  else
    echo "[INFO] Node is behind by $DELTA blocks. Skipping snapshot."
    return 1
  fi
}

take_snapshot() {
  stop_main_bin

  S3_URL="s3://$BUCKET/reth.tar.lz4"
  SIZE=$(du -sb "$DATA_DIR/reth" | awk '{print $1}')

  echo "[INFO] Compressing and uploading to S3…"
  tar -C "$DATA_DIR" -cf - reth | \
    lz4 -v -1 -c - | \
    aws s3 cp - "$S3_URL.tmp" \
      --region "$AWS_REGION" \
      --expected-size "$SIZE"

  echo "[INFO] Finalising snapshot"
  aws s3 cp "$S3_URL.tmp" "$S3_URL" --region "$AWS_REGION"
  aws s3 rm "$S3_URL.tmp"          --region "$AWS_REGION"
  echo "[INFO] Snapshot completed"

  start_main_bin
}

wait_any() {
  while :; do
    kill -0 "$1" 2>/dev/null || return 0   # $1 has exited
    kill -0 "$2" 2>/dev/null || return 0   # $2 has exited
    sleep 0.5
  done
}

# Main loop
start_main_bin
trap 'echo "[INFO] Caught signal, stopping..."; stop_main_bin; exit 0' SIGTERM SIGINT
while true; do
  echo "[INFO] Waiting $SNAPSHOT_INTERVAL seconds before snapshot..."
  sleep "$SNAPSHOT_INTERVAL" & SLEEP_PID=$!

  # wait -n "$MAIN_PID" "$SLEEP_PID"
  wait_any "$MAIN_PID" "$SLEEP_PID"
  EXIT_CODE=$?

  if ! kill -0 "$MAIN_PID" 2>/dev/null; then
    echo "[ERROR] Main binary exited unexpectedly with code $EXIT_CODE"
    exit $EXIT_CODE
  fi

  if ! is_synced; then
    continue
  fi

  take_snapshot
done


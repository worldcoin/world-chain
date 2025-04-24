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

AWS_REGION="${AWS_REGION:=eu-central-2}"
export AWS_REGION

RPC_PORT="${RPC_PORT:=8545}"
DATA_DIR="${DATA_DIR:=/data}"

MAIN_BIN="$1"
shift
MAIN_ARGS=("$@")

start_main_bin() {
  echo "[INFO] Starting: $MAIN_BIN ${MAIN_ARGS[*]}"
  "$MAIN_BIN" "${MAIN_ARGS[@]}" &
  MAIN_PID=$!
}

stop_main_bin() {
  echo "[INFO] Stopping PID $MAIN_PID"
  kill "$MAIN_PID" 2>/dev/null || true
  wait "$MAIN_PID" || true
}

is_synced() {
  local result
  result=$(curl -s --max-time 5 -X POST http://localhost:$RPC_PORT \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","id":1,"method":"eth_syncing","params":[]}' || true)

  echo -e "\n\n[INFO] Sync status: $result"

  if echo "$result" | grep -q '"result":false'; then
    echo "[INFO] Node is synced."
    return 0
  else
    echo "[INFO] Node is not synced."
    return 1
  fi
}

take_snapshot() {
  stop_main_bin
  S3_URL="s3://$BUCKET/mdbx.tar.lz4"

  echo "[INFO] Copying DB file"
  mkdir -p "$DATA_DIR/snapshots"
  cp "$DATA_DIR/db/mdbx.dat" "$DATA_DIR/snapshots/mdbx-copy.dat"

  start_main_bin

  echo "[INFO] Compressing and uploading to S3..."
  tar --use-compress-program=lz4 -cvf - $DATA_DIR/snapshots/mdbx-copy.dat | \
    aws s3 cp - "$S3_URL.tmp" --region "$AWS_REGION"
  aws s3 cp "$S3_URL.tmp" "$S3_URL" --region "$AWS_REGION"
  aws s3 rm "$S3_URL.tmp" --region "$AWS_REGION"

  echo "[INFO] Snapshot completed and uploaded."
}

# Main loop
start_main_bin
trap 'echo "[INFO] Caught signal, stopping..."; kill "$MAIN_PID" 2>/dev/null || true; exit 0' SIGTERM SIGINT
while true; do
  echo "[INFO] Waiting $SNAPSHOT_INTERVAL seconds before snapshot..."
  sleep "$SNAPSHOT_INTERVAL" & SLEEP_PID=$!

  wait -n "$MAIN_PID" "$SLEEP_PID"
  EXIT_CODE=$?

  if ! kill -0 "$MAIN_PID" 2>/dev/null; then
    echo "[ERROR] Main binary exited unexpectedly with code $EXIT_CODE"
    exit $EXIT_CODE
  fi

  echo "[INFO] Checking sync status..."
  if ! is_synced; then
    echo "[INFO] Node is still syncing. Skipping snapshot."
    continue
  fi

  take_snapshot
done


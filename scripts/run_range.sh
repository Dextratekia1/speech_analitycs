#!/usr/bin/env bash
set -euo pipefail

START="${START:-2026-05-11}"
END="${END:-2026-05-11}"

CLIENTS=("maf" "natura")
#CLIENTS=("natura")
# match = fetcher + converter + matcher
# full  = pipeline completo
MODE="${MODE:-full}"

# DEV corporativo: SIEMPRE en la netns persistente
NET_MODE="${NET_MODE:-container:work-netns}"

# Secrets (podman secrets)
MSSQL_SECRET="${MSSQL_SECRET:-mssql-env-v2}"
SFTP_SECRET="${SFTP_SECRET:-sftp-env}"
MSSQL_ENV_FILE="${MSSQL_ENV_FILE:-/run/secrets/mssql-env-v2}"

# Images
IMG_FETCHER="localhost/audios-natura/audio-fetcher-rs:dev"
IMG_CONVERTER="localhost/audios-natura/audio-converter-rs:dev"
IMG_MATCHER="localhost/audios-natura/metadata-matcher-rs:dev"
IMG_UPLOADER="localhost/audios-natura/audio-uploader-go:dev"
IMG_PIPELINE="localhost/audios-natura/pipeline-runner:dev"

mkdir -p logs shared

VOL_SHARED="$(pwd)/shared:/shared:Z"

if ! podman inspect -f '{{.State.Running}}' work-netns 2>/dev/null | grep -qx true; then
  echo "ERROR: work-netns no está corriendo" >&2
  exit 1
fi

echo "START=$START END=$END MODE=$MODE NET_MODE=$NET_MODE"

if [[ "${BUILD:-0}" == "1" ]]; then
  podman build -t "$IMG_FETCHER"   -f audio-fetcher-rs/Containerfile .
  podman build -t "$IMG_CONVERTER" -f audio-converter-rs/Containerfile .
  podman build -t "$IMG_MATCHER"   -f metadata-matcher-rs/Containerfile .
  podman build -t "$IMG_UPLOADER"  -f audio-uploader-go/Containerfile .
  podman build -t "$IMG_PIPELINE"  -f pipeline-runner/Containerfile .
fi

run_fetcher() {
  local client="$1" d="$2" run_id="$3"
  podman run --rm \
    --network "$NET_MODE" \
    -v "$VOL_SHARED" \
    -e RUST_LOG=info \
    "$IMG_FETCHER" --client "$client" --date "$d" --run-id "$run_id"
}

run_converter() {
  local client="$1" d="$2" run_id="$3"
  podman run --rm \
    --network "$NET_MODE" \
    -v "$VOL_SHARED" \
    -e RUST_LOG=info \
    "$IMG_CONVERTER" --client "$client" --date "$d" --run-id "$run_id"
}

run_matcher() {
  local client="$1" d="$2" run_id="$3"
  podman run --rm \
    --network "$NET_MODE" \
    -v "$VOL_SHARED" \
    --secret "${MSSQL_SECRET},type=mount" \
    -e RUST_LOG=info \
    "$IMG_MATCHER" --client "$client" --date "$d" --run-id "$run_id" \
      --mssql-env-file "$MSSQL_ENV_FILE"
}

run_pipeline() {
  local client="$1" d="$2" run_id="$3"
  podman run --rm \
    --network "$NET_MODE" \
    -v "$VOL_SHARED" \
    --secret "${MSSQL_SECRET},type=mount" \
    --secret "${SFTP_SECRET},type=mount" \
    -e RUST_LOG=info \
    "$IMG_PIPELINE" --client "$client" --date "$d" --run-id "$run_id"
}

for client in "${CLIENTS[@]}"; do
  d="$START"
  while [[ "$d" < "$END" || "$d" == "$END" ]]; do
    run_id="bulk_${client}_$(date -d "$d" +%Y%m%d)"
    run_dir="shared/runs/$client/$d/$run_id"
    log="logs/${client}_${d}.log"

    if [[ -d "$run_dir" ]]; then
      echo "SKIP $client $d (exists: $run_dir)"
      d=$(date -d "$d +1 day" +%F)
      continue
    fi

    echo "RUN  $client $d run_id=$run_id" | tee -a "$log"

    if [[ "$MODE" == "match" ]]; then
      run_fetcher   "$client" "$d" "$run_id" |& tee -a "$log"
      run_converter "$client" "$d" "$run_id" |& tee -a "$log"
      run_matcher   "$client" "$d" "$run_id" |& tee -a "$log"
    else
      run_pipeline  "$client" "$d" "$run_id" |& tee -a "$log"
    fi

    d=$(date -d "$d +1 day" +%F)
  done
done

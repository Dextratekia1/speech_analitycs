#!/usr/bin/env bash
# DEPRECATED: usar scripts/run_range.sh para ejecuciones operativas.
# run_range.sh usa container:work-netns e incluye los preflight checks necesarios.
# Este script se conserva solo como referencia histórica.
set -euo pipefail

START="2026-02-09"
END="2026-02-09"
CLIENTS=("natura" "maf")
MODE="${MODE:-full}"
MSSQL_ENV_FILE="${MSSQL_ENV_FILE:-/run/secrets/mssql-env-v2}"

mkdir -p logs

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
    
    echo "RUN  $client $d  run_id=$run_id" | tee -a "$log"
    
    if [[ "$MODE" == "match" ]]; then
      podman run --rm --pull=never --network=container:work-netns \
        -v ./shared:/shared:Z -e RUST_LOG=info \
        localhost/audios-natura/audio-fetcher-rs:dev \
        --client "$client" --date "$d" --run-id "$run_id" |& tee -a "$log"
      
      podman run --rm --pull=never --network=container:work-netns \
        -v ./shared:/shared:Z -e RUST_LOG=info \
        localhost/audios-natura/audio-converter-rs:dev \
        --client "$client" --date "$d" --run-id "$run_id" |& tee -a "$log"
      
      podman run --rm --pull=never --network=container:work-netns \
        -v ./shared:/shared:Z -e RUST_LOG=info \
        --secret mssql-env-v2 \
        localhost/audios-natura/metadata-matcher-rs:dev \
        --client "$client" --date "$d" --run-id "$run_id" \
        --mssql-env-file "$MSSQL_ENV_FILE" |& tee -a "$log"
    else
      podman run --rm --pull=never --network=container:work-netns \
        -v ./shared:/shared:Z -e RUST_LOG=info \
        --secret mssql-env-v2 --secret sftp-env \
        localhost/audios-natura/pipeline-runner:dev \
        --client "$client" --date "$d" --run-id "$run_id" |& tee -a "$log"
    fi
    
    d=$(date -d "$d +1 day" +%F)
  done
done

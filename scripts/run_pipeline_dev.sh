#!/usr/bin/env bash
set -euo pipefail
CLIENT="${1:-natura}"
DATE="${2:-$(date +%F)}"
DRY="${3:---dry-run}"
sudo podman-compose -f podman-compose.yml -f podman-compose.override.yml \
  run --rm pipeline-runner --client "$CLIENT" --date "$DATE" $DRY

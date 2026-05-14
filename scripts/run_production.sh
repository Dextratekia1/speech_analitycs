#!/usr/bin/env bash
# Production daily pipeline wrapper for Fedora CoreOS.
# Invoked by audios-natura-pipeline.service at 22:00.
# Locks: --sftp-mode real, --client all, date = today.
# Production server is on the company network directly; no work-netns needed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Production image: :release (not :dev).
# Override with PIPELINE_IMG env var for emergency rollback.
export PIPELINE_IMG="${PIPELINE_IMG:-localhost/audios-natura/pipeline-runner:release}"

# No explicit --network flag: production server is already on the company network.
# Set to work-netns only for dev/test runs through this wrapper.
export PIPELINE_NETWORK_MODE="${PIPELINE_NETWORK_MODE:-default}"

RUN_DATE="$(date +%F)"

echo "=== run_production.sh ==="
echo "  date        : $RUN_DATE"
echo "  image       : $PIPELINE_IMG"
echo "  network     : $PIPELINE_NETWORK_MODE"
echo "  client      : all"
echo "  sftp-mode   : real"
echo ""

# Optional emergency run label (e.g.: RUN_LABEL=retry1 run_production.sh).
# Validated by run_pipeline.sh; must match ^[A-Za-z0-9_-]{1,32}$.
LABEL_ARGS=()
if [[ -n "${RUN_LABEL:-}" ]]; then
    LABEL_ARGS=(--run-label "$RUN_LABEL")
fi

exec "$SCRIPT_DIR/run_pipeline.sh" \
    --client all \
    --date "$RUN_DATE" \
    --sftp-mode real \
    "${LABEL_ARGS[@]+"${LABEL_ARGS[@]}"}"

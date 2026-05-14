#!/usr/bin/env bash
# scripts/start_test_sftp.sh — starts the local test SFTP server for test-mode
# pipeline validation. Outputs only the generated synthetic env file path to
# stdout. All human-readable status messages go to stderr.
#
# Usage: scripts/start_test_sftp.sh [--build]
#   --build   Force rebuild of the test-sftp-server image before starting.
#
# Example:
#   TEST_ENV=$(bash scripts/start_test_sftp.sh --build)
#   scripts/run_pipeline.sh --sftp-mode test --test-sftp-env "$TEST_ENV" \
#     --client all --date 2026-05-14
#   bash scripts/stop_test_sftp.sh --cleanup-env "$TEST_ENV"
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

IMG="localhost/audios-natura/test-sftp-server:dev"
CONTAINER_NAME="audios-test-sftp"
BUILD=0
MAX_WAIT=10

while [[ $# -gt 0 ]]; do
    case "$1" in
        --build) BUILD=1; shift ;;
        *) echo "ERROR: Unknown option: $1. Usage: start_test_sftp.sh [--build]" >&2; exit 1 ;;
    esac
done

# The test SFTP server shares the work-netns network namespace so that
# pipeline-runner containers can reach 127.0.0.1:2222 within that namespace.
if ! podman inspect -f '{{.State.Running}}' work-netns 2>/dev/null | grep -qx true; then
    echo "ERROR: work-netns is not running." >&2
    echo "       The test SFTP server uses --network container:work-netns." >&2
    echo "       Start work-netns first, then re-run this script." >&2
    exit 1
fi

# Build the image if missing or if --build was requested.
if [[ "$BUILD" -eq 1 ]] || ! podman image exists "$IMG" 2>/dev/null; then
    echo "Building test-sftp-server image..." >&2
    podman build -t "$IMG" -f Containerfile.test-sftp-server . >&2
    echo "Build complete." >&2
fi

# Remove any stale container from a previous run.
if podman container exists "$CONTAINER_NAME" 2>/dev/null; then
    echo "Removing stale container: $CONTAINER_NAME" >&2
    podman rm -f "$CONTAINER_NAME" >/dev/null
fi

# Create a temporary setup directory on the host to receive the env file.
SETUP_DIR=$(mktemp -d /tmp/audios-test-sftp-XXXXXX)
echo "Setup dir: $SETUP_DIR" >&2

# Start the test SFTP server. It binds to 127.0.0.1:2222 within work-netns.
# Pipeline containers also use --network container:work-netns, so they can
# reach the server at 127.0.0.1:2222 through the shared namespace.
echo "Starting $CONTAINER_NAME..." >&2
podman run --detach \
    --name "$CONTAINER_NAME" \
    --network container:work-netns \
    -v "${SETUP_DIR}:/run/sftp-setup:Z" \
    "$IMG" >/dev/null

# Wait for the server to write the env file (up to MAX_WAIT seconds).
ENV_FILE="${SETUP_DIR}/sftp.env"
ELAPSED=0
while [[ ! -f "$ENV_FILE" && "$ELAPSED" -lt "$MAX_WAIT" ]]; do
    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

if [[ ! -f "$ENV_FILE" ]]; then
    echo "ERROR: env file not created after ${MAX_WAIT}s." >&2
    echo "Container logs:" >&2
    podman logs "$CONTAINER_NAME" >&2
    podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
    rm -rf "$SETUP_DIR"
    exit 1
fi

# Verify all required keys are present in the env file.
for key in SFTP_HOST SFTP_PORT SFTP_USER SFTP_PASSWORD SFTP_REMOTE_BASE SFTP_HOST_KEY; do
    if ! grep -q "^${key}=" "$ENV_FILE" 2>/dev/null; then
        echo "ERROR: Required key $key not found in env file." >&2
        podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
        rm -rf "$SETUP_DIR"
        exit 1
    fi
done

echo "Test SFTP server running: container=$CONTAINER_NAME" >&2
echo "Env file ready (path only — contents are synthetic credentials, not printed)." >&2

# Print only the env file path to stdout. The caller uses this with
# --test-sftp-env. Contents are never printed.
echo "$ENV_FILE"

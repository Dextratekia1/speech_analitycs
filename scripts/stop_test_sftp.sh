#!/usr/bin/env bash
# scripts/stop_test_sftp.sh — stops and removes the test SFTP server container.
# Idempotent if the container does not exist.
#
# Usage: scripts/stop_test_sftp.sh [--cleanup-env <path>]
#   --cleanup-env <path>   Remove the temporary setup directory created by
#                          start_test_sftp.sh. Only paths matching
#                          /tmp/audios-test-sftp-* are accepted.
set -euo pipefail

CONTAINER_NAME="audios-test-sftp"
CLEANUP_PATH=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --cleanup-env) CLEANUP_PATH="$2"; shift 2 ;;
        *) echo "ERROR: Unknown option: $1" >&2; exit 1 ;;
    esac
done

if podman container exists "$CONTAINER_NAME" 2>/dev/null; then
    podman stop "$CONTAINER_NAME" >/dev/null
    podman rm "$CONTAINER_NAME" >/dev/null
    echo "Container $CONTAINER_NAME stopped and removed."
else
    echo "Container $CONTAINER_NAME not found (already stopped or never started)."
fi

# Optional: remove the temporary setup directory.
# Accepts either the env file path (e.g. /tmp/audios-test-sftp-XXXX/sftp.env) or the
# directory itself. In both cases the parent /tmp/audios-test-sftp-* dir is removed.
# Restricted to paths under /tmp/audios-test-sftp-* to avoid accidental deletion.
if [[ -n "$CLEANUP_PATH" ]]; then
    # Resolve to directory: if given a file path, use its parent.
    if [[ -f "$CLEANUP_PATH" ]]; then
        CLEANUP_DIR="$(dirname "$CLEANUP_PATH")"
    else
        CLEANUP_DIR="$CLEANUP_PATH"
    fi
    case "$CLEANUP_DIR" in
        /tmp/audios-test-sftp-*)
            if [[ -d "$CLEANUP_DIR" ]]; then
                rm -rf "$CLEANUP_DIR"
                echo "Cleaned up setup dir: $CLEANUP_DIR"
            else
                echo "Setup dir not found (already removed): $CLEANUP_DIR"
            fi
            ;;
        *)
            echo "WARNING: --cleanup-env path is outside /tmp/audios-test-sftp-* — ignoring: $CLEANUP_PATH" >&2
            ;;
    esac
fi

#!/usr/bin/env bash
# Retention cleanup for audios-natura production artifacts.
# Deletes run directories older than RUNS_RETAIN_DAYS (default: 30).
# Deletes log files older than LOGS_RETAIN_DAYS (default: 60).
# Never touches secrets, config files, or today's run directories.
# Run weekly via audios-natura-cleanup.service + timer.
# Usage: audios-natura-cleanup.sh [--dry-run]
set -euo pipefail

AUDIOS_NATURA_ROOT="${AUDIOS_NATURA_ROOT:-/opt/audios-natura}"
RUNS_RETAIN_DAYS=30
LOGS_RETAIN_DAYS=60
DRY_RUN=0

for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        *) echo "ERROR: Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

# ---- Validate root path before any deletion ----
[[ -n "$AUDIOS_NATURA_ROOT" ]] \
    || { echo "ERROR: AUDIOS_NATURA_ROOT is empty" >&2; exit 1; }
[[ "$AUDIOS_NATURA_ROOT" == /* ]] \
    || { echo "ERROR: AUDIOS_NATURA_ROOT must be an absolute path; got: '$AUDIOS_NATURA_ROOT'" >&2; exit 1; }
[[ "$AUDIOS_NATURA_ROOT" != "/" ]] \
    || { echo "ERROR: AUDIOS_NATURA_ROOT must not be /" >&2; exit 1; }
[[ "$AUDIOS_NATURA_ROOT" != "/opt" ]] \
    || { echo "ERROR: AUDIOS_NATURA_ROOT must not be /opt" >&2; exit 1; }

RUNS_DIR="$AUDIOS_NATURA_ROOT/shared/runs"
LOGS_DIR="$AUDIOS_NATURA_ROOT/logs"
TODAY="$(date +%F)"

echo "=== audios-natura-cleanup ==="
echo "  root        : $AUDIOS_NATURA_ROOT"
echo "  retain runs : $RUNS_RETAIN_DAYS days"
echo "  retain logs : $LOGS_RETAIN_DAYS days"
echo "  today       : $TODAY"
[[ "$DRY_RUN" -eq 1 ]] && echo "  mode        : DRY RUN (no deletions)"
echo ""

runs_deleted=0
logs_deleted=0

# ---- Cleanup run directories ----
# Layout: $RUNS_DIR/<client>/<date>/<run_id>/
# Delete run_id directories (depth 3) older than RUNS_RETAIN_DAYS.
# Filenames are not printed to avoid PII exposure (log filenames embed phone numbers).
if [[ -d "$RUNS_DIR" ]]; then
    while IFS= read -r -d '' run_dir; do
        # Safety: skip anything whose absolute path contains today's date string.
        if [[ "$run_dir" == *"/$TODAY/"* || "$run_dir" == *"/$TODAY" ]]; then
            continue
        fi
        runs_deleted=$((runs_deleted + 1))
        if [[ "$DRY_RUN" -eq 0 ]]; then
            rm -rf -- "$run_dir"
        fi
    done < <(
        find "$RUNS_DIR" \
            -mindepth 3 -maxdepth 3 \
            -type d \
            -mtime "+${RUNS_RETAIN_DAYS}" \
            -print0 2>/dev/null
    )
fi

# ---- Cleanup log files ----
# Filenames not printed: they may embed phone numbers (PII).
if [[ -d "$LOGS_DIR" ]]; then
    while IFS= read -r -d '' log_file; do
        logs_deleted=$((logs_deleted + 1))
        if [[ "$DRY_RUN" -eq 0 ]]; then
            rm -f -- "$log_file"
        fi
    done < <(
        find "$LOGS_DIR" \
            -maxdepth 1 \
            -type f \
            -name '*.log' \
            -mtime "+${LOGS_RETAIN_DAYS}" \
            -print0 2>/dev/null
    )
fi

echo "  run dirs purged : $runs_deleted"
echo "  log files purged: $logs_deleted"
[[ "$DRY_RUN" -eq 1 ]] && echo "  (dry run — nothing was deleted)"
echo "Done."

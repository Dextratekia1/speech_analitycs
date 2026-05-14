#!/usr/bin/env bash
# scripts/run_pipeline.sh — operational runner for audios-natura-v2
# Three explicit SFTP modes: real (production), test (synthetic), dry-run (no SFTP).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

IMG_FETCHER="localhost/audios-natura/audio-fetcher-rs:dev"
IMG_CONVERTER="localhost/audios-natura/audio-converter-rs:dev"
IMG_MATCHER="localhost/audios-natura/metadata-matcher-rs:dev"
IMG_UPLOADER="localhost/audios-natura/audio-uploader-go:dev"
IMG_PIPELINE="${PIPELINE_IMG:-localhost/audios-natura/pipeline-runner:dev}"

MSSQL_SECRET="${MSSQL_SECRET:-mssql-env-v2}"
SFTP_SECRET="${SFTP_SECRET:-sftp-env}"
REAL_SFTP_SECRET_MOUNT="/run/secrets/sftp-env"

print_usage() {
    cat <<'EOF'
Usage: scripts/run_pipeline.sh --sftp-mode <real|test|dry-run> [OPTIONS]

SFTP mode (required):
  --sftp-mode real       Live production SFTP. Requires sftp-env Podman secret.
                         With PIPELINE_NETWORK_MODE=work-netns (dev default), also
                         requires work-netns running.
  --sftp-mode test       Test SFTP using a synthetic credential file. Requires
                         --test-sftp-env <path>. Refuses production secret paths.
  --sftp-mode dry-run    No SFTP. Stages run in dry-run mode: no downloads,
                         no conversions, no SFTP connections.

Date selection (one required):
  --date YYYY-MM-DD            Process a single date.
  --start YYYY-MM-DD           Start of date range (inclusive). Requires --end.
  --end   YYYY-MM-DD           End of date range (inclusive). Requires --start.

Client:
  --client <name|all>    Client to process. 'all' loads every entry from
                         shared/config/clients/enabled.txt.
                         Default: all. Override file with CLIENTS_FILE env var.

Pipeline mode (stages):
  --mode <full|fetch|convert|match|upload>    Default: full.
    full     Run complete pipeline via pipeline-runner (recommended).
    fetch    Run audio-fetcher-rs only.
    convert  Run audio-converter-rs only.
    match    Run fetcher + converter + metadata-matcher-rs.
    upload   Run audio-uploader-go only.

Build:
  --build    Rebuild all images before running.

Test SFTP:
  --test-sftp-env <path>    Synthetic SFTP env file (required for --sftp-mode test).
                            Must not be a production secret path.

Conversion concurrency:
  --conversion-concurrency <N>    Thread pool size for audio-converter-rs.
                                  Operational runner default: 4 (based on OPS-R5 benchmark).
                                  audio-converter-rs internal default: 2 (direct invocation).
                                  N must be a positive integer (≥ 1).

Run label:
  --run-label <LABEL>    Optional label appended to the run ID (e.g. 'cc4').
                         Allows repeated runs for the same client/date without
                         overwriting previous results. 1-32 chars: letters,
                         digits, '-', '_'. No slashes, spaces, or special chars.
                         Labels must not contain paths or PII.

  --help    Show this help.

Environment overrides (set before calling this script — no CLI flags):
  PIPELINE_IMG=<tag>              Image tag for pipeline-runner.
                                  Default: localhost/audios-natura/pipeline-runner:dev
                                  Production: :release (set by run_production.sh).
  PIPELINE_NETWORK_MODE=<mode>    Container network mode.
                                  work-netns (default): --network container:work-netns.
                                  default: no explicit --network flag (for production).
  CLIENTS_FILE=<path>             Path to enabled clients list.
                                  Default: shared/config/clients/enabled.txt.

Examples:
  # Dry-run for a single date:
  scripts/run_pipeline.sh --sftp-mode dry-run --client natura --date 2026-05-14

  # Test SFTP with synthetic credentials:
  scripts/run_pipeline.sh --sftp-mode test --test-sftp-env /tmp/my-test-sftp.env \
    --client maf --date 2026-05-14

  # Real production run (requires work-netns running and sftp-env Podman secret):
  scripts/run_pipeline.sh --sftp-mode real --client all \
    --start 2026-05-01 --end 2026-05-14

  # Rebuild images then run:
  scripts/run_pipeline.sh --sftp-mode dry-run --date 2026-05-14 --build
EOF
}

usage() { print_usage >&2; exit 1; }

die() { echo "ERROR: $*" >&2; exit 1; }

# ---- Argument parsing ----
SFTP_MODE=""
CLIENT="all"
DATE=""
START_DATE=""
END_DATE=""
PIPELINE_MODE="full"
BUILD=0
TEST_SFTP_ENV=""
# Operational default: 4 (OPS-R5 benchmark). audio-converter-rs internal default is 2.
CONVERSION_CONCURRENCY="4"
RUN_LABEL=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sftp-mode)     SFTP_MODE="$2";       shift 2 ;;
        --client)        CLIENT="$2";          shift 2 ;;
        --date)          DATE="$2";            shift 2 ;;
        --start)         START_DATE="$2";      shift 2 ;;
        --end)           END_DATE="$2";        shift 2 ;;
        --mode)          PIPELINE_MODE="$2";   shift 2 ;;
        --build)         BUILD=1;              shift   ;;
        --test-sftp-env)           TEST_SFTP_ENV="$2";           shift 2 ;;
        --conversion-concurrency)  CONVERSION_CONCURRENCY="$2";  shift 2 ;;
        --run-label)               RUN_LABEL="$2";                shift 2 ;;
        --help|-h)       print_usage; exit 0 ;;
        *) die "Unknown option: $1. Use --help for usage." ;;
    esac
done

# ---- Validation ----

[[ -z "$SFTP_MODE" ]] && die "--sftp-mode is required (real|test|dry-run). Use --help for usage."

case "$SFTP_MODE" in
    real|test|dry-run) ;;
    *) die "--sftp-mode must be real, test, or dry-run; got: '$SFTP_MODE'" ;;
esac


case "$PIPELINE_MODE" in
    full|fetch|convert|match|upload) ;;
    *) die "--mode must be full|fetch|convert|match|upload; got: '$PIPELINE_MODE'" ;;
esac

DATE_RE='^[0-9]{4}-[0-9]{2}-[0-9]{2}$'

if [[ -n "$DATE" && (-n "$START_DATE" || -n "$END_DATE") ]]; then
    die "--date cannot be combined with --start/--end."
fi

if [[ -z "$DATE" && -z "$START_DATE" && -z "$END_DATE" ]]; then
    die "Provide --date or --start/--end. Use --help for usage."
fi

if [[ -n "$DATE" ]]; then
    [[ "$DATE" =~ $DATE_RE ]] || die "--date must be YYYY-MM-DD; got: '$DATE'"
    START_DATE="$DATE"
    END_DATE="$DATE"
else
    [[ -n "$START_DATE" ]] || die "--start is required when using date range."
    [[ -n "$END_DATE"   ]] || die "--end is required when using date range."
    [[ "$START_DATE" =~ $DATE_RE ]] || die "--start must be YYYY-MM-DD; got: '$START_DATE'"
    [[ "$END_DATE"   =~ $DATE_RE ]] || die "--end must be YYYY-MM-DD; got: '$END_DATE'"
    [[ "$START_DATE" > "$END_DATE" ]] && \
        die "--start ($START_DATE) must not be after --end ($END_DATE)."
fi

# SFTP mode-specific validation

if [[ "$SFTP_MODE" == "test" ]]; then
    [[ -z "$TEST_SFTP_ENV" ]] && \
        die "--sftp-mode test requires --test-sftp-env <path>. Provide a synthetic SFTP credential file."

    # Refuse production secret paths.
    ABS_TEST_SFTP_ENV="$(realpath -m "$TEST_SFTP_ENV" 2>/dev/null || echo "$TEST_SFTP_ENV")"
    ABS_REAL_SECRET="$(realpath -m "$REPO_ROOT/secrets/sftp.env" 2>/dev/null || echo "$REPO_ROOT/secrets/sftp.env")"

    if [[ "$ABS_TEST_SFTP_ENV" == "$REAL_SFTP_SECRET_MOUNT" || \
          "$ABS_TEST_SFTP_ENV" == "$ABS_REAL_SECRET" ]]; then
        die "--test-sftp-env '$TEST_SFTP_ENV' is a production secret path. " \
            "Test mode must use a synthetic credential file, not a real secret."
    fi

    [[ -f "$TEST_SFTP_ENV" ]] || \
        die "--test-sftp-env '$TEST_SFTP_ENV': file not found."
fi

# Production-safe network mode. Default: work-netns (dev). Production sets: default.
PIPELINE_NETWORK_MODE="${PIPELINE_NETWORK_MODE:-work-netns}"
case "$PIPELINE_NETWORK_MODE" in
    work-netns|default) ;;
    *) die "PIPELINE_NETWORK_MODE must be 'work-netns' or 'default'; got: '$PIPELINE_NETWORK_MODE'" ;;
esac

if [[ "$SFTP_MODE" == "real" && "$PIPELINE_NETWORK_MODE" == "work-netns" ]]; then
    if ! podman inspect -f '{{.State.Running}}' work-netns 2>/dev/null | grep -qx true; then
        die "work-netns container is not running. Real SFTP mode with work-netns requires VPN namespace. " \
            "Start work-netns, or set PIPELINE_NETWORK_MODE=default for production."
    fi
fi

if [[ -n "$CONVERSION_CONCURRENCY" ]]; then
    [[ "$CONVERSION_CONCURRENCY" =~ ^[1-9][0-9]*$ ]] || \
        die "--conversion-concurrency must be a positive integer ≥ 1; got: '$CONVERSION_CONCURRENCY'"
fi

# Conversion concurrency flag array — forwarded to pipeline-runner and audio-converter-rs.
CC_ARGS=()
[[ -n "$CONVERSION_CONCURRENCY" ]] && CC_ARGS=(--conversion-concurrency "$CONVERSION_CONCURRENCY")

# Run label validation: safe token before any container starts.
LABEL_SUFFIX=""
if [[ -n "$RUN_LABEL" ]]; then
    [[ "$RUN_LABEL" =~ ^[A-Za-z0-9_-]{1,32}$ ]] || \
        die "--run-label must be 1-32 chars (letters, digits, '-' or '_'); got: '$RUN_LABEL'"
    LABEL_SUFFIX="_${RUN_LABEL}"
fi
# Run label forwarding args for pipeline-runner (defense-in-depth).
RUN_LABEL_ARGS=()
[[ -n "$RUN_LABEL" ]] && RUN_LABEL_ARGS=(--run-label "$RUN_LABEL")

# ---- Build images ----

if [[ "$BUILD" -eq 1 ]]; then
    echo "Building images..."
    podman build -t "$IMG_FETCHER"   -f audio-fetcher-rs/Containerfile .
    podman build -t "$IMG_CONVERTER" -f audio-converter-rs/Containerfile .
    podman build -t "$IMG_MATCHER"   -f metadata-matcher-rs/Containerfile .
    podman build -t "$IMG_UPLOADER"  -f audio-uploader-go/Containerfile .
    podman build -t "$IMG_PIPELINE"  -f pipeline-runner/Containerfile .
    echo ""
fi

# ---- Image existence check ----

check_image() {
    local img="$1"
    if ! podman image exists "$img" 2>/dev/null; then
        die "Image not found: $img. Run with --build to build images first."
    fi
}

case "$PIPELINE_MODE" in
    full)    check_image "$IMG_PIPELINE" ;;
    fetch)   check_image "$IMG_FETCHER" ;;
    convert) check_image "$IMG_CONVERTER" ;;
    match)   check_image "$IMG_FETCHER"; check_image "$IMG_CONVERTER"; check_image "$IMG_MATCHER" ;;
    upload)  check_image "$IMG_UPLOADER" ;;
esac

# ---- Client discovery from enabled.txt ----
CLIENTS_FILE="${CLIENTS_FILE:-shared/config/clients/enabled.txt}"

if [[ "$CLIENT" != "all" ]]; then
    [[ "$CLIENT" =~ ^[a-z0-9_-]+$ ]] || \
        die "--client must be 'all' or a safe token (lowercase letters, digits, '-', '_'); got: '$CLIENT'"
fi

[[ -f "$CLIENTS_FILE" ]] || \
    die "Client list file not found: $CLIENTS_FILE"

mapfile -t ALL_CLIENTS < <(grep -E '^[a-z0-9_-]+$' -- "$CLIENTS_FILE")

[[ "${#ALL_CLIENTS[@]}" -gt 0 ]] || \
    die "No valid client entries in $CLIENTS_FILE. File must contain at least one client token."

_dup=$(printf '%s\n' "${ALL_CLIENTS[@]}" | sort | uniq -d)
if [[ -n "$_dup" ]]; then
    die "Duplicate client '$_dup' in $CLIENTS_FILE. Remove duplicates before running."
fi
unset _dup

if [[ "$CLIENT" == "all" ]]; then
    CLIENTS=("${ALL_CLIENTS[@]}")
else
    CLIENTS=()
    for _c in "${ALL_CLIENTS[@]}"; do
        if [[ "$_c" == "$CLIENT" ]]; then
            CLIENTS=("$CLIENT")
            break
        fi
    done
    [[ "${#CLIENTS[@]}" -gt 0 ]] || \
        die "--client '$CLIENT' is not listed in $CLIENTS_FILE"
    unset _c
fi

# ---- Summary ----

echo "=== run_pipeline.sh ==="
echo "  sftp-mode  : $SFTP_MODE"
echo "  client(s)  : ${CLIENTS[*]}"
echo "  date range : $START_DATE — $END_DATE"
echo "  mode       : $PIPELINE_MODE"
[[ "$SFTP_MODE" == "test" ]] && echo "  test-env   : $TEST_SFTP_ENV"
[[ -n "$CONVERSION_CONCURRENCY" ]] && echo "  concurrency: $CONVERSION_CONCURRENCY"
[[ -n "$RUN_LABEL" ]] && echo "  run-label  : $RUN_LABEL"
echo "  network    : $PIPELINE_NETWORK_MODE"
echo ""

mkdir -p logs shared/runs

case "$PIPELINE_NETWORK_MODE" in
    work-netns) NETWORK_ARGS=(--network "container:work-netns") ;;
    default)    NETWORK_ARGS=() ;;
esac
VOL_SHARED="$(pwd)/shared:/shared:Z"

# ---- Stage runner helpers ----

# run_full: uses pipeline-runner image (all stages).
run_full() {
    local client="$1" d="$2" run_id="$3"
    case "$SFTP_MODE" in
        real)
            podman run --rm \
                "${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"}" \
                -v "$VOL_SHARED" \
                --secret "${MSSQL_SECRET},type=mount" \
                --secret "${SFTP_SECRET},type=mount" \
                -e RUST_LOG=info \
                "$IMG_PIPELINE" \
                    --client "$client" --date "$d" --run-id "$run_id" \
                    "${CC_ARGS[@]+"${CC_ARGS[@]}"}" \
                    "${RUN_LABEL_ARGS[@]+"${RUN_LABEL_ARGS[@]}"}"
            ;;
        test)
            podman run --rm \
                "${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"}" \
                -v "$VOL_SHARED" \
                -v "${TEST_SFTP_ENV}:/run/secrets/test-sftp-env:Z,ro" \
                --secret "${MSSQL_SECRET},type=mount" \
                -e RUST_LOG=info \
                "$IMG_PIPELINE" \
                    --client "$client" --date "$d" --run-id "$run_id" \
                    --sftp-secret-path /run/secrets/test-sftp-env \
                    "${CC_ARGS[@]+"${CC_ARGS[@]}"}" \
                    "${RUN_LABEL_ARGS[@]+"${RUN_LABEL_ARGS[@]}"}"
            ;;
        dry-run)
            podman run --rm \
                -v "$VOL_SHARED" \
                -e RUST_LOG=info \
                "$IMG_PIPELINE" \
                    --client "$client" --date "$d" --run-id "$run_id" \
                    --dry-run \
                    "${CC_ARGS[@]+"${CC_ARGS[@]}"}" \
                    "${RUN_LABEL_ARGS[@]+"${RUN_LABEL_ARGS[@]}"}"
            ;;
    esac
}

# run_fetch: audio-fetcher-rs only.
run_fetch() {
    local client="$1" d="$2" run_id="$3"
    local dry_flag=()
    [[ "$SFTP_MODE" == "dry-run" ]] && dry_flag=(--dry-run)
    podman run --rm \
        "${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"}" \
        -v "$VOL_SHARED" \
        -e RUST_LOG=info \
        "$IMG_FETCHER" \
            --client "$client" --date "$d" --run-id "$run_id" \
            "${dry_flag[@]+"${dry_flag[@]}"}"
}

# run_convert: audio-converter-rs only.
run_convert() {
    local client="$1" d="$2" run_id="$3"
    local dry_flag=()
    [[ "$SFTP_MODE" == "dry-run" ]] && dry_flag=(--dry-run)
    podman run --rm \
        "${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"}" \
        -v "$VOL_SHARED" \
        -e RUST_LOG=info \
        "$IMG_CONVERTER" \
            --client "$client" --date "$d" --run-id "$run_id" \
            "${dry_flag[@]+"${dry_flag[@]}"}" \
            "${CC_ARGS[@]+"${CC_ARGS[@]}"}"
}

# run_match: fetcher + converter + metadata-matcher-rs.
run_match() {
    local client="$1" d="$2" run_id="$3"
    local dry_flag=()
    [[ "$SFTP_MODE" == "dry-run" ]] && dry_flag=(--dry-run)
    run_fetch "$client" "$d" "$run_id"
    run_convert "$client" "$d" "$run_id"
    podman run --rm \
        "${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"}" \
        -v "$VOL_SHARED" \
        --secret "${MSSQL_SECRET},type=mount" \
        -e RUST_LOG=info \
        "$IMG_MATCHER" \
            --client "$client" --date "$d" --run-id "$run_id" \
            "${dry_flag[@]+"${dry_flag[@]}"}"
}

# run_upload: audio-uploader-go only.
run_upload() {
    local client="$1" d="$2" run_id="$3"
    case "$SFTP_MODE" in
        real)
            podman run --rm \
                "${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"}" \
                -v "$VOL_SHARED" \
                --secret "${SFTP_SECRET},type=mount" \
                -e RUST_LOG=info \
                "$IMG_UPLOADER" \
                    --client "$client" --date "$d" --run-id "$run_id"
            ;;
        test)
            podman run --rm \
                "${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"}" \
                -v "$VOL_SHARED" \
                -v "${TEST_SFTP_ENV}:/run/secrets/test-sftp-env:Z,ro" \
                -e RUST_LOG=info \
                "$IMG_UPLOADER" \
                    --client "$client" --date "$d" --run-id "$run_id" \
                    --sftp-secret-path /run/secrets/test-sftp-env
            ;;
        dry-run)
            podman run --rm \
                -v "$VOL_SHARED" \
                -e RUST_LOG=info \
                "$IMG_UPLOADER" \
                    --client "$client" --date "$d" --run-id "$run_id" \
                    --dry-run
            ;;
    esac
}

# ---- Main loop ----

for client in "${CLIENTS[@]}"; do
    d="$START_DATE"
    while [[ "$d" < "$END_DATE" || "$d" == "$END_DATE" ]]; do
        run_id="pipe_${client}_$(date -d "$d" +%Y%m%d)${LABEL_SUFFIX}"
        run_dir="shared/runs/$client/$d/$run_id"
        log="logs/${client}_${d}_${SFTP_MODE}.log"

        if [[ -d "$run_dir" ]]; then
            echo "SKIP $client $d (exists: $run_dir)"
            d=$(date -d "$d +1 day" +%F)
            continue
        fi

        echo "RUN  $client $d sftp_mode=$SFTP_MODE pipeline_mode=$PIPELINE_MODE run_id=$run_id" \
            | tee -a "$log"

        case "$PIPELINE_MODE" in
            full)    run_full    "$client" "$d" "$run_id" 2>&1 | tee -a "$log" ;;
            fetch)   run_fetch   "$client" "$d" "$run_id" 2>&1 | tee -a "$log" ;;
            convert) run_convert "$client" "$d" "$run_id" 2>&1 | tee -a "$log" ;;
            match)   run_match   "$client" "$d" "$run_id" 2>&1 | tee -a "$log" ;;
            upload)  run_upload  "$client" "$d" "$run_id" 2>&1 | tee -a "$log" ;;
        esac

        d=$(date -d "$d +1 day" +%F)
    done
done

echo ""
echo "Done."

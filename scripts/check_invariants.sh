#!/usr/bin/env bash
# Static invariant checker for audios-natura-v2.
# Exits 0 if all checks pass, exits 1 if any fail.
# Safe to run repeatedly; does not modify any file or connect to any service.
# Usage: bash scripts/check_invariants.sh
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

failures=0

fail() {
    echo "  FAIL: $*"
    failures=$((failures + 1))
}

pass() {
    echo "  PASS: $*"
}

# check_absent DESC PATTERN PATH...
# Fails if PATTERN is found anywhere in the given paths (grep -rn, BRE).
check_absent() {
    local desc="$1" pattern="$2"
    shift 2
    local hits
    hits="$(grep -rn -- "$pattern" "$@" 2>/dev/null)"
    if [[ -n "$hits" ]]; then
        fail "$desc"
        printf '%s\n' "$hits" | head -5 | sed 's/^/    /'
    else
        pass "$desc"
    fi
}

# check_present DESC PATTERN PATH...
# Fails if PATTERN is NOT found in any of the given paths (grep -rn, BRE).
check_present() {
    local desc="$1" pattern="$2"
    shift 2
    if grep -rn -q -- "$pattern" "$@" 2>/dev/null; then
        pass "$desc"
    else
        fail "$desc — required anchor absent"
    fi
}

echo "=== audios-natura-v2 static invariant check ==="
echo "repo: $REPO_ROOT"
echo ""

# ==========================================================================
echo "--- [1] SFTP unsafe host key callback ---"
# Exclude pure comment lines (// ...) — explanatory comments that name the
# forbidden API are documentation of absence, not actual usage.
_inv1_hits="$(grep -rn -- "ssh\.InsecureIgnoreHostKey" "audio-uploader-go/" 2>/dev/null \
    | grep -Ev ':[0-9]+:[[:space:]]*//')"
if [[ -n "$_inv1_hits" ]]; then
    fail "ssh.InsecureIgnoreHostKey absent from audio-uploader-go"
    printf '%s\n' "$_inv1_hits" | head -5 | sed 's/^/    /'
else
    pass "ssh.InsecureIgnoreHostKey absent from audio-uploader-go"
fi
echo ""

# ==========================================================================
echo "--- [2] SFTP credential environment mutation ---"
# Targets os.Setenv (production credential leak path).
# Does NOT match t.Setenv, which is test-scoped, safe, and expected in test files.
check_absent \
    "os.Setenv absent from audio-uploader-go" \
    "os\.Setenv" \
    "audio-uploader-go/"
echo ""

# ==========================================================================
echo "--- [3] Stale network namespace name ---"
# CLAUDE.md is excluded: it defines this invariant rule by naming the stale value.
# check_invariants.sh is excluded: it contains the pattern as a grep argument.
# Both are meta/tooling references, not active configuration.
check_absent \
    "container:container-vpn absent from active config/docs/scripts" \
    "container:container-vpn" \
    "README.md" "SECURITY.md" \
    "podman-compose.yml" "podman-compose.override.yml" \
    "scripts/run_range.sh" "scripts/run_rangev2.sh" "scripts/run_pipeline_dev.sh"
echo ""

# ==========================================================================
echo "--- [4] Deprecated operational script reference guard ---"
# Any reference to run_rangev2 in docs/compose/scripts outside the deprecated
# script file itself must appear on a line that also contains "deprecated" or
# "obsoleto". This catches new active operational references being added.
echo "  Checking run_rangev2.sh references outside script file..."
bad_v2_refs=0
while IFS= read -r hit; do
    content="${hit#*:}"
    content="${content#*:}"
    if echo "$content" | grep -qiE "deprecated|obsoleto"; then
        : # allowed — deprecated context present on this line
    else
        fail "run_rangev2 reference lacks deprecated/obsoleto context"
        printf '    %s\n' "$hit"
        bad_v2_refs=$((bad_v2_refs + 1))
    fi
done < <(
    # Never traverses secrets/ (credentials), shared/ (runtime PII), target/, or .git/.
    grep -rn \
        --include="*.md" --include="*.yml" --include="*.sh" \
        --exclude-dir=.git --exclude-dir=target --exclude-dir=shared \
        --exclude-dir=secrets \
        -- "run_rangev2" . 2>/dev/null \
        | grep -v "^\./scripts/run_rangev2\.sh:" \
        | grep -v "^\./scripts/check_invariants\.sh:"
)
if [[ "$bad_v2_refs" -eq 0 ]]; then
    pass "run_rangev2 references are all in deprecated context (or absent)"
fi
echo ""

# ==========================================================================
echo "--- [5] Removed MSSQL CA-file forward reference ---"
check_absent \
    "\"proper CA validation path\" absent from docs" \
    "proper CA validation path" \
    "README.md" "SECURITY.md" "CLAUDE.md"
echo ""

# ==========================================================================
echo "--- [6] Schema version 3 regression guard ---"
# Checks that no source or doc file assigns schema_version the value 3.
# Current expected values: pipeline.json=1, fetch/convert/match manifests=1, upload.json=2.
# Pattern matches struct/JSON/YAML assignments: schema_version: 3  SchemaVersion: 3
# Uses ERE (-E) with [[:space:]] to avoid false positives from type names like u32.
schema_v3_files=(
    "pipeline-runner/src/main.rs"
    "audio-uploader-go/cmd/audio-uploader-go/main.go"
    "audio-fetcher-rs/src/main.rs"
    "audio-converter-rs/src/main.rs"
    "metadata-matcher-rs/src/main.rs"
    "README.md"
    "CLAUDE.md"
    "SECURITY.md"
)
schema_v3_pattern='(schema_version|SchemaVersion)[[:space:]]*[=:][[:space:]]*3'
schema_v3_hits=""
for f in "${schema_v3_files[@]}"; do
    [[ -f "$f" ]] || continue
    h="$(grep -En "$schema_v3_pattern" -- "$f" 2>/dev/null)"
    [[ -n "$h" ]] && schema_v3_hits="${schema_v3_hits}${f}: ${h}"$'\n'
done
if [[ -d "crates/common/src" ]]; then
    h="$(grep -rEn "$schema_v3_pattern" -- "crates/common/src/" 2>/dev/null)"
    [[ -n "$h" ]] && schema_v3_hits="${schema_v3_hits}${h}"$'\n'
fi
if [[ -n "$schema_v3_hits" ]]; then
    fail "No schema_version 3 in source/docs — unexpected version 3 assignment found"
    printf '%s\n' "$schema_v3_hits" | head -10 | sed 's/^/    /'
else
    pass "No schema_version 3 in source/docs"
fi
echo ""

# ==========================================================================
echo "--- [7] Required positive anchors ---"
check_present \
    "ssh.FixedHostKey present in audio-uploader-go" \
    "ssh\.FixedHostKey" \
    "audio-uploader-go/"

check_present \
    "HostKeyAlgorithms present in audio-uploader-go" \
    "HostKeyAlgorithms" \
    "audio-uploader-go/"

check_present \
    "SFTP_HOST_KEY present in audio-uploader-go" \
    "SFTP_HOST_KEY" \
    "audio-uploader-go/"

check_present \
    "MSSQL_TRUST_CERT present in metadata-matcher-rs" \
    "MSSQL_TRUST_CERT" \
    "metadata-matcher-rs/"

check_present \
    "MSSQL_ENCRYPT present in metadata-matcher-rs" \
    "MSSQL_ENCRYPT" \
    "metadata-matcher-rs/"

check_present \
    "stderr_tail present in pipeline-runner" \
    "stderr_tail" \
    "pipeline-runner/"

check_present \
    "upload_send_error present in pipeline-runner" \
    "upload_send_error" \
    "pipeline-runner/"

check_present \
    "container:work-netns present in podman-compose.override.yml" \
    "container:work-netns" \
    "podman-compose.override.yml"

check_present \
    "run_rangev2.sh deprecated notice present in README.md" \
    "run_rangev2" \
    "README.md"

check_present \
    "run_rangev2.sh deprecated notice present in CLAUDE.md" \
    "run_rangev2" \
    "CLAUDE.md"
echo ""

# ==========================================================================
echo "--- [8] Stale documentation phrases ---"
check_absent \
    "\"go.sum is not committed\" absent from docs" \
    "go\.sum is not committed" \
    "README.md" "SECURITY.md" "CLAUDE.md"

check_absent \
    "\"generated at build time\" absent from docs" \
    "generated at build time" \
    "README.md" "SECURITY.md" "CLAUDE.md"

check_absent \
    "\"aggregation not yet implemented\" absent" \
    "aggregation not yet implemented" \
    "README.md" "SECURITY.md" "CLAUDE.md" "pipeline-runner/"

check_absent \
    "\"Phase 2H-B\" absent from docs" \
    "Phase 2H-B" \
    "README.md" "SECURITY.md" "CLAUDE.md"
echo ""

# ==========================================================================
echo "--- [9] OPS invariant contract anchors ---"
# Verify that CLAUDE.md contains the OPS INVARIANT CONTRACT section and its
# required phrases. These checks protect against accidental deletion of the
# invariant contract or its key findings.
check_present \
    "OPS INVARIANT CONTRACT section present in CLAUDE.md" \
    "OPS INVARIANT CONTRACT" \
    "CLAUDE.md"

check_present \
    "manual_approval limitation documented in CLAUDE.md" \
    "manual_approval" \
    "CLAUDE.md"

check_present \
    "needs_approval finding documented in CLAUDE.md" \
    "needs_approval" \
    "CLAUDE.md"

check_present \
    "no-approve flag limitation documented in CLAUDE.md" \
    "no --approve" \
    "CLAUDE.md"

check_present \
    "shared/runs PII boundary documented in CLAUDE.md" \
    "shared/runs" \
    "CLAUDE.md"

check_present \
    "G-INVARIANTS-PASS gate documented in CLAUDE.md" \
    "G-INVARIANTS-PASS" \
    "CLAUDE.md"

check_present \
    "future prompt rule documented in CLAUDE.md" \
    "OPS invariant contract applies" \
    "CLAUDE.md"
echo ""

# ==========================================================================
echo "--- [10] OPS-18 run_pipeline.sh anchors ---"
# Verify that scripts/run_pipeline.sh exists and contains required interface markers.
check_present \
    "--sftp-mode flag present in scripts/run_pipeline.sh" \
    "--sftp-mode" \
    "scripts/run_pipeline.sh"

check_present \
    "--test-sftp-env flag present in scripts/run_pipeline.sh" \
    "--test-sftp-env" \
    "scripts/run_pipeline.sh"

check_present \
    "real|test|dry-run modes documented in scripts/run_pipeline.sh" \
    "dry-run" \
    "scripts/run_pipeline.sh"

check_present \
    "--sftp-secret-path flag present in pipeline-runner/src/main.rs" \
    "sftp.secret.path" \
    "pipeline-runner/src/main.rs"
echo ""

# ==========================================================================
echo "--- [11] OPS-T2A test SFTP server harness anchors ---"
# Verify that the test SFTP server harness exists and preserves key safety
# properties: no InsecureIgnoreHostKey, proper host key generation, container
# name and network namespace usage consistent with pipeline test mode.

check_present \
    "InMemHandler (SFTP serving) present in test-sftp-server" \
    "InMemHandler" \
    "audio-uploader-go/cmd/test-sftp-server/"

check_present \
    "SFTP_HOST_KEY written by test-sftp-server" \
    "SFTP_HOST_KEY" \
    "audio-uploader-go/cmd/test-sftp-server/"

check_present \
    "AddHostKey present in test-sftp-server" \
    "AddHostKey" \
    "audio-uploader-go/cmd/test-sftp-server/"

check_absent \
    "InsecureIgnoreHostKey absent from test-sftp-server" \
    "InsecureIgnoreHostKey" \
    "audio-uploader-go/cmd/test-sftp-server/"

check_present \
    "test-sftp-server binary referenced in Containerfile.test-sftp-server" \
    "test-sftp-server" \
    "Containerfile.test-sftp-server"

check_present \
    "container:work-netns present in scripts/start_test_sftp.sh" \
    "container:work-netns" \
    "scripts/start_test_sftp.sh"

check_present \
    "audios-test-sftp container name present in scripts/start_test_sftp.sh" \
    "audios-test-sftp" \
    "scripts/start_test_sftp.sh"

check_present \
    "audios-test-sftp container name present in scripts/stop_test_sftp.sh" \
    "audios-test-sftp" \
    "scripts/stop_test_sftp.sh"

check_absent \
    "env file contents not printed in scripts/start_test_sftp.sh" \
    "cat.*sftp" \
    "scripts/start_test_sftp.sh"

check_present \
    "--sftp-mode still present in scripts/run_pipeline.sh" \
    "--sftp-mode" \
    "scripts/run_pipeline.sh"
echo ""

# ==========================================================================
echo "--- Summary ---"
if [[ "$failures" -eq 0 ]]; then
    echo "All checks PASSED (0 failures)."
    exit 0
else
    printf '%d check(s) FAILED.\n' "$failures"
    exit 1
fi

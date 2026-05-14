#!/usr/bin/env bash
# Build, tag, and export the production release image for audios-natura-v2.
#
# Usage: scripts/build_release.sh [OPTIONS]
#
# Options:
#   --allow-dirty        Build even if the working tree is not clean.
#                        Not recommended for production releases.
#   --no-save            Build and tag only; do not save the archive.
#   --output-dir <dir>   Output directory for the archive. Default: dist/
#   --help               Show this help.
#
# Environment overrides:
#   OUTPUT_DIR=<dir>     Same as --output-dir. CLI flag takes precedence.
#
# What this script does:
#   1. Verifies clean git working tree (unless --allow-dirty).
#   2. Builds: localhost/audios-natura/pipeline-runner:git-<sha7>
#   3. Tags:   localhost/audios-natura/pipeline-runner:release
#   4. Saves:  <output-dir>/pipeline-runner-git-<sha7>.tar.gz (unless --no-save)
#
# What this script does NOT do:
#   - Push to a registry.
#   - Load the image on the production host.
#   - Run the pipeline.
#   - Read secrets.
#   - Modify systemd units.
#   - Commit or push git changes.
#
# See deploy/release_checklist.md for the full release workflow.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ---- Defaults ----
ALLOW_DIRTY=0
NO_SAVE=0
OUTPUT_DIR="${OUTPUT_DIR:-dist}"

# ---- Argument parsing ----
while [[ $# -gt 0 ]]; do
    case "$1" in
        --allow-dirty)
            ALLOW_DIRTY=1; shift ;;
        --no-save)
            NO_SAVE=1; shift ;;
        --output-dir)
            [[ $# -ge 2 ]] || { echo "ERROR: --output-dir requires an argument" >&2; exit 1; }
            OUTPUT_DIR="$2"; shift 2 ;;
        --help|-h)
            cat <<'HELP'
Build, tag, and export the production release image for audios-natura-v2.

Usage: scripts/build_release.sh [OPTIONS]

Options:
  --allow-dirty        Build even if the working tree is not clean.
                       Not recommended for production releases.
  --no-save            Build and tag only; do not save the archive.
  --output-dir <dir>   Output directory for the archive. Default: dist/
  --help               Show this help.

Environment: OUTPUT_DIR=<dir> (same as --output-dir; CLI takes precedence)

Steps performed:
  1. Verify clean git working tree (unless --allow-dirty)
  2. podman build ... localhost/audios-natura/pipeline-runner:git-<sha7>
  3. podman tag  ... localhost/audios-natura/pipeline-runner:release
  4. podman save ... | gzip > <output-dir>/pipeline-runner-git-<sha7>.tar.gz

See deploy/release_checklist.md for the full release workflow.
HELP
            exit 0 ;;
        *)
            echo "ERROR: Unknown argument: $1" >&2; exit 1 ;;
    esac
done

# ---- Working tree check ----
if [[ "$ALLOW_DIRTY" -eq 0 ]]; then
    _dirty=0
    git diff --quiet HEAD 2>/dev/null || _dirty=1
    git diff --cached --quiet 2>/dev/null || _dirty=1
    if [[ "$_dirty" -eq 1 ]]; then
        echo "ERROR: Working tree is dirty. Commit or stash changes before building a release." >&2
        echo "       Use --allow-dirty to override (not recommended for production releases)." >&2
        git status --short >&2
        exit 1
    fi
    _untracked="$(git ls-files --others --exclude-standard)"
    if [[ -n "$_untracked" ]]; then
        echo "WARNING: Untracked files present (not included in image):" >&2
        echo "$_untracked" | head -10 | sed 's/^/  /' >&2
    fi
    unset _dirty _untracked
fi

# ---- Resolve SHA ----
SHA7="$(git rev-parse --short=7 HEAD)"
IMG_BASE="localhost/audios-natura/pipeline-runner"
IMG_SHA="${IMG_BASE}:git-${SHA7}"
IMG_RELEASE="${IMG_BASE}:release"

echo "=== build_release.sh ==="
echo "  sha7        : $SHA7"
echo "  image       : $IMG_SHA"
echo "  tag release : $IMG_RELEASE"
if [[ "$NO_SAVE" -eq 1 ]]; then
    echo "  save        : no (--no-save)"
else
    echo "  save        : ${OUTPUT_DIR}/pipeline-runner-git-${SHA7}.tar.gz"
fi
echo "  allow-dirty : $ALLOW_DIRTY"
echo ""

# ---- Build ----
echo "--- Building $IMG_SHA ---"
podman build \
    -t "$IMG_SHA" \
    -f pipeline-runner/Containerfile \
    .
echo "Build OK: $IMG_SHA"
echo ""

# ---- Tag as :release ----
echo "--- Tagging $IMG_RELEASE ---"
podman tag "$IMG_SHA" "$IMG_RELEASE"
echo "Tag OK: $IMG_RELEASE"
echo ""

# ---- Save ----
if [[ "$NO_SAVE" -eq 0 ]]; then
    mkdir -p "$OUTPUT_DIR"
    ARCHIVE="${OUTPUT_DIR}/pipeline-runner-git-${SHA7}.tar.gz"
    echo "--- Saving $ARCHIVE ---"
    podman save "$IMG_RELEASE" | gzip > "$ARCHIVE"
    echo "Saved: $ARCHIVE"
    echo ""
fi

# ---- Done ----
echo "=== Build complete ==="
echo "  SHA7:    $SHA7"
echo "  Image:   $IMG_SHA"
echo "  Release: $IMG_RELEASE"
if [[ "$NO_SAVE" -eq 0 ]]; then
    echo "  Archive: ${OUTPUT_DIR}/pipeline-runner-git-${SHA7}.tar.gz"
fi
echo ""
echo "Next steps (see deploy/release_checklist.md):"
echo "  1. Verify: podman images localhost/audios-natura/pipeline-runner"
if [[ "$NO_SAVE" -eq 0 ]]; then
    echo "  2. Transfer to production host:"
    echo "       scp ${OUTPUT_DIR}/pipeline-runner-git-${SHA7}.tar.gz production-host:/tmp/"
    echo "  3. Load on production host:"
    echo "       podman load < /tmp/pipeline-runner-git-${SHA7}.tar.gz"
fi

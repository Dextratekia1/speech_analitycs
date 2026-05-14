#!/usr/bin/env bash
# Installer for audios-natura production deployment on Fedora CoreOS.
# Rootless user-level install — runs as the production user (useraval).
#
# Admin pre-requisites (run once, as root/admin, BEFORE this script):
#   sudo mkdir -p /opt/audios-natura
#   sudo chown -R useraval:useraval /opt/audios-natura
#   sudo loginctl enable-linger useraval
#
# Usage: bash deploy/install.sh [--enable-timer]
#
# --enable-timer   Enable and start the pipeline and cleanup timers after install.
#                  Default: units are installed but NOT started. Requires provisioned
#                  rootless Podman secrets and loaded release image before enabling.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/audios-natura}"
UNIT_DIR="${HOME}/.config/systemd/user"
ENABLE_TIMER=0

# ---- Must NOT run as root ----
if [[ "$(id -u)" -eq 0 ]]; then
    echo "ERROR: install.sh must not run as root." >&2
    echo "       Run as the production user (e.g. useraval)." >&2
    echo "       Admin pre-requisites (run once, as root/admin):" >&2
    echo "         sudo mkdir -p /opt/audios-natura" >&2
    echo "         sudo chown -R useraval:useraval /opt/audios-natura" >&2
    echo "         sudo loginctl enable-linger useraval" >&2
    exit 1
fi

for arg in "$@"; do
    case "$arg" in
        --enable-timer) ENABLE_TIMER=1 ;;
        *) echo "ERROR: Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

echo "=== install.sh ==="
echo "  repo root    : $REPO_ROOT"
echo "  install root : $INSTALL_ROOT"
echo "  unit dir     : $UNIT_DIR"
echo "  enable timer : $ENABLE_TIMER"
echo "  running as   : $(id -un)"
echo ""

# ---- Create directory layout ----
mkdir -p \
    "$INSTALL_ROOT/scripts" \
    "$INSTALL_ROOT/deploy" \
    "$INSTALL_ROOT/shared/config/clients" \
    "$INSTALL_ROOT/shared/runs" \
    "$INSTALL_ROOT/logs"

mkdir -p "$UNIT_DIR"

echo "Directories created."

# ---- Install scripts ----
install -m 755 "$REPO_ROOT/scripts/run_pipeline.sh"        "$INSTALL_ROOT/scripts/"
install -m 755 "$REPO_ROOT/scripts/run_production.sh"      "$INSTALL_ROOT/scripts/"
install -m 755 "$REPO_ROOT/deploy/audios-natura-cleanup.sh" "$INSTALL_ROOT/deploy/"

# ---- Copy config files ----
# shared/config/ is tracked in git; shared/runs/ is runtime-only (gitignored).
cp -r "$REPO_ROOT/shared/config/." "$INSTALL_ROOT/shared/config/"

# ---- Copy deployment reference files ----
install -m 644 "$REPO_ROOT/deploy/audios-natura-pipeline.service" "$INSTALL_ROOT/deploy/"
install -m 644 "$REPO_ROOT/deploy/audios-natura-pipeline.timer"   "$INSTALL_ROOT/deploy/"
install -m 644 "$REPO_ROOT/deploy/audios-natura-cleanup.service"  "$INSTALL_ROOT/deploy/"
install -m 644 "$REPO_ROOT/deploy/audios-natura-cleanup.timer"    "$INSTALL_ROOT/deploy/"
install -m 644 "$REPO_ROOT/DEPLOY.md"                             "$INSTALL_ROOT/"

echo "Files installed."

# ---- Install user-level systemd units ----
install -m 644 "$REPO_ROOT/deploy/audios-natura-pipeline.service" "$UNIT_DIR/"
install -m 644 "$REPO_ROOT/deploy/audios-natura-pipeline.timer"   "$UNIT_DIR/"
install -m 644 "$REPO_ROOT/deploy/audios-natura-cleanup.service"  "$UNIT_DIR/"
install -m 644 "$REPO_ROOT/deploy/audios-natura-cleanup.timer"    "$UNIT_DIR/"

systemctl --user daemon-reload
echo "User-level systemd units installed and daemon reloaded."
echo ""

# ---- Enable timers (requires --enable-timer) ----
if [[ "$ENABLE_TIMER" -eq 1 ]]; then
    systemctl --user enable --now audios-natura-pipeline.timer
    systemctl --user enable --now audios-natura-cleanup.timer
    echo "Timers enabled and started."
    systemctl --user list-timers audios-natura-pipeline.timer
else
    echo "Timers NOT enabled. Complete these steps before enabling:"
    echo ""
    echo "  1. Provision credential files (never commit these):"
    echo "       mkdir -p ~/.config/audios-natura/secrets"
    echo "       chmod 700 ~/.config/audios-natura/secrets"
    echo "       # Place mssql.env and sftp.env in that directory."
    echo "       # See secrets/*.env.example for required variable names."
    echo "       chmod 600 ~/.config/audios-natura/secrets/mssql.env"
    echo "       chmod 600 ~/.config/audios-natura/secrets/sftp.env"
    echo ""
    echo "  2. Create rootless Podman secrets (no sudo):"
    echo "       podman secret create mssql-env-v2 ~/.config/audios-natura/secrets/mssql.env"
    echo "       podman secret create sftp-env     ~/.config/audios-natura/secrets/sftp.env"
    echo "       podman secret ls"
    echo ""
    echo "  3. Load the release image (as $(id -un), no sudo):"
    echo "       podman load < /tmp/pipeline-runner-git-<sha7>.tar.gz"
    echo "       podman images localhost/audios-natura/pipeline-runner"
    echo "       # WARNING: If loaded with sudo, this user will not see the image."
    echo ""
    echo "  4. Run a manual dry validation:"
    echo "       PIPELINE_NETWORK_MODE=default \\"
    echo "       PIPELINE_IMG=localhost/audios-natura/pipeline-runner:release \\"
    echo "       $INSTALL_ROOT/scripts/run_pipeline.sh --sftp-mode dry-run --client all --date \$(date +%F)"
    echo ""
    echo "  5. Enable timers:"
    echo "       systemctl --user enable --now audios-natura-pipeline.timer"
    echo "       systemctl --user enable --now audios-natura-cleanup.timer"
    echo "       systemctl --user list-timers audios-natura-pipeline.timer"
fi

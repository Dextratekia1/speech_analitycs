#!/usr/bin/env bash
# Installer for audios-natura production deployment on Fedora CoreOS.
#
# REVIEW ONLY in OPS-D2 — authorized for execution from OPS-D4 onward.
# Requires root. Copies files to /opt/audios-natura and installs systemd units.
#
# Usage: bash deploy/install.sh [--enable-timer]
#
# --enable-timer   Enable and start the pipeline and cleanup timers after install.
#                  Default: units are installed but NOT started. Requires provisioned
#                  Podman secrets and loaded release image before enabling.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/audios-natura}"
ENABLE_TIMER=0

for arg in "$@"; do
    case "$arg" in
        --enable-timer) ENABLE_TIMER=1 ;;
        *) echo "ERROR: Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

echo "=== install.sh ==="
echo "  repo root    : $REPO_ROOT"
echo "  install root : $INSTALL_ROOT"
echo "  enable timer : $ENABLE_TIMER"
echo ""
echo "WARNING: This script requires root privileges and modifies /opt and /etc/systemd."
echo "WARNING: Authorized for execution from OPS-D4 onward only."
echo ""

# ---- Create directory layout ----
mkdir -p \
    "$INSTALL_ROOT/scripts" \
    "$INSTALL_ROOT/deploy" \
    "$INSTALL_ROOT/shared/config/clients" \
    "$INSTALL_ROOT/shared/runs" \
    "$INSTALL_ROOT/logs"

mkdir -p /etc/audios-natura/secrets
chmod 700 /etc/audios-natura/secrets

echo "Directories created."

# ---- Install scripts ----
install -m 755 "$REPO_ROOT/scripts/run_pipeline.sh"   "$INSTALL_ROOT/scripts/"
install -m 755 "$REPO_ROOT/scripts/run_production.sh" "$INSTALL_ROOT/scripts/"
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

# ---- Install systemd units ----
install -m 644 "$REPO_ROOT/deploy/audios-natura-pipeline.service" /etc/systemd/system/
install -m 644 "$REPO_ROOT/deploy/audios-natura-pipeline.timer"   /etc/systemd/system/
install -m 644 "$REPO_ROOT/deploy/audios-natura-cleanup.service"  /etc/systemd/system/
install -m 644 "$REPO_ROOT/deploy/audios-natura-cleanup.timer"    /etc/systemd/system/

systemctl daemon-reload
echo "Systemd units installed and daemon reloaded."
echo ""

# ---- Enable timers (requires --enable-timer) ----
if [[ "$ENABLE_TIMER" -eq 1 ]]; then
    systemctl enable --now audios-natura-pipeline.timer
    systemctl enable --now audios-natura-cleanup.timer
    echo "Timers enabled and started."
else
    echo "Timers NOT enabled. Complete these steps before enabling:"
    echo ""
    echo "  1. Provision credential files (never commit these):"
    echo "       # Write mssql.env and sftp.env to /etc/audios-natura/secrets/"
    echo "       # See secrets/*.env.example for required variable names."
    echo ""
    echo "  2. Create Podman secrets:"
    echo "       podman secret create mssql-env-v2 /etc/audios-natura/secrets/mssql.env"
    echo "       podman secret create sftp-env     /etc/audios-natura/secrets/sftp.env"
    echo ""
    echo "  3. Load the release image:"
    echo "       podman load < pipeline-runner-release.tar.gz"
    echo "       podman images localhost/audios-natura/pipeline-runner"
    echo ""
    echo "  4. Run a manual dry validation (OPS-D4):"
    echo "       PIPELINE_NETWORK_MODE=default \\"
    echo "       PIPELINE_IMG=localhost/audios-natura/pipeline-runner:release \\"
    echo "       $INSTALL_ROOT/scripts/run_pipeline.sh --sftp-mode dry-run --client all --date \$(date +%F)"
    echo ""
    echo "  5. Enable timers:"
    echo "       systemctl enable --now audios-natura-pipeline.timer"
    echo "       systemctl enable --now audios-natura-cleanup.timer"
fi

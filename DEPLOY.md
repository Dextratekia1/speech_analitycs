# DEPLOY.md — Production Deployment Guide

Fedora CoreOS deployment guide for the audios-natura-v2 daily pipeline.

---

## 1. Production Architecture Summary

- **Runtime:** Podman containers, rootful systemd service.
- **Image:** Single monolithic release image (`pipeline-runner/Containerfile`) containing all five pipeline binaries and `ffmpeg`.
- **Scheduling:** `Type=oneshot` systemd service triggered by a daily timer at 22:00 local time.
- **Network:** No `work-netns` required. The production server is already on the company network. Containers use the host network directly (`PIPELINE_NETWORK_MODE=default`).
- **SFTP mode:** `real` only. No test SFTP in production.
- **Clients:** `--client all`, resolved from `shared/config/clients/enabled.txt`.

---

## 2. Host Filesystem Layout

```
/opt/audios-natura/                    (deployment root)
  scripts/
    run_pipeline.sh                    (operational runner)
    run_production.sh                  (production daily wrapper)
  deploy/
    audios-natura-pipeline.service     (reference copy)
    audios-natura-pipeline.timer       (reference copy)
    audios-natura-cleanup.sh           (retention cleanup)
    audios-natura-cleanup.service      (reference copy)
    audios-natura-cleanup.timer        (reference copy)
    install.sh                         (installer — run from repo only)
  shared/
    config/
      clients/
        enabled.txt                    (enabled client list)
        maf.yml
        natura.yml
        maf/batch_lookup.yml
        natura/batch_lookup.yml
    runs/                              (PII runtime artifacts — not in git)
  logs/                                (execution logs — not in git)
  DEPLOY.md                            (this file)

/etc/audios-natura/
  secrets/                             (chmod 700 — never commit)
    mssql.env                          (chmod 600)
    sftp.env                           (chmod 600)

/etc/systemd/system/
  audios-natura-pipeline.service
  audios-natura-pipeline.timer
  audios-natura-cleanup.service
  audios-natura-cleanup.timer
```

---

## 3. Release Image Requirements

**Tag convention:**

| Tag | Purpose |
|---|---|
| `localhost/audios-natura/pipeline-runner:git-<sha7>` | Specific build — immutable, for rollback |
| `localhost/audios-natura/pipeline-runner:release` | Current production pointer |

**Rules:**
- Production units always reference `:release`.
- `:dev` must never appear in production unit files or production scripts.
- `:git-<sha7>` is created at build time; `:release` is re-tagged after validation.

**Building on dev machine:**
```bash
SHA7="$(git rev-parse --short=7 HEAD)"
podman build -t "localhost/audios-natura/pipeline-runner:git-${SHA7}" \
             -f pipeline-runner/Containerfile .
podman tag "localhost/audios-natura/pipeline-runner:git-${SHA7}" \
           "localhost/audios-natura/pipeline-runner:release"
```

**Transferring to production host (no registry):**
```bash
podman save localhost/audios-natura/pipeline-runner:release \
  | gzip > pipeline-runner-release.tar.gz
scp pipeline-runner-release.tar.gz production-host:/tmp/
# On production host:
podman load < /tmp/pipeline-runner-release.tar.gz
podman images localhost/audios-natura/pipeline-runner
```

---

## 4. Secret Provisioning

Secrets are never committed to git. See `secrets/*.env.example` for the required variable names.

Create credential files on the production host:

```
/etc/audios-natura/secrets/mssql.env   # MSSQL credentials
/etc/audios-natura/secrets/sftp.env    # SFTP credentials and host key
```

File permissions:
```bash
chmod 700 /etc/audios-natura/secrets
chmod 600 /etc/audios-natura/secrets/mssql.env
chmod 600 /etc/audios-natura/secrets/sftp.env
```

**SFTP host key:** Set `SFTP_HOST_KEY` in `sftp.env` to the server's public key in OpenSSH `authorized_keys` format. Obtain it on a trusted network:
```bash
ssh-keyscan -t ed25519 <SFTP_HOST>
```
The uploader fails closed if `SFTP_HOST_KEY` is missing or the key does not match the server.

---

## 5. Podman Secret Commands

Run once after provisioning the credential files:
```bash
podman secret create mssql-env-v2 /etc/audios-natura/secrets/mssql.env
podman secret create sftp-env     /etc/audios-natura/secrets/sftp.env
```

**Rotation:** After updating credential files:
```bash
podman secret rm mssql-env-v2 sftp-env
podman secret create mssql-env-v2 /etc/audios-natura/secrets/mssql.env
podman secret create sftp-env     /etc/audios-natura/secrets/sftp.env
```
The next scheduled run picks up new secrets automatically.

---

## 6. Network Model

**Production server is already on the company network.**

- No `work-netns` requirement in production.
- `run_production.sh` sets `PIPELINE_NETWORK_MODE=default`, which means no `--network` flag is passed to `podman run`. Containers use the host's default network.
- The production systemd service sets `Environment=PIPELINE_NETWORK_MODE=default` explicitly.

**Development/local runs** may still use `work-netns`:
```bash
# Dev default (work-netns):
scripts/run_pipeline.sh --sftp-mode real --client all --date 2026-05-14

# Production default (no explicit network):
PIPELINE_NETWORK_MODE=default scripts/run_pipeline.sh --sftp-mode real ...
```

---

## 7. Install Steps

Run from the repository root on the production host (requires root):

```bash
bash deploy/install.sh
```

This creates directories, copies scripts and config, and installs systemd units. The timer is **not** enabled by default. Complete secret provisioning and image loading before enabling.

---

## 8. Enabling the Timer

After completing §4 (secrets), §5 (Podman secrets), and loading the release image:

```bash
systemctl enable --now audios-natura-pipeline.timer
systemctl enable --now audios-natura-cleanup.timer
systemctl list-timers audios-natura-pipeline.timer
```

The pipeline runs daily at 22:00 local time. `Persistent=true` fires any missed run at next boot.

---

## 9. Manual Run — Today

```bash
/opt/audios-natura/scripts/run_production.sh
```

This uses today's date, `--client all`, `--sftp-mode real`, and the `:release` image.

---

## 10. Manual Run — Specific Date

```bash
PIPELINE_IMG=localhost/audios-natura/pipeline-runner:release \
PIPELINE_NETWORK_MODE=default \
/opt/audios-natura/scripts/run_pipeline.sh \
  --sftp-mode real \
  --client all \
  --date 2026-05-10
```

---

## 11. Rerun with Label (Skip-Safe)

If a run directory already exists, the pipeline skips it. Use `--run-label` to create a new `run_id`:

```bash
RUN_LABEL=retry1 /opt/audios-natura/scripts/run_production.sh
```

Or for a specific date:
```bash
PIPELINE_IMG=localhost/audios-natura/pipeline-runner:release \
PIPELINE_NETWORK_MODE=default \
/opt/audios-natura/scripts/run_pipeline.sh \
  --sftp-mode real \
  --client all \
  --date 2026-05-10 \
  --run-label retry1
```

---

## 12. Retention Cleanup

Runs weekly on Sundays at 03:00 via the cleanup timer.

**Manual dry run (reports counts, deletes nothing):**
```bash
/opt/audios-natura/deploy/audios-natura-cleanup.sh --dry-run
```

**Manual cleanup:**
```bash
/opt/audios-natura/deploy/audios-natura-cleanup.sh
```

**Retention policy:**
- `shared/runs/` run directories: 30 days
- `logs/` files: 60 days
- Today's directories are never deleted regardless of age.
- Secrets and config files are never touched.

---

## 13. Rollback

**Image rollback (tag previous build as :release):**
```bash
# On production host:
podman tag localhost/audios-natura/pipeline-runner:git-<previous-sha7> \
           localhost/audios-natura/pipeline-runner:release
# Verify:
podman images localhost/audios-natura/pipeline-runner
```

The next timer fire or manual run uses the rolled-back image. No service restart needed (`Type=oneshot`).

**Avoid accidental date rerun:** The pipeline's skip detection (`run_pipeline.sh`) skips any client/date where the run directory already exists. Reruns without `--run-label` are safe — they will skip already-completed runs.

---

## 14. Troubleshooting

**Check timer status:**
```bash
systemctl status audios-natura-pipeline.timer
systemctl list-timers audios-natura-pipeline.timer
```

**View service logs (no PII — safe):**
```bash
journalctl -u audios-natura-pipeline.service -n 100
journalctl -u audios-natura-pipeline.service --since today
```

**View aggregate pipeline status (no PII):**
```bash
# pipeline.json contains only counts and status — no debtor names, phones, or audio.
python3 -m json.tool \
  /opt/audios-natura/shared/runs/maf/2026-05-14/pipe_maf_20260514/manifests/pipeline.json \
  | grep -E '"status"|"summary"|"schema_version"'
```

**Do NOT read `matched/` files** — they contain PII (debtor names, phones, debt data).

**Run logs (file-based, may contain phone numbers in filenames):**
```bash
# Safe: grep for non-PII lines only
grep -E 'RUN |SKIP |Done\.' /opt/audios-natura/logs/maf_2026-05-14_real.log
```

**Stop/disable timer:**
```bash
systemctl stop audios-natura-pipeline.timer
systemctl disable audios-natura-pipeline.timer
```

---

## 15. Security Constraints

- `ssh.InsecureIgnoreHostKey()` is **permanently forbidden** in this codebase.
- Secrets are mounted via Podman at `/run/secrets/` — never on `podman run` command lines.
- `SFTP_HOST_KEY` is pinned to the server's public key; the uploader fails closed if it doesn't match.
- `shared/runs/` contains PII: audio recordings, debtor names, phone numbers, debt balances. Treat as personal financial data. 30-day retention.
- Log filenames may embed phone numbers. Do not print log filenames. 60-day retention.
- `pipeline.json` and `manifests/` contain only aggregate counts — no PII. Safe to inspect.
- `:dev` image must never appear in production unit files.
- `--build` must never appear in the production service or timer.
- No test SFTP credentials in production.

---

## 16. Rootless Podman Hardening (Future)

The initial deployment uses rootful Podman (systemd runs as root). For hardening:

1. Create a dedicated system user: `useradd -r -s /sbin/nologin audios-natura`
2. Enable linger: `loginctl enable-linger audios-natura`
3. Move unit files to user scope: `~audios-natura/.config/systemd/user/`
4. Ensure Podman secrets are created under the `audios-natura` user context.
5. Ensure the release image is imported under the `audios-natura` user context.

Rootless requires both the pipeline container and the `work-netns` container (if used) to be in the same Podman user context. Since production does not use `work-netns`, rootless hardening is straightforward.

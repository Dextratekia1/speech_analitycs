# Release Checklist — audios-natura-v2

Use this checklist for each production release. Complete all steps in order.
Record the SHA7 of the deployed commit at the end.

---

## Pre-release checks (dev machine)

- [ ] Working tree is clean: `git status`
- [ ] On `main` branch and up to date: `git branch --show-current`
- [ ] Invariants pass: `bash scripts/check_invariants.sh` — expect 0 failures
- [ ] Orchestrator gates closed: `orchestrator gates-status` — all 4 gates closed
- [ ] Recent commit matches expected changes: `git log --oneline -5`
- [ ] No `:dev` in production service:
      `grep :dev deploy/audios-natura-pipeline.service` (expect no output)

---

## Build (dev machine)

```bash
bash scripts/build_release.sh
```

Expected output summary:
```
=== Build complete ===
  SHA7:    <sha7>
  Image:   localhost/audios-natura/pipeline-runner:git-<sha7>
  Release: localhost/audios-natura/pipeline-runner:release
  Archive: dist/pipeline-runner-git-<sha7>.tar.gz
```

Verify image:
```bash
podman images localhost/audios-natura/pipeline-runner
```

Confirm both `:git-<sha7>` and `:release` tags appear and point to the same image ID.

---

## Transfer to production host

```bash
scp dist/pipeline-runner-git-<sha7>.tar.gz production-host:/tmp/
```

Optional checksum verification:
```bash
sha256sum dist/pipeline-runner-git-<sha7>.tar.gz
# On production host, compare:
sha256sum /tmp/pipeline-runner-git-<sha7>.tar.gz
```

---

## Load on production host

**Run as `useraval` — rootless Podman, no sudo.**

```bash
podman load < /tmp/pipeline-runner-git-<sha7>.tar.gz
podman images localhost/audios-natura/pipeline-runner
```

> **Warning:** If loaded with `sudo podman load`, the `useraval` rootless service will **not** see the image. Always load as `useraval`.

Confirm `:release` tag is present and shows the new SHA. The `:git-<previous-sha7>` tag
from the prior release is retained automatically for rollback.

---

## Dry-run validation (production host)

```bash
PIPELINE_NETWORK_MODE=default \
PIPELINE_IMG=localhost/audios-natura/pipeline-runner:release \
/opt/audios-natura/scripts/run_pipeline.sh \
  --sftp-mode dry-run \
  --client all \
  --date "$(date +%F)"
```

Expected: all stages complete, `pipeline.json` written with status `ok`.

Review output:
```bash
python3 -m json.tool \
  /opt/audios-natura/shared/runs/<client>/<date>/<run_id>/manifests/pipeline.json \
  | grep -E '"status"|"summary"|"schema_version"'
```

`pipeline.json` contains only counts — no PII. `matched/` files must not be opened.

---

## Enable or restart timer

**Initial deployment (timer not yet enabled):**
```bash
systemctl --user enable --now audios-natura-pipeline.timer
systemctl --user enable --now audios-natura-cleanup.timer
```

**Subsequent releases:** No timer restart needed. The next scheduled run picks up the
new `:release` image automatically (`Type=oneshot`).

Verify:
```bash
systemctl --user list-timers audios-natura-pipeline.timer
```

---

## Rollback

If the new image causes issues, restore the previous build:
```bash
# On production host:
podman tag localhost/audios-natura/pipeline-runner:git-<previous-sha7> \
           localhost/audios-natura/pipeline-runner:release
podman images localhost/audios-natura/pipeline-runner
```

The next run uses the rolled-back image. No service restart needed.

---

## Post-release

- [ ] Note the deployed SHA7 here: `_______`
- [ ] Remove the archive from `/tmp/` on the production host:
      `rm /tmp/pipeline-runner-git-<sha7>.tar.gz`
- [ ] `dist/` is gitignored — do not commit it.
- [ ] Retain `dist/pipeline-runner-git-<sha7>.tar.gz` locally until the next
      release is confirmed stable (rollback source).

---

## Security reminders

- `scripts/build_release.sh` does not read secrets.
- `dist/` archives contain only the container image — no credentials, no PII.
- Never tag `:dev` as `:release`.
- Never add `--build` to the systemd service.
- `--allow-dirty` builds must not be used for production releases.
- See DEPLOY.md §15 for the full security constraint list.

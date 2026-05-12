# SECURITY.md — audios-analitycs

Security policies and handling requirements for this project.

---

## 1. Credentials — never commit real secrets

The files `secrets/mssql.env` and `secrets/sftp.env` contain real credentials
(MSSQL password, SFTP password, SFTP host key).  They are excluded from version
control via `.gitignore`.

Only the `*.env.example` files are tracked in git.  They contain placeholder
values only and serve as documentation of the expected environment variables.

**If you believe real credentials have been committed to a git remote:**
1. Rotate all affected credentials immediately (MSSQL password, SFTP password).
2. Revoke and regenerate the SFTP host key pair on the server if it was exposed.
3. Force-push a cleaned history using `git filter-repo` or similar, then notify
   all collaborators to re-clone.
4. Audit SFTP server and MSSQL server access logs for unauthorized activity.

---

## 2. Runtime artifacts — never commit `shared/runs/`

The `shared/runs/` directory is a runtime artifact store.  It contains:

- GSM and WAV audio recordings of debt-collection calls.
- Per-record JSON files (`matched/`) with: debtor names, phone numbers, debt
  balances, payment commitments, agent names, and timestamps.
- Prepared JSON payloads (`prepared/json/`) sent to the SFTP destination.

This data is **personal and financial PII**.  It must not enter version control,
be shared externally, or be stored beyond the operational retention period.

The directory is excluded via `.gitignore`.  The `shared/.gitkeep` file
preserves the directory structure without tracking its contents.

---

## 3. SFTP host key pinning

The `audio-uploader-go` service connects to an SFTP server to deliver processed
audio and JSON files.  Without host key verification, any host on the network
path can intercept the connection (MITM), receiving all uploaded audio and PII.

**Requirement:** `SFTP_HOST_KEY` must be set in `secrets/sftp.env` before
running in non-dry-run mode.  The value is the server's public host key in
OpenSSH `authorized_keys` format:

```
SFTP_HOST_KEY=ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... sftp-host
```

To obtain the correct value, run on a trusted network:
```bash
ssh-keyscan -t ed25519 <SFTP_HOST>
```
Copy the output line (excluding the hostname prefix) and set it as `SFTP_HOST_KEY`.

The uploader will fail closed (exit non-zero) if `SFTP_HOST_KEY` is missing or
unparseable when not in dry-run mode.  The dry-run path does not connect to SFTP
and does not require this variable.

`ssh.InsecureIgnoreHostKey()` is permanently forbidden in this codebase.

---

## 4. Podman `/run/secrets/` runtime model

Secrets are never passed as environment variables directly on the `podman run`
command line (which would be visible in `ps` output).  Instead they are injected
as Podman secrets:

```bash
podman secret create mssql-env-v2 ./secrets/mssql.env
podman secret create sftp-env     ./secrets/sftp.env
```

Inside the container, secrets are mounted as files:
- `/run/secrets/mssql-env-v2` — read by `metadata-matcher-rs` and `pipeline-runner`
- `/run/secrets/sftp-env` — read by `audio-uploader-go` and `pipeline-runner`

The services parse these files as `KEY=VALUE` pairs.  Secret values are never
set into the process environment via `os.Setenv()` in production paths.

---

## 5. PII in `shared/runs/`

Every processed audio run produces files containing personal data:

| File | PII present |
|---|---|
| `raw/*.gsm` | Audio recording of a real person |
| `wav/*.wav` | Audio recording of a real person |
| `matched/*.json` | Debtor name, phone number, debt balance, payment date, agent name |
| `prepared/json/*.json` | Subset of matched JSON — same PII |
| `manifests/upload.json` | Record IDs, skip reasons (no direct PII but linkable) |

Operational retention policy (to be defined by the data owner):
- Audio files should be deleted after successful upload and confirmation.
- JSON artifacts should be purged after the downstream system has ingested them.
- Logs in `logs/` contain filenames that embed phone numbers — treat as PII.

---

## 6. Credential rotation checklist

If there is any reason to believe credentials may have been exposed (accidental
git push, screenshot shared, log file published, etc.):

- [ ] Change `MSSQL_PASSWORD` in the database server and update `secrets/mssql.env`.
- [ ] Change `SFTP_PASSWORD` on the SFTP server and update `secrets/sftp.env`.
- [ ] Re-run `ssh-keyscan` and update `SFTP_HOST_KEY` in `secrets/sftp.env`
      if the server host key was regenerated.
- [ ] Recreate Podman secrets:
      ```bash
      podman secret rm mssql-env-v2 sftp-env
      podman secret create mssql-env-v2 ./secrets/mssql.env
      podman secret create sftp-env     ./secrets/sftp.env
      ```
- [ ] Audit MSSQL and SFTP server access logs.
- [ ] If committed to git remote: clean history and force-push.

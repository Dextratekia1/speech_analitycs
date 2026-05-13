# CLAUDE.md — audios-analitycs

This file is the base contract for all AI agents working on this repository.
Read it completely before implementing anything.

---

## Project purpose

Multi-client audio processing pipeline that:

1. Downloads GSM audio files from an HTTP directory listing (internal network).
2. Converts them to WAV using ffmpeg.
3. Matches each audio to a debt-collection record in MSSQL via batch SQL.
4. Uploads the matched JSON + WAV pair to a downstream SFTP server.

Current clients: `natura` (id=52), `maf` (id=59).

---

## Runtime model — Podman containers

The project runs **inside Podman containers**. This is the authoritative runtime.

- `podman-compose.yml` defines the service/container topology.
- `podman-compose.override.yml` sets `network_mode: container:work-netns`
  (VPN namespace sharing — required for access to internal MSSQL/HTTP/SFTP servers).
- Secrets are injected as Podman secrets mounted at `/run/secrets/` inside containers.
  - `/run/secrets/mssql-env-v2` — MSSQL credentials
  - `/run/secrets/sftp-env` — SFTP credentials + host key
- The `./shared` directory is bind-mounted as `/shared` inside containers.

**Never replace the Podman-based workflow with a host-only workflow.**

---

## Pipeline flow

```
audio-fetcher-rs  → raw/*.gsm        → manifests/fetch.json
audio-converter-rs → wav/*.wav       → manifests/convert.json
metadata-matcher-rs → matched/*.json → manifests/match.json
audio-uploader-go  → prepared/json/  → manifests/upload.json
                                          → SFTP server
```

The `pipeline-runner` binary orchestrates all four stages in sequence
by invoking them as subprocesses. Each stage reads the previous stage's
manifest from `manifests/`.

Runtime layout: `/shared/runs/{client}/{date}/{run_id}/`
- `raw/` — downloaded GSM files
- `wav/` — converted WAV files
- `matched/` — rich per-record JSON (PII: names, phones, financial data)
- `prepared/json/` — trimmed outgoing JSON (what gets SFTPed)
- `manifests/` — fetch.json, convert.json, match.json, upload.json

Config: `/shared/config/clients/{client}.yml`

---

## Services

| Service | Language | Role |
|---|---|---|
| `audio-fetcher-rs` | Rust (blocking) | HTTP scrape + download |
| `audio-converter-rs` | Rust (blocking) | ffmpeg + ffprobe |
| `metadata-matcher-rs` | Rust (async/tokio) | MSSQL batch lookup |
| `audio-uploader-go` | Go | Validate + SFTP upload |
| `pipeline-runner` | Rust | Orchestrator (subprocess runner) |
| `crates/common` | Rust lib | Shared types, paths, util |

---

## Mandatory rules for all agents

### General
- Read this file completely before starting any task.
- Implement only what the authorized phase/task specifies.
- Do not refactor, clean up, or improve code outside the authorized scope.
- Do not add features not requested.
- Do not remove or bypass `--dry-run` guards.
- End every task with the mandatory closure report (see below).

### Containerfiles and compose files
- Do not modify `Containerfile`, `podman-compose.yml`, or
  `podman-compose.override.yml` unless the authorized task explicitly requires it.
- All container builds use `podman build` / `podman-compose build`, not Docker.

### Dependencies
- Do not run `cargo update`, `cargo add`, `go get`, or `go mod tidy` unless
  explicitly authorized by the current phase.
- Do not run `cargo install` or any command that installs to the system.

### Secrets and PII
- **Never open, print, read, edit, copy, or transform `secrets/mssql.env`
  or `secrets/sftp.env`.**  These files contain real credentials.
- Only `secrets/*.env.example` files may be read or modified.
- Do not print secret values in error messages, logs, or code comments.
- `shared/runs/` contains runtime PII (debtor names, phone numbers, debt data,
  agent names). Do not read individual record files unless strictly required
  by the authorized task. Never commit or print PII.
- Do not stage, commit, or push `logs/`, audio files (`*.gsm`, `*.wav`), zip
  snapshots, or any `shared/runs/` runtime artifacts.
- Do not use `shared/runs/` real data as test fixtures.

### SFTP host key
- `ssh.InsecureIgnoreHostKey()` is **forbidden**.
- All SFTP connections must use `ssh.FixedHostKey()` with a key loaded from
  `SFTP_HOST_KEY` (OpenSSH authorized_keys format, from the sftp-env secret).
- The non-dry-run path must fail closed if `SFTP_HOST_KEY` is missing or invalid.
- The dry-run path must not require `SFTP_HOST_KEY`.
- SFTP credentials must not be placed in process-global environment via `os.Setenv`.
- SFTP config must flow through an explicit `SFTPConfig` struct (`audio-uploader-go`).

### Validation model — container-first

**All development validation for this project is container-first.**

- The authoritative build and test environment is the Podman container, not
  the host machine.
- Do not use host-installed Rust toolchains, Go toolchains, LSP servers,
  cargo plugins, or any project language tooling as the source of truth for
  correctness, compilation, or test results.
- Do not install Go, Rust, language servers, or project dependencies on the host.
- `Cargo.lock` and `go.sum` presence or absence on the host filesystem is not
  by itself meaningful — their significance depends on whether the Containerfiles
  copy them into the build context. Verify through the containerized build path.
- Build, test, and validation commands must be run through `podman build` /
  `podman run` only when the current phase explicitly authorizes container
  execution. If container execution is not authorized, report findings as
  "requires container validation" rather than running host tooling.
- Do not run `podman build`, `podman run`, or `podman-compose` unless the
  current phase explicitly authorizes it.

### Build and test
- Do not run the live pipeline (fetcher/converter/matcher/uploader) against
  real MSSQL, SFTP, or HTTP audio servers.
- Do not create commits or push unless explicitly authorized.

### MSSQL TLS
- `MSSQL_TRUST_CERT` defaults to `false`; unrecognized values also resolve to `false`.
- Do not change `MSSQL_ENCRYPT` behavior without explicit authorization.
- CA-file certificate validation is not implemented; do not imply or add it.

### Test harnesses
- Rust tests run through `Containerfile.test-rust` (covers `audios_common` + `metadata-matcher-rs`).
- Go tests run through `Containerfile.test-go` (covers `audio-uploader-go`).
- All test fixtures must be synthetic. No real PII, real phone numbers, real debt
  values, real names, real audio recordings, or data from `shared/runs/`.

### Uploader validation
- `descrip_rpta` values equal to `"OTRO"` after `strings.TrimSpace` must be rejected.
- For MAF, `placa` null/missing/empty must not block upload; it is emitted as `""`.
- All other MAF required fields remain required unless a future phase explicitly changes them.

### Pipeline-runner forwarding
- `--clients-dir` is accepted by `pipeline-runner` and forwarded only to
  `audio-fetcher-rs` and `metadata-matcher-rs`.
- Do not forward `--clients-dir` to `audio-converter-rs` or `audio-uploader-go` (unsupported).
- Do not add `--resume`, `--force`, `pipeline.json`, manifest schema changes, retry
  logic, or new exit codes unless explicitly authorized.

---

## Forbidden commands (always)

```
sudo
podman build / podman run / podman-compose build / podman-compose run
cargo update / cargo install / cargo add
go get / go mod tidy (unless explicitly authorized)
git commit / git push
```

---

## Mandatory closure report format

Every task must end with:

```
1. Decision executed
2. Files created/modified
3. Commands executed
4. Test results
5. Risks or deviations
6. What was deliberately NOT implemented
7. Recommended next step
8. Confirmation of no advancement without authorization
```

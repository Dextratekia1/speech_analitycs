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
- `ssh.ClientConfig.HostKeyAlgorithms` must be pinned to `[]string{pubKey.Type()}` where
  `pubKey` is the result of parsing `SFTP_HOST_KEY` via `ssh.ParseAuthorizedKey`.
- SFTP host key algorithm negotiation must follow the `SFTP_HOST_KEY` key type; it must
  not depend on `crypto/ssh` default algorithm ordering.
- The server must present a key matching both the algorithm type and the key value of the
  pinned `SFTP_HOST_KEY`; if either does not match, the connection must fail closed.
- Do not add fallback host key algorithms unless explicitly authorized by a future phase.

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

### Operational network namespace invariants

- Real pipeline runs requiring VPN access (MSSQL, HTTP audio source, SFTP destination)
  must use direct `podman run --network container:work-netns`; bare `podman-compose run`
  is not sufficient for real runs.
- `work-netns` must already be running before any non-dry-run pipeline execution.
- `work-netns` is external operational infrastructure; this repository does not manage
  its lifecycle or the VPN.
- `scripts/run_range.sh` is the authoritative operational script for real runs; it passes
  `--network container:work-netns` and hard-fails if `work-netns` is not running.
- `scripts/run_rangev2.sh` is deprecated and must not be used for operational runs.
- `podman-compose.override.yml` must reference `container:work-netns` for all services.
- `container:container-vpn` is a stale name; it must not appear in active configurations.
- In this environment, podman-compose 1.5.0 does not auto-merge the override file.
  Compose-based commands must always pass:
  `-f podman-compose.yml -f podman-compose.override.yml`
- Before using compose for any real run, verify effective network config with:
  `podman-compose -f podman-compose.yml -f podman-compose.override.yml config`
  — the output must show `network_mode: container:work-netns` for all pipeline services.

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

### Upload report invariants
- `upload.json` `schema_version` is `2`; incrementing requires explicit authorization.
- `UploadItem.status` must use only the defined constants: `sent`, `prepared`,
  `skipped_parse`, `skipped_validation`, `skipped_prepare`, `send_error`.
- `UploadItem.error_code` must use only the constants defined in `main.go`
  (empty string `""` represents success/no error).
- `reasonToErrorCode` is the single translation point from reason strings to
  `error_code`; do not inline reason→error_code mappings at call sites.
- `upload.json` must not add PII fields beyond the filename/path-derived fields
  already present: `record_id`, `json_in`, `json_out`, `wav_path`.
- `UploadCounts` JSON field names must remain snake_case.

### Pipeline-runner forwarding
- `--clients-dir` is accepted by `pipeline-runner` and forwarded only to
  `audio-fetcher-rs` and `metadata-matcher-rs`.
- Do not forward `--clients-dir` to `audio-converter-rs` or `audio-uploader-go` (unsupported).
- Do not add `--resume`, `--force`, `pipeline.json`, manifest schema changes, retry
  logic, or new exit codes unless explicitly authorized.

### Pipeline aggregation invariants

- `pipeline.json` `schema_version` is `1`; incrementing requires explicit authorization.
- `pipeline.json` is always written before `pipeline-runner` exits, whether the pipeline
  succeeded or failed.
- `PipelineStage.status` must use only: `pending`, `ok`, `failed`, `skipped`.
- `PipelineReport.status` must use only: `ok`, `failed`, `partial`.
- `run_dir=<path>` is printed to stdout only on full pipeline success or `partial` status (all stages exit 0).
- `PipelineReport.status` is `partial` when all stages succeed (exit 0) but `upload_send_error > 0` in `upload.json.counts`; exit code is 0; `run_dir=` is printed.
- `partial` is not produced by: `upload_send_error = 0`, zero/null/non-numeric `upload_send_error`, `skipped_parse > 0`, `skipped_validation > 0`, `skipped_prepare > 0`, or missing/invalid `upload.json`.
- Stage failure takes precedence over `partial`: if any stage exits non-zero, `status` is `failed`, not `partial`.
- `partial` appends exactly one warning to `pipeline.json.warnings`: `"upload partial success: upload_send_error > 0"`.
- `partial` does not change process exit behavior; a non-zero partial exit code is not implemented and requires explicit authorization.
- Upload-stage counts aggregation into `pipeline.json` is non-fatal. If `upload.json` is
  missing, unreadable, invalid JSON, missing the `counts` key, or `counts` is not a JSON
  object: `PipelineStage.counts` for the upload stage remains `null`, exactly one concise
  non-secret warning is appended to `pipeline.json.warnings`, and pipeline exit behavior
  is unchanged.
- `PipelineReport.summary` is populated from `upload.json.counts` only when `counts` is a
  valid JSON object; it is `{}` otherwise.
- When populated, `summary` contains exactly:
  `upload_total`, `upload_sent_ok`, `upload_skipped_parse`, `upload_skipped_validation`,
  `upload_skipped_prepare`, `upload_send_error`.
- All `summary` values are numeric integers. Non-numeric values in `upload.json.counts`
  fields are treated as `0`; individual field coercion failures do not produce warnings.
- `summary` must not include record IDs, file paths, debtor data, agent data, secrets,
  or any PII sourced from `upload.json`.
- `PipelineStage.counts` is currently populated only for the upload stage.
  Fetch, convert, and match stage counts remain `null` until a future explicitly
  authorized phase implements those aggregations.

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

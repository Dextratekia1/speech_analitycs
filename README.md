# audios-natura-v2

Proyecto base (desde cero) para pipeline multi-cliente con layout `/shared/runs/`.

Servicios incluidos:
- `audio-fetcher-rs` (Rust): descarga audios desde HTTP directory listing.
- `audio-converter-rs` (Rust): convierte a WAV usando ffmpeg.
- `metadata-matcher-rs` (Rust): batch lookup a MSSQL + genera JSON por audio.
- `audio-uploader-go` (Go): sube JSON+WAV por SFTP.
- `pipeline-runner` (Rust): orquesta las 4 etapas dentro de un mismo contenedor.

## Layout
Se monta `./shared` como `/shared` dentro de contenedores.

Ejecución:
`/shared/runs/{client}/{date}/{run_id}/`
- `raw/` descargas
- `wav/` conversiones
- `matched/` json
- `manifests/` manifests

## Config por cliente
- `/shared/config/clients/natura.yml`
- `/shared/config/clients/maf.yml`
- Tail SQL:
  - `/shared/config/natura/batch_lookup.yml`
  - `/shared/config/maf/batch_lookup.yml`

## Dev (Arch + podman-compose + netns)
Build:
```bash
sudo podman-compose -f podman-compose.yml -f podman-compose.override.yml build
```

Run pipeline (dry-run):
```bash
sudo podman-compose -f podman-compose.yml -f podman-compose.override.yml \
  run --rm pipeline-runner --client natura --date 2026-01-08 --dry-run
```

Quitar `--dry-run` para activar MSSQL/SFTP (crear secrets externos):
```bash
sudo podman secret create mssql-env ./secrets/mssql.env
sudo podman secret create sftp-env  ./secrets/sftp.env
```

## Notas
- `--date` formato `yyyy-mm-dd`.
- Tipo 1 usa ventana ±600s.
- El matcher corre 2 queries: carteras del día (`carteras_sql`) y luego el batch con `IN ({{CARTERAS_LIST}})`.

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
- `raw/` — archivos GSM descargados
- `wav/` — archivos WAV convertidos
- `matched/` — JSON enriquecido por registro (contiene PII)
- `prepared/json/` — JSON recortado preparado para subida por SFTP
- `manifests/` — manifests de etapa: fetch.json, convert.json, match.json, upload.json
- `pipeline.json` — reporte de ejecución escrito por pipeline-runner

## Config por cliente
- `/shared/config/clients/natura.yml`
- `/shared/config/clients/maf.yml`
- Tail SQL:
  - `/shared/config/natura/batch_lookup.yml`
  - `/shared/config/maf/batch_lookup.yml`

## Reporte de ejecución: pipeline.json

`pipeline-runner` escribe `pipeline.json` en el directorio del run al finalizar, tanto en
caso de éxito como de fallo. Contiene:

- `schema_version: 1`
- Estado global del pipeline: `ok`, `failed`, o `partial`.
- Estado por etapa: `ok`, `failed`, `skipped`.
- Resumen de conteos: `fetch_total`, `convert_total`, `match_total`, `upload_sent_ok`,
  `upload_send_error`, etc.
- `stderr_tail`: fragmento final del stderr capturado para etapas que fallan con stderr
  no vacío (limitado a 2048 bytes). Nulo para etapas exitosas o sin stderr.

`pipeline.json` no contiene PII de registros individuales. Los valores del resumen son
conteos numéricos.

### Estado del pipeline

| Status | Significado |
|---|---|
| `ok` | Todas las etapas completaron exitosamente. |
| `failed` | Al menos una etapa salió con error (exit code ≠ 0). |
| `partial` | Todas las etapas salieron con exit 0, pero `upload_send_error > 0`. |

- `partial` imprime `run_dir=` y retorna exit 0.
- `partial` es controlado únicamente por `upload_send_error > 0` en el reporte de upload.
- Una falla de etapa (exit code ≠ 0) toma precedencia sobre `partial`.

## Red VPN — prerequisito para ejecuciones reales

Las ejecuciones reales (non-dry-run) requieren acceso a servidores internos (MSSQL,
HTTP de audios, SFTP de destino). El acceso se obtiene a través del contenedor de
namespace de red `work-netns`.

**`work-netns` debe estar corriendo antes de ejecutar el pipeline en modo real.**

`work-netns` es infraestructura operativa externa. Este repositorio no gestiona su
ciclo de vida ni el de la VPN. No se levanta desde los archivos de compose de este
proyecto.

Para verificar que esté corriendo:
```bash
podman inspect -f '{{.State.Running}}' work-netns
```

## Ejecución operativa (runs reales)

El script autorizado para ejecuciones operativas es:

```bash
scripts/run_range.sh
```

Utiliza `podman run` directo con `--network container:work-netns`, verifica que
`work-netns` esté corriendo antes de proceder, y soporta rangos de fechas y múltiples
clientes.

Variables de control (todas opcionales con defaults):
```bash
START=2026-05-13 END=2026-05-13 MODE=full scripts/run_range.sh
```
- `MODE=full` — ejecuta el pipeline completo (fetcher + converter + matcher + uploader).
- `MODE=match` — ejecuta solo fetcher + converter + matcher (sin SFTP).
- `BUILD=1` — reconstruye imágenes antes de ejecutar.
- `NET_MODE` — sobreescribe el namespace de red (default: `container:work-netns`).

`scripts/run_rangev2.sh` está obsoleto (deprecated) y no debe usarse para ejecuciones
operativas. Usar únicamente `scripts/run_range.sh`.

## Comportamiento de skip y recuperación de runs fallidos

`scripts/run_range.sh` salta un client/fecha automáticamente si el directorio del run ya
existe en `shared/runs/{client}/{date}/{run_id}/`. El script imprime
`SKIP {client} {date} (exists: {run_dir})` y continúa al siguiente. Esto evita
sobreescribir artefactos de un run previo.

Si un run anterior falló después de crear el directorio, el reintento para el mismo
client/fecha quedará saltado hasta que el operador intervenga.

**Para reintentar un run fallido o incompleto:**

1. Revisar `pipeline.json` en el directorio del run para identificar la etapa fallida
   y el motivo.
2. Confirmar que el directorio corresponde únicamente al run fallido o incompleto que
   debe descartarse.
3. Mover o eliminar el directorio según las reglas de retención y manejo de datos del
   responsable. `shared/runs/` puede contener PII (nombres, teléfonos, datos de deuda)
   y grabaciones de audio.
4. Volver a ejecutar `scripts/run_range.sh` con el rango de fechas correspondiente.

No leer archivos individuales de `matched/` ni grabaciones de audio como paso de
diagnóstico estándar.

## Dev / dry-run con podman-compose

Para builds y dry-runs de desarrollo, podman-compose puede usarse con flags explícitos:

```bash
podman-compose -f podman-compose.yml -f podman-compose.override.yml build
```

```bash
podman-compose -f podman-compose.yml -f podman-compose.override.yml \
  run --rm pipeline-runner --client natura --date 2026-01-08 --dry-run
```

**Nota importante sobre el override:**
- `podman-compose 1.5.0` (entorno actual) **no fusiona automáticamente**
  `podman-compose.override.yml` al invocar `podman-compose config` o
  `podman-compose run` sin flags `-f`.
- Siempre pasar `-f podman-compose.yml -f podman-compose.override.yml` para que
  el override se aplique.
- `podman-compose run` **no debe usarse para ejecuciones reales** a menos que se
  verifique previamente que el config efectivo incluye
  `network_mode: container:work-netns`. Verificación:
  ```bash
  podman-compose -f podman-compose.yml -f podman-compose.override.yml config \
    | grep network_mode
  ```

Para secrets externos (requeridos en modo real):
```bash
podman secret create mssql-env-v2 ./secrets/mssql.env
podman secret create sftp-env     ./secrets/sftp.env
```

## Notas
- `--date` formato `yyyy-mm-dd`.
- Tipo 1 usa ventana ±600s.
- El matcher corre 2 queries: carteras del día (`carteras_sql`) y luego el batch con `IN ({{CARTERAS_LIST}})`.

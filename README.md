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
- Estado por etapa: `pending`, `ok`, `failed`, `skipped`.
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

### Estado por etapa

Cada etapa individual (`fetch`, `convert`, `match`, `upload`) registra uno de los
siguientes estados en `pipeline.json`. Estos valores son distintos del estado global
del pipeline documentado arriba.

| Status | Significado |
|---|---|
| `pending` | Etapa inicializada pero no ejecutada; aparece cuando una etapa anterior falló antes de que esta pudiera comenzar. |
| `ok` | Etapa completó exitosamente (exit code 0). |
| `failed` | Etapa salió con error (exit code ≠ 0). |
| `skipped` | Etapa no ejecutó debido a la falla de una etapa anterior. |

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

## Ejecución operativa — scripts/run_pipeline.sh

El script moderno para ejecuciones operativas y de prueba es:

```bash
scripts/run_pipeline.sh --sftp-mode <real|test|dry-run> [OPTIONS]
```

### Modos SFTP

| Modo | Descripción |
|---|---|
| `real` | SFTP productivo. Requiere `work-netns` corriendo y el Podman secret `sftp-env`. |
| `test` | SFTP con credenciales sintéticas. Requiere `--test-sftp-env <ruta>`. Rechaza rutas de secretos reales. |
| `dry-run` | Sin SFTP. No descarga, no convierte, no sube. Genera manifests y JSON preparado. |

### Ejemplos

```bash
# Dry-run para una fecha:
scripts/run_pipeline.sh --sftp-mode dry-run --client natura --date 2026-05-14

# Test SFTP con credenciales sintéticas:
scripts/run_pipeline.sh --sftp-mode test --test-sftp-env /tmp/test-sftp.env \
  --client maf --date 2026-05-14

# Run productivo (requiere work-netns y sftp-env secret):
scripts/run_pipeline.sh --sftp-mode real --client all \
  --start 2026-05-01 --end 2026-05-14

# Reconstruir imágenes antes de ejecutar:
scripts/run_pipeline.sh --sftp-mode dry-run --date 2026-05-14 --build
```

### Opciones principales

```
--sftp-mode <real|test|dry-run>     Modo SFTP (requerido).
--client <maf|natura|all>           Cliente(s). Default: all.
--date YYYY-MM-DD                   Fecha única.
--start / --end YYYY-MM-DD          Rango de fechas (inclusivo, mutualmente exclusivo con --date).
--mode <full|fetch|convert|match|upload>   Etapas a ejecutar. Default: full.
--build                             Reconstruir imágenes antes de ejecutar.
--test-sftp-env <ruta>              Archivo de credenciales SFTP sintéticas (requerido con --sftp-mode test).
--conversion-concurrency <N>        Override de concurrencia de audio-converter-rs (default: 2). N ≥ 1.
--help                              Mostrar ayuda.
```

Ver `scripts/run_pipeline.sh --help` para la referencia completa.

## Servidor SFTP de prueba — scripts/start_test_sftp.sh

Para ejecutar el pipeline en modo `--sftp-mode test` se requiere un servidor SFTP
sintético. El harness de prueba levanta un contenedor con credenciales y host key
generados en el momento, aislado de producción.

### Inicio

```bash
# Construir la imagen (solo la primera vez, o si hubo cambios):
TEST_ENV=$(bash scripts/start_test_sftp.sh --build)

# Si la imagen ya existe:
TEST_ENV=$(bash scripts/start_test_sftp.sh)
```

La variable `TEST_ENV` contiene la ruta al archivo de credenciales sintéticas
(por ejemplo `/tmp/audios-test-sftp-XXXXXX/sftp.env`). No se imprimen los
contenidos del archivo.

### Ejecutar pipeline en modo test

```bash
scripts/run_pipeline.sh \
  --sftp-mode test \
  --test-sftp-env "$TEST_ENV" \
  --client all \
  --date 2026-05-14
```

### Detener y limpiar

```bash
bash scripts/stop_test_sftp.sh --cleanup-env "$TEST_ENV"
```

### Advertencia de seguridad

**Nunca** usar `secrets/sftp.env` ni `/run/secrets/sftp-env` como valor de
`--test-sftp-env`. El script `run_pipeline.sh` rechaza estas rutas
explícitamente. El archivo de credenciales de test debe ser sintético y
generado por `scripts/start_test_sftp.sh`.

## Ejecución operativa — scripts/run_range.sh (legacy)

`scripts/run_range.sh` es el script heredado. Soporta `MODE=full` y `MODE=match`
vía variables de entorno:

```bash
START=2026-05-13 END=2026-05-13 MODE=full scripts/run_range.sh
```

`scripts/run_rangev2.sh` está obsoleto (deprecated) y no debe usarse para ejecuciones
operativas. Usar `scripts/run_pipeline.sh` o `scripts/run_range.sh`.

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

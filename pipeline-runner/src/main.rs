use anyhow::{Context, Result};
use audios_common::{paths, util};
use chrono::Utc;
use clap::Parser;
use serde::Serialize;
use serde_json::Value;
use std::time::Instant;
use std::{fs, path::{Path, PathBuf}, process::Command};

// Pipeline-level status values.
const PIPELINE_STATUS_OK: &str = "ok";
const PIPELINE_STATUS_FAILED: &str = "failed";

// Stage-level status values.
const STAGE_STATUS_PENDING: &str = "pending";
const STAGE_STATUS_OK: &str = "ok";
const STAGE_STATUS_FAILED: &str = "failed";
const STAGE_STATUS_SKIPPED: &str = "skipped";

#[derive(Serialize)]
struct PipelineStage {
    name: String,
    command: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    duration_ms: Option<i64>,
    exit_code: Option<i32>,
    status: String,
    manifest_path: String,
    counts: Option<Value>,
    error: Option<String>,
    stderr_tail: Option<String>,
}

#[derive(Serialize)]
struct PipelineReport {
    schema_version: u32,
    client: String,
    date: String,
    run_id: String,
    dry_run: bool,
    shared_root: String,
    clients_dir: String,
    run_dir: String,
    started_at: String,
    finished_at: String,
    duration_ms: i64,
    status: String,
    failed_stage: Option<String>,
    exit_code: i32,
    stages: Vec<PipelineStage>,
    summary: Value,
    warnings: Vec<String>,
    error: Option<String>,
}

#[derive(Parser, Debug)]
#[command(name = "pipeline-runner")]
struct Args {
    #[arg(long)]
    client: String,

    #[arg(long)]
    date: String,

    #[arg(long, default_value = "/shared")]
    shared_root: String,

    #[arg(long)]
    run_id: Option<String>,

    #[arg(long, default_value_t = false)]
    dry_run: bool,

    #[arg(long, default_value = "config/clients")]
    clients_dir: String,
}

fn make_pending_stage(name: &str, command: &str, manifest_path: &str) -> PipelineStage {
    PipelineStage {
        name: name.to_string(),
        command: command.to_string(),
        started_at: None,
        finished_at: None,
        duration_ms: None,
        exit_code: None,
        status: STAGE_STATUS_PENDING.to_string(),
        manifest_path: manifest_path.to_string(),
        counts: None,
        error: None,
        stderr_tail: None,
    }
}

fn make_skipped_stage(name: &str, command: &str, manifest_path: &str) -> PipelineStage {
    PipelineStage {
        name: name.to_string(),
        command: command.to_string(),
        started_at: None,
        finished_at: None,
        duration_ms: None,
        exit_code: None,
        status: STAGE_STATUS_SKIPPED.to_string(),
        manifest_path: manifest_path.to_string(),
        counts: None,
        error: None,
        stderr_tail: None,
    }
}

// Runs cmd and populates stage timing/status. Returns true on success.
fn run_stage(mut cmd: Command, stage: &mut PipelineStage) -> bool {
    stage.started_at = Some(Utc::now().to_rfc3339());
    let t0 = Instant::now();
    match cmd.status() {
        Err(e) => {
            stage.finished_at = Some(Utc::now().to_rfc3339());
            stage.duration_ms = Some(t0.elapsed().as_millis() as i64);
            stage.status = STAGE_STATUS_FAILED.to_string();
            stage.error = Some(format!("spawn {}: {e}", stage.command));
            false
        }
        Ok(status) => {
            stage.finished_at = Some(Utc::now().to_rfc3339());
            stage.duration_ms = Some(t0.elapsed().as_millis() as i64);
            stage.exit_code = status.code();
            if status.success() {
                stage.status = STAGE_STATUS_OK.to_string();
                true
            } else {
                stage.status = STAGE_STATUS_FAILED.to_string();
                stage.error = Some(format!("{} exited: {status}", stage.command));
                false
            }
        }
    }
}

fn write_pipeline_json(report: &PipelineReport, manifest_dir: &PathBuf) -> Result<()> {
    fs::create_dir_all(manifest_dir)
        .with_context(|| format!("creating {}", manifest_dir.display()))?;
    let path = manifest_dir.join("pipeline.json");
    let bytes = serde_json::to_vec_pretty(report).context("serializing pipeline.json")?;
    fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// Reads upload.json and extracts the top-level "counts" object.
// Returns (Some(counts_value), None) on success.
// Returns (None, Some(warning)) on any failure; never panics and never fails the pipeline.
fn read_upload_counts(path: &Path) -> (Option<Value>, Option<String>) {
    let bytes = match fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (None, Some("upload manifest missing".to_string()));
        }
        Err(e) => {
            return (None, Some(format!("upload manifest read failed: {e}")));
        }
        Ok(b) => b,
    };
    let doc: Value = match serde_json::from_slice(&bytes) {
        Err(_) => return (None, Some("upload manifest parse failed".to_string())),
        Ok(v) => v,
    };
    match doc.get("counts") {
        None => (None, Some("upload manifest counts missing".to_string())),
        Some(c) if c.is_object() => (Some(c.clone()), None),
        Some(_) => (None, Some("upload manifest counts not object".to_string())),
    }
}

// Builds the pipeline-level summary object from upload counts.
// Maps upload.json counts fields to upload_* summary keys.
// Missing or non-numeric fields default to 0 without error.
fn summarize_upload_counts(counts: Option<&Value>) -> Value {
    let Some(c) = counts else {
        return Value::Object(serde_json::Map::new());
    };
    let n = |key: &str| -> u64 { c.get(key).and_then(|v| v.as_u64()).unwrap_or(0) };
    serde_json::json!({
        "upload_total":              n("total"),
        "upload_sent_ok":            n("sent_ok"),
        "upload_skipped_parse":      n("skipped_parse"),
        "upload_skipped_validation": n("skipped_validation"),
        "upload_skipped_prepare":    n("skipped_prepare"),
        "upload_send_error":         n("send_error"),
    })
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    util::parse_date_ymd(&args.date)?;

    let run_id = args
        .run_id
        .unwrap_or_else(|| format!("{}", Utc::now().format("%Y%m%dT%H%M%SZ")));

    let shared_root = PathBuf::from(&args.shared_root);
    let run_dir = paths::run_dir(
        &shared_root.join("runs"),
        &args.client,
        &args.date,
        &run_id,
    );
    fs::create_dir_all(&run_dir)?;

    let manifest_dir = run_dir.join("manifests");
    let pipeline_t0 = Instant::now();
    let pipeline_started_at = Utc::now().to_rfc3339();

    let mut stages: Vec<PipelineStage> = Vec::new();
    let mut failed = false;
    let mut report_failed_stage: Option<String> = None;
    let mut report_status = PIPELINE_STATUS_OK.to_string();
    let mut report_exit_code: i32 = 0;
    let mut report_error: Option<String> = None;
    let mut report_summary: Value = Value::Object(serde_json::Map::new());
    let mut report_warnings: Vec<String> = Vec::new();

    // 1) fetch
    {
        let mut c = Command::new("audio-fetcher-rs");
        c.args([
            "--client",
            &args.client,
            "--date",
            &args.date,
            "--shared-root",
            &args.shared_root,
            "--run-id",
            &run_id,
            "--clients-dir",
            &args.clients_dir,
        ]);
        if args.dry_run {
            c.arg("--dry-run");
        }
        let mut st = make_pending_stage("fetch", "audio-fetcher-rs", "manifests/fetch.json");
        if !run_stage(c, &mut st) {
            failed = true;
            report_failed_stage = Some("fetch".to_string());
            report_status = PIPELINE_STATUS_FAILED.to_string();
            report_exit_code = 1;
            report_error = st.error.clone();
        }
        stages.push(st);
    }

    // 2) convert
    let stage_convert = if failed {
        make_skipped_stage("convert", "audio-converter-rs", "manifests/convert.json")
    } else {
        let mut c = Command::new("audio-converter-rs");
        c.args([
            "--client",
            &args.client,
            "--date",
            &args.date,
            "--shared-root",
            &args.shared_root,
            "--run-id",
            &run_id,
        ]);
        if args.dry_run {
            c.arg("--dry-run");
        }
        let mut st =
            make_pending_stage("convert", "audio-converter-rs", "manifests/convert.json");
        if !run_stage(c, &mut st) {
            failed = true;
            report_failed_stage = Some("convert".to_string());
            report_status = PIPELINE_STATUS_FAILED.to_string();
            report_exit_code = 1;
            report_error = st.error.clone();
        }
        st
    };
    stages.push(stage_convert);

    // 3) match
    let stage_match = if failed {
        make_skipped_stage("match", "metadata-matcher-rs", "manifests/match.json")
    } else {
        let mut c = Command::new("metadata-matcher-rs");
        c.args([
            "--client",
            &args.client,
            "--date",
            &args.date,
            "--shared-root",
            &args.shared_root,
            "--run-id",
            &run_id,
            "--clients-dir",
            &args.clients_dir,
        ]);
        if args.dry_run {
            c.arg("--dry-run");
        }
        let mut st = make_pending_stage("match", "metadata-matcher-rs", "manifests/match.json");
        if !run_stage(c, &mut st) {
            failed = true;
            report_failed_stage = Some("match".to_string());
            report_status = PIPELINE_STATUS_FAILED.to_string();
            report_exit_code = 1;
            report_error = st.error.clone();
        }
        st
    };
    stages.push(stage_match);

    // 4) upload
    let upload_was_skipped = failed;
    let stage_upload = if failed {
        make_skipped_stage("upload", "audio-uploader-go", "manifests/upload.json")
    } else {
        let mut c = Command::new("audio-uploader-go");
        c.args([
            "--client",
            &args.client,
            "--date",
            &args.date,
            "--shared-root",
            &args.shared_root,
            "--run-id",
            &run_id,
        ]);
        if args.dry_run {
            c.arg("--dry-run");
        }
        let mut st = make_pending_stage("upload", "audio-uploader-go", "manifests/upload.json");
        if !run_stage(c, &mut st) {
            failed = true;
            report_failed_stage = Some("upload".to_string());
            report_status = PIPELINE_STATUS_FAILED.to_string();
            report_exit_code = 1;
            report_error = st.error.clone();
        }
        st
    };
    stages.push(stage_upload);

    if !upload_was_skipped {
        let upload_manifest = manifest_dir.join("upload.json");
        let (counts, warn) = read_upload_counts(&upload_manifest);
        if let Some(ref c) = counts {
            if let Some(st) = stages.last_mut() {
                st.counts = Some(c.clone());
            }
            report_summary = summarize_upload_counts(Some(c));
        } else if let Some(w) = warn {
            report_warnings.push(w);
        }
    }

    let pipeline_finished_at = Utc::now().to_rfc3339();
    let pipeline_duration_ms = pipeline_t0.elapsed().as_millis() as i64;

    let report = PipelineReport {
        schema_version: 1,
        client: args.client.clone(),
        date: args.date.clone(),
        run_id: run_id.clone(),
        dry_run: args.dry_run,
        shared_root: args.shared_root.clone(),
        clients_dir: args.clients_dir.clone(),
        run_dir: run_dir.display().to_string(),
        started_at: pipeline_started_at,
        finished_at: pipeline_finished_at,
        duration_ms: pipeline_duration_ms,
        status: report_status,
        failed_stage: report_failed_stage,
        exit_code: report_exit_code,
        stages,
        summary: report_summary,
        warnings: report_warnings,
        error: report_error,
    };

    write_pipeline_json(&report, &manifest_dir)?;

    if !failed {
        println!("run_dir={}", run_dir.display());
        Ok(())
    } else {
        anyhow::bail!(
            "{} failed",
            report.failed_stage.as_deref().unwrap_or("unknown stage")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn synthetic_report() -> PipelineReport {
        PipelineReport {
            schema_version: 1,
            client: "synthetic-client".to_string(),
            date: "2026-01-08".to_string(),
            run_id: "test-run".to_string(),
            dry_run: true,
            shared_root: "/shared".to_string(),
            clients_dir: "config/clients".to_string(),
            run_dir: "/shared/runs/synthetic-client/2026-01-08/test-run".to_string(),
            started_at: "2026-01-08T10:00:00Z".to_string(),
            finished_at: "2026-01-08T10:00:01Z".to_string(),
            duration_ms: 1000,
            status: PIPELINE_STATUS_OK.to_string(),
            failed_stage: None,
            exit_code: 0,
            stages: vec![
                make_pending_stage("fetch", "audio-fetcher-rs", "manifests/fetch.json"),
                make_pending_stage("convert", "audio-converter-rs", "manifests/convert.json"),
                make_pending_stage("match", "metadata-matcher-rs", "manifests/match.json"),
                make_pending_stage("upload", "audio-uploader-go", "manifests/upload.json"),
            ],
            summary: Value::Object(serde_json::Map::new()),
            warnings: vec![],
            error: None,
        }
    }

    // Verifies schema_version field is 1 in both struct and JSON.
    #[test]
    fn test_pipeline_report_schema_version_is_one() {
        let report = synthetic_report();
        assert_eq!(report.schema_version, 1u32);
        let v: Value = serde_json::to_value(&report).unwrap();
        assert_eq!(v["schema_version"].as_u64().unwrap(), 1u64);
    }

    // Verifies all PipelineReport top-level JSON keys are snake_case.
    #[test]
    fn test_pipeline_report_json_field_names_are_snake_case() {
        let v: Value = serde_json::to_value(&synthetic_report()).unwrap();
        let required = [
            "schema_version", "client", "date", "run_id", "dry_run",
            "shared_root", "clients_dir", "run_dir", "started_at", "finished_at",
            "duration_ms", "status", "failed_stage", "exit_code", "stages",
            "summary", "warnings", "error",
        ];
        for field in &required {
            assert!(v.get(*field).is_some(), "missing required PipelineReport field: {field}");
        }
        let camel = [
            "schemaVersion", "runId", "dryRun", "sharedRoot", "clientsDir",
            "runDir", "startedAt", "finishedAt", "durationMs", "failedStage", "exitCode",
        ];
        for field in &camel {
            assert!(v.get(*field).is_none(), "unexpected CamelCase key in PipelineReport: {field}");
        }
    }

    // Verifies all PipelineStage JSON keys are snake_case.
    #[test]
    fn test_pipeline_stage_json_field_names_are_snake_case() {
        let stage = make_pending_stage("fetch", "audio-fetcher-rs", "manifests/fetch.json");
        let v: Value = serde_json::to_value(&stage).unwrap();
        let required = [
            "name", "command", "started_at", "finished_at", "duration_ms",
            "exit_code", "status", "manifest_path", "counts", "error", "stderr_tail",
        ];
        for field in &required {
            assert!(v.get(*field).is_some(), "missing required PipelineStage field: {field}");
        }
        let camel = [
            "startedAt", "finishedAt", "durationMs", "exitCode",
            "manifestPath", "stderrTail",
        ];
        for field in &camel {
            assert!(v.get(*field).is_none(), "unexpected CamelCase key in PipelineStage: {field}");
        }
    }

    // Verifies make_pending_stage initializes all fields to expected defaults.
    #[test]
    fn test_make_pending_stage_defaults() {
        let s = make_pending_stage("fetch", "audio-fetcher-rs", "manifests/fetch.json");
        assert_eq!(s.name, "fetch");
        assert_eq!(s.command, "audio-fetcher-rs");
        assert_eq!(s.manifest_path, "manifests/fetch.json");
        assert_eq!(s.status, STAGE_STATUS_PENDING);
        assert!(s.started_at.is_none());
        assert!(s.finished_at.is_none());
        assert!(s.duration_ms.is_none());
        assert!(s.exit_code.is_none());
        assert!(s.counts.is_none());
        assert!(s.error.is_none());
        assert!(s.stderr_tail.is_none());
    }

    // Verifies make_skipped_stage initializes status to skipped and all optionals to None.
    #[test]
    fn test_make_skipped_stage_defaults() {
        let s = make_skipped_stage("upload", "audio-uploader-go", "manifests/upload.json");
        assert_eq!(s.name, "upload");
        assert_eq!(s.command, "audio-uploader-go");
        assert_eq!(s.manifest_path, "manifests/upload.json");
        assert_eq!(s.status, STAGE_STATUS_SKIPPED);
        assert!(s.started_at.is_none());
        assert!(s.finished_at.is_none());
        assert!(s.duration_ms.is_none());
        assert!(s.exit_code.is_none());
        assert!(s.counts.is_none());
        assert!(s.error.is_none());
        assert!(s.stderr_tail.is_none());
    }

    // Verifies all status constants equal their designed string values.
    #[test]
    fn test_stage_status_constants() {
        assert_eq!(PIPELINE_STATUS_OK, "ok");
        assert_eq!(PIPELINE_STATUS_FAILED, "failed");
        assert_eq!(STAGE_STATUS_PENDING, "pending");
        assert_eq!(STAGE_STATUS_OK, "ok");
        assert_eq!(STAGE_STATUS_FAILED, "failed");
        assert_eq!(STAGE_STATUS_SKIPPED, "skipped");
    }

    // Verifies stages array contains exactly 4 stages in the correct order with correct manifest paths.
    #[test]
    fn test_pipeline_report_four_stage_shape() {
        let report = synthetic_report();
        assert_eq!(report.stages.len(), 4);
        let expected = [
            ("fetch",   "manifests/fetch.json"),
            ("convert", "manifests/convert.json"),
            ("match",   "manifests/match.json"),
            ("upload",  "manifests/upload.json"),
        ];
        for (i, (name, manifest)) in expected.iter().enumerate() {
            assert_eq!(report.stages[i].name, *name,
                "stage[{i}].name mismatch");
            assert_eq!(report.stages[i].manifest_path, *manifest,
                "stage[{i}].manifest_path mismatch");
        }
    }

    // Guards that counts and stderr_tail serialize as JSON null in the Phase 2H-B shape.
    #[test]
    fn test_counts_and_stderr_tail_are_null_in_phase_2h_b_shape() {
        let stage = make_pending_stage("fetch", "audio-fetcher-rs", "manifests/fetch.json");
        let v: Value = serde_json::to_value(&stage).unwrap();
        assert_eq!(v.get("counts").unwrap(), &json!(null),
            "counts must be null in Phase 2H-B (aggregation not yet implemented)");
        assert_eq!(v.get("stderr_tail").unwrap(), &json!(null),
            "stderr_tail must be null in Phase 2H-B (stderr capture not yet implemented)");
    }

    // Guards against PII field names being added to PipelineReport or PipelineStage JSON.
    #[test]
    fn test_pipeline_report_no_direct_pii_fields() {
        let forbidden = [
            "telefono", "phone",
            "nombre_deudor", "deudor", "deuda", "monto_deuda", "saldo",
            "agent_name", "nombre_agente",
            "password", "sftp_password",
            "host_key", "sftp_host_key",
        ];

        let report_v: Value = serde_json::to_value(&synthetic_report()).unwrap();
        if let Value::Object(m) = &report_v {
            for key in m.keys() {
                let k = key.as_str();
                assert!(!forbidden.contains(&k),
                    "PipelineReport has forbidden PII field: {k}");
            }
        }

        let stage = make_pending_stage("fetch", "audio-fetcher-rs", "manifests/fetch.json");
        let stage_v: Value = serde_json::to_value(&stage).unwrap();
        if let Value::Object(m) = &stage_v {
            for key in m.keys() {
                let k = key.as_str();
                assert!(!forbidden.contains(&k),
                    "PipelineStage has forbidden PII field: {k}");
            }
        }
    }

    // --- Helpers for read_upload_counts / summarize_upload_counts tests ---

    // Writes synthetic JSON content to a unique temp file and returns the path.
    fn write_temp_json(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir()
            .join(format!("pipetest_{}_{}.json", name, std::process::id()));
        std::fs::write(&path, content).expect("write_temp_json failed");
        path
    }

    // --- read_upload_counts tests ---

    // Verifies that a well-formed upload.json with a counts object returns Some(counts) and no warning.
    #[test]
    fn test_read_upload_counts_valid_counts_object() {
        let content = r#"{"schema_version":2,"counts":{"total":5,"sent_ok":3,"skipped_parse":0,"skipped_validation":1,"skipped_prepare":0,"send_error":1}}"#;
        let path = write_temp_json("valid_counts", content);
        let (counts, warn) = read_upload_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_some(), "expected Some(counts), got None");
        assert!(warn.is_none(), "expected no warning, got: {warn:?}");
        let c = counts.unwrap();
        assert_eq!(c["total"].as_u64().unwrap(), 5);
        assert_eq!(c["sent_ok"].as_u64().unwrap(), 3);
        assert_eq!(c["send_error"].as_u64().unwrap(), 1);
    }

    // Verifies that a nonexistent path returns None and a warning containing "missing".
    #[test]
    fn test_read_upload_counts_missing_file_warns() {
        let path = std::env::temp_dir()
            .join(format!("pipetest_missing_{}_nonexistent.json", std::process::id()));
        let (counts, warn) = read_upload_counts(&path);
        assert!(counts.is_none(), "expected None counts for missing file");
        assert!(warn.is_some(), "expected a warning for missing file");
        assert!(
            warn.unwrap().contains("missing"),
            "warning must contain 'missing'"
        );
    }

    // Verifies that invalid JSON content returns None and a warning containing "parse failed".
    #[test]
    fn test_read_upload_counts_invalid_json_warns() {
        let path = write_temp_json("invalid_json", "not valid json {{{");
        let (counts, warn) = read_upload_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts for invalid JSON");
        assert!(warn.is_some(), "expected a warning for invalid JSON");
        assert!(
            warn.unwrap().contains("parse failed"),
            "warning must contain 'parse failed'"
        );
    }

    // Verifies that valid JSON with no counts field returns None and a warning containing "counts missing".
    #[test]
    fn test_read_upload_counts_missing_counts_warns() {
        let path = write_temp_json("missing_counts", r#"{"schema_version":2}"#);
        let (counts, warn) = read_upload_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts when counts field absent");
        assert!(warn.is_some(), "expected a warning when counts field absent");
        assert!(
            warn.unwrap().contains("counts missing"),
            "warning must contain 'counts missing'"
        );
    }

    // Verifies that a counts field that is not an object (null) returns None and a warning containing "counts not object".
    #[test]
    fn test_read_upload_counts_counts_not_object_warns() {
        let path = write_temp_json("counts_null", r#"{"schema_version":2,"counts":null}"#);
        let (counts, warn) = read_upload_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts when counts is not an object");
        assert!(warn.is_some(), "expected a warning when counts is not an object");
        assert!(
            warn.unwrap().contains("counts not object"),
            "warning must contain 'counts not object'"
        );
    }

    // --- summarize_upload_counts tests ---

    // Verifies that a full counts object produces the correct upload_* summary fields.
    #[test]
    fn test_summarize_upload_counts_valid_counts() {
        let counts = json!({
            "total": 5, "sent_ok": 3, "skipped_parse": 0,
            "skipped_validation": 1, "skipped_prepare": 0, "send_error": 1
        });
        let summary = summarize_upload_counts(Some(&counts));
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 5);
        assert_eq!(summary["upload_sent_ok"].as_u64().unwrap(), 3);
        assert_eq!(summary["upload_skipped_parse"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_skipped_validation"].as_u64().unwrap(), 1);
        assert_eq!(summary["upload_skipped_prepare"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_send_error"].as_u64().unwrap(), 1);
    }

    // Verifies that None input returns an empty JSON object.
    #[test]
    fn test_summarize_upload_counts_none_returns_empty_object() {
        let summary = summarize_upload_counts(None);
        assert!(summary.is_object(), "result must be a JSON object");
        assert_eq!(
            summary.as_object().unwrap().len(),
            0,
            "object must be empty when counts is None"
        );
    }

    // Verifies that missing numeric fields in a partial counts object default to 0.
    #[test]
    fn test_summarize_upload_counts_missing_fields_default_to_zero() {
        let counts = json!({"total": 7, "sent_ok": 6});
        let summary = summarize_upload_counts(Some(&counts));
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 7);
        assert_eq!(summary["upload_sent_ok"].as_u64().unwrap(), 6);
        assert_eq!(summary["upload_skipped_parse"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_skipped_validation"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_skipped_prepare"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_send_error"].as_u64().unwrap(), 0);
    }

    // Verifies that non-numeric values for counts fields are treated as 0.
    #[test]
    fn test_summarize_upload_counts_non_numeric_fields_default_to_zero() {
        let counts = json!({"total": "not-a-number", "sent_ok": 4, "send_error": "bad"});
        let summary = summarize_upload_counts(Some(&counts));
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_sent_ok"].as_u64().unwrap(), 4);
        assert_eq!(summary["upload_send_error"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_skipped_parse"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_skipped_validation"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_skipped_prepare"].as_u64().unwrap(), 0);
    }
}

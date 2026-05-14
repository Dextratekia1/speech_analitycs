use anyhow::{Context, Result};
use audios_common::{paths, util};
use chrono::Utc;
use clap::Parser;
use serde::Serialize;
use serde_json::Value;
use std::io::Write;
use std::time::Instant;
use std::{fs, path::{Path, PathBuf}, process::{Command, Stdio}};

// Pipeline-level status values.
const PIPELINE_STATUS_OK: &str = "ok";
const PIPELINE_STATUS_FAILED: &str = "failed";
const PIPELINE_STATUS_PARTIAL: &str = "partial";

// Stage-level status values.
const STAGE_STATUS_PENDING: &str = "pending";
const STAGE_STATUS_OK: &str = "ok";
const STAGE_STATUS_FAILED: &str = "failed";
const STAGE_STATUS_SKIPPED: &str = "skipped";

const STDERR_TAIL_LIMIT: usize = 2048;

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

    #[arg(long, default_value = "/run/secrets/sftp-env")]
    sftp_secret_path: String,

    #[arg(long)]
    conversion_concurrency: Option<usize>,
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

// Returns a bounded UTF-8 tail of stderr bytes, or None if empty.
// Advances past UTF-8 continuation bytes at the trim point to avoid
// splitting a multi-byte sequence. Always returns valid UTF-8.
fn extract_stderr_tail(stderr: &[u8]) -> Option<String> {
    if stderr.is_empty() {
        return None;
    }
    let start = if stderr.len() <= STDERR_TAIL_LIMIT {
        0
    } else {
        let raw = stderr.len() - STDERR_TAIL_LIMIT;
        // Skip continuation bytes (0x80..=0xBF) at the cut point.
        let mut pos = raw;
        while pos < stderr.len() && (stderr[pos] & 0xC0) == 0x80 {
            pos += 1;
        }
        pos
    };
    let tail = &stderr[start..];
    if tail.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(tail).into_owned())
}

// Runs cmd, capturing stderr while leaving stdout inherited.
// Populates stage timing, status, error, and stderr_tail.
// Returns true on success (exit code 0), false otherwise.
fn run_stage(mut cmd: Command, stage: &mut PipelineStage) -> bool {
    cmd.stderr(Stdio::piped());
    stage.started_at = Some(Utc::now().to_rfc3339());
    let t0 = Instant::now();
    let child = match cmd.spawn() {
        Err(e) => {
            stage.finished_at = Some(Utc::now().to_rfc3339());
            stage.duration_ms = Some(t0.elapsed().as_millis() as i64);
            stage.status = STAGE_STATUS_FAILED.to_string();
            stage.error = Some(format!("spawn {}: {e}", stage.command));
            return false;
        }
        Ok(c) => c,
    };
    let output = match child.wait_with_output() {
        Err(e) => {
            stage.finished_at = Some(Utc::now().to_rfc3339());
            stage.duration_ms = Some(t0.elapsed().as_millis() as i64);
            stage.status = STAGE_STATUS_FAILED.to_string();
            stage.error = Some(format!("wait {}: {e}", stage.command));
            return false;
        }
        Ok(o) => o,
    };
    stage.finished_at = Some(Utc::now().to_rfc3339());
    stage.duration_ms = Some(t0.elapsed().as_millis() as i64);
    stage.exit_code = output.status.code();
    if output.status.success() {
        stage.status = STAGE_STATUS_OK.to_string();
        true
    } else {
        stage.status = STAGE_STATUS_FAILED.to_string();
        stage.error = Some(format!("{} exited: {}", stage.command, output.status));
        // Mirror captured stderr to parent stderr so operator logs remain useful.
        if !output.stderr.is_empty() {
            let _ = std::io::stderr().write_all(&output.stderr);
        }
        stage.stderr_tail = extract_stderr_tail(&output.stderr);
        false
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

// Returns true only when summary contains upload_send_error as a numeric value > 0.
// Returns false for missing, zero, negative, non-numeric, or null values.
fn detect_partial_upload_success(summary: &Value) -> bool {
    summary
        .get("upload_send_error")
        .and_then(|v| v.as_u64())
        .map(|n| n > 0)
        .unwrap_or(false)
}

// Reads fetch.json and extracts item count.
// Returns (Some(counts_value), None) on success.
// Returns (None, Some(warning)) on any failure; never panics and never fails the pipeline.
fn read_fetch_counts(path: &Path) -> (Option<Value>, Option<String>) {
    let bytes = match fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (None, Some("fetch manifest missing".to_string()));
        }
        Err(_) => {
            return (None, Some("fetch manifest read failed".to_string()));
        }
        Ok(b) => b,
    };
    let doc: Value = match serde_json::from_slice(&bytes) {
        Err(_) => return (None, Some("fetch manifest parse failed".to_string())),
        Ok(v) => v,
    };
    match doc.get("items") {
        None => (None, Some("fetch manifest items missing".to_string())),
        Some(arr) if arr.is_array() => {
            let total = arr.as_array().map(|a| a.len()).unwrap_or(0) as u64;
            (Some(serde_json::json!({ "total": total })), None)
        }
        Some(_) => (None, Some("fetch manifest items not array".to_string())),
    }
}

// Builds the fetch portion of the pipeline-level summary.
// Missing or non-numeric fields default to 0 without error.
fn summarize_fetch_counts(counts: Option<&Value>) -> Value {
    let Some(c) = counts else {
        return Value::Object(serde_json::Map::new());
    };
    let n = |key: &str| -> u64 { c.get(key).and_then(|v| v.as_u64()).unwrap_or(0) };
    serde_json::json!({
        "fetch_total": n("total"),
    })
}

// Merges all key-value pairs from `addition` into `base`.
// Both must be JSON objects; if either is not, the call is a no-op.
fn merge_json_object(base: &mut Value, addition: &Value) {
    if let (Value::Object(b), Value::Object(a)) = (base, addition) {
        for (k, v) in a {
            b.insert(k.clone(), v.clone());
        }
    }
}

fn read_convert_counts(path: &Path) -> (Option<Value>, Option<String>) {
    let bytes = match fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (None, Some("convert manifest missing".to_string()))
        }
        Err(_) => return (None, Some("convert manifest read failed".to_string())),
        Ok(b) => b,
    };
    let doc: Value = match serde_json::from_slice(&bytes) {
        Err(_) => return (None, Some("convert manifest parse failed".to_string())),
        Ok(v) => v,
    };
    let items = match doc.get("items") {
        None => return (None, Some("convert manifest items missing".to_string())),
        Some(arr) if arr.is_array() => arr.as_array().unwrap(),
        Some(_) => return (None, Some("convert manifest items not array".to_string())),
    };
    let mut total: u64 = 0;
    let mut ok: u64 = 0;
    let mut skip_exists: u64 = 0;
    let mut dry_run: u64 = 0;
    let mut ffmpeg_error: u64 = 0;
    for item in items {
        total += 1;
        match item.get("status").and_then(|v| v.as_str()) {
            Some("ok") => ok += 1,
            Some("skip_exists") => skip_exists += 1,
            Some("dry_run") => dry_run += 1,
            Some("ffmpeg_error") => ffmpeg_error += 1,
            _ => {}
        }
    }
    (
        Some(serde_json::json!({
            "total": total,
            "ok": ok,
            "skip_exists": skip_exists,
            "dry_run": dry_run,
            "ffmpeg_error": ffmpeg_error,
        })),
        None,
    )
}

fn summarize_convert_counts(counts: Option<&Value>) -> Value {
    let Some(c) = counts else {
        return Value::Object(serde_json::Map::new());
    };
    let n = |key: &str| -> u64 { c.get(key).and_then(|v| v.as_u64()).unwrap_or(0) };
    serde_json::json!({
        "convert_total": n("total"),
        "convert_ok": n("ok"),
        "convert_skip_exists": n("skip_exists"),
        "convert_dry_run": n("dry_run"),
        "convert_ffmpeg_error": n("ffmpeg_error"),
    })
}

fn read_match_counts(path: &Path) -> (Option<Value>, Option<String>) {
    let bytes = match fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (None, Some("match manifest missing".to_string()))
        }
        Err(_) => return (None, Some("match manifest read failed".to_string())),
        Ok(b) => b,
    };
    let doc: Value = match serde_json::from_slice(&bytes) {
        Err(_) => return (None, Some("match manifest parse failed".to_string())),
        Ok(v) => v,
    };
    let items = match doc.get("items") {
        None => return (None, Some("match manifest items missing".to_string())),
        Some(arr) if arr.is_array() => arr.as_array().unwrap(),
        Some(_) => return (None, Some("match manifest items not array".to_string())),
    };
    let mut total: u64 = 0;
    let mut lookup_ok: u64 = 0;
    let mut lookup_failed: u64 = 0;
    for item in items {
        total += 1;
        match item.get("lookup_ok").and_then(|v| v.as_bool()) {
            Some(true) => lookup_ok += 1,
            Some(false) => lookup_failed += 1,
            None => {}
        }
    }
    (
        Some(serde_json::json!({
            "total": total,
            "lookup_ok": lookup_ok,
            "lookup_failed": lookup_failed,
        })),
        None,
    )
}

fn summarize_match_counts(counts: Option<&Value>) -> Value {
    let Some(c) = counts else {
        return Value::Object(serde_json::Map::new());
    };
    let n = |key: &str| -> u64 { c.get(key).and_then(|v| v.as_u64()).unwrap_or(0) };
    serde_json::json!({
        "match_total": n("total"),
        "match_lookup_ok": n("lookup_ok"),
        "match_lookup_failed": n("lookup_failed"),
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
    let convert_was_skipped = failed;
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
        if let Some(n) = args.conversion_concurrency {
            c.args(["--conversion-concurrency", &n.to_string()]);
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
    let match_was_skipped = failed;
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
            "--sftp-secret-path",
            &args.sftp_secret_path,
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

    // Fetch aggregation — fetch always executes as the first stage.
    {
        let fetch_manifest = manifest_dir.join("fetch.json");
        let (counts, warn) = read_fetch_counts(&fetch_manifest);
        if let Some(ref c) = counts {
            if let Some(st) = stages.get_mut(0) {
                st.counts = Some(c.clone());
            }
            merge_json_object(&mut report_summary, &summarize_fetch_counts(Some(c)));
        } else if let Some(w) = warn {
            report_warnings.push(w);
        }
    }

    if !convert_was_skipped {
        let convert_manifest = manifest_dir.join("convert.json");
        let (counts, warn) = read_convert_counts(&convert_manifest);
        if let Some(ref c) = counts {
            if let Some(st) = stages.get_mut(1) {
                st.counts = Some(c.clone());
            }
            merge_json_object(&mut report_summary, &summarize_convert_counts(Some(c)));
        } else if let Some(w) = warn {
            report_warnings.push(w);
        }
    }

    if !match_was_skipped {
        let match_manifest = manifest_dir.join("match.json");
        let (counts, warn) = read_match_counts(&match_manifest);
        if let Some(ref c) = counts {
            if let Some(st) = stages.get_mut(2) {
                st.counts = Some(c.clone());
            }
            merge_json_object(&mut report_summary, &summarize_match_counts(Some(c)));
        } else if let Some(w) = warn {
            report_warnings.push(w);
        }
    }

    if !upload_was_skipped {
        let upload_manifest = manifest_dir.join("upload.json");
        let (counts, warn) = read_upload_counts(&upload_manifest);
        if let Some(ref c) = counts {
            if let Some(st) = stages.last_mut() {
                st.counts = Some(c.clone());
            }
            merge_json_object(&mut report_summary, &summarize_upload_counts(Some(c)));
        } else if let Some(w) = warn {
            report_warnings.push(w);
        }
    }

    // Partial: all stages exited 0 but upload_send_error > 0.
    // Stage failure takes precedence: only promote to partial when status is ok.
    if report_status == PIPELINE_STATUS_OK && detect_partial_upload_success(&report_summary) {
        report_status = PIPELINE_STATUS_PARTIAL.to_string();
        report_warnings.push("upload partial success: upload_send_error > 0".to_string());
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

    // Concise final summary on stderr — does not affect run_dir= stdout behavior.
    {
        let summary = &report.summary;
        let n = |key: &str| -> u64 { summary.get(key).and_then(|v| v.as_u64()).unwrap_or(0) };
        eprintln!(
            "pipeline status={} client={} date={} duration={}ms dry_run={}",
            report.status, report.client, report.date, pipeline_duration_ms, report.dry_run
        );
        for st in &report.stages {
            eprintln!("  stage={} status={} duration={}ms",
                st.name, st.status, st.duration_ms.unwrap_or(0));
        }
        if summary.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
            eprintln!(
                "  counts fetch={} convert={} match={} upload_total={} sent={} send_error={}",
                n("fetch_total"), n("convert_total"), n("match_total"),
                n("upload_total"), n("upload_sent_ok"), n("upload_send_error")
            );
        }
    }

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

    // Guards that a pending stage serializes counts and stderr_tail as JSON null.
    // counts becomes non-null only after the stage runs and aggregation populates it.
    // stderr_tail becomes non-null only when a stage fails with non-empty stderr.
    #[test]
    fn test_pending_stage_stderr_tail_starts_null() {
        let stage = make_pending_stage("fetch", "audio-fetcher-rs", "manifests/fetch.json");
        let v: Value = serde_json::to_value(&stage).unwrap();
        assert_eq!(v.get("counts").unwrap(), &json!(null),
            "pending stage counts must serialize as null before aggregation runs");
        assert_eq!(v.get("stderr_tail").unwrap(), &json!(null),
            "pending stage stderr_tail must serialize as null before the stage runs");
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

    // --- detect_partial_upload_success / PIPELINE_STATUS_PARTIAL tests ---

    #[test]
    fn test_pipeline_status_constants_include_partial() {
        assert_eq!(PIPELINE_STATUS_PARTIAL, "partial");
    }

    #[test]
    fn test_detect_partial_upload_success_send_error_positive() {
        let summary = json!({"upload_send_error": 1u64});
        assert!(detect_partial_upload_success(&summary));
    }

    #[test]
    fn test_detect_partial_upload_success_send_error_zero() {
        let summary = json!({"upload_send_error": 0u64});
        assert!(!detect_partial_upload_success(&summary));
    }

    #[test]
    fn test_detect_partial_upload_success_missing_summary_field() {
        let summary = json!({});
        assert!(!detect_partial_upload_success(&summary));
    }

    #[test]
    fn test_detect_partial_upload_success_non_numeric_send_error() {
        let summary = json!({"upload_send_error": "1"});
        assert!(!detect_partial_upload_success(&summary));
    }

    // --- read_fetch_counts / summarize_fetch_counts / merge_json_object tests ---

    #[test]
    fn test_read_fetch_counts_valid_items() {
        let content = r#"{"schema_version":1,"items":[{"status":"ok"},{"status":"ok"},{"status":"ok"}]}"#;
        let path = write_temp_json("fetch_valid", content);
        let (counts, warn) = read_fetch_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_some(), "expected Some(counts)");
        assert!(warn.is_none(), "expected no warning");
        assert_eq!(counts.unwrap()["total"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_read_fetch_counts_missing_file_warns() {
        let path = std::env::temp_dir()
            .join(format!("pipetest_fetch_missing_{}_nonexistent.json", std::process::id()));
        let (counts, warn) = read_fetch_counts(&path);
        assert!(counts.is_none(), "expected None counts for missing file");
        assert!(warn.is_some(), "expected a warning for missing file");
        assert!(warn.unwrap().contains("missing"), "warning must contain 'missing'");
    }

    #[test]
    fn test_read_fetch_counts_invalid_json_warns() {
        let path = write_temp_json("fetch_invalid_json", "not valid {{{");
        let (counts, warn) = read_fetch_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts for invalid JSON");
        assert!(warn.is_some(), "expected a warning for invalid JSON");
        assert!(warn.unwrap().contains("parse failed"), "warning must contain 'parse failed'");
    }

    #[test]
    fn test_read_fetch_counts_items_missing_warns() {
        let path = write_temp_json("fetch_no_items", r#"{"schema_version":1}"#);
        let (counts, warn) = read_fetch_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts when items absent");
        assert!(warn.is_some(), "expected a warning when items absent");
        assert!(warn.unwrap().contains("items missing"), "warning must contain 'items missing'");
    }

    #[test]
    fn test_read_fetch_counts_items_not_array_warns() {
        let path = write_temp_json("fetch_items_null", r#"{"schema_version":1,"items":null}"#);
        let (counts, warn) = read_fetch_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts when items is not array");
        assert!(warn.is_some(), "expected a warning when items is not array");
        assert!(warn.unwrap().contains("items not array"), "warning must contain 'items not array'");
    }

    #[test]
    fn test_summarize_fetch_counts_valid() {
        let counts = json!({"total": 3u64});
        let summary = summarize_fetch_counts(Some(&counts));
        assert_eq!(summary["fetch_total"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_summarize_fetch_counts_none_returns_empty() {
        let summary = summarize_fetch_counts(None);
        assert!(summary.is_object(), "result must be a JSON object");
        assert_eq!(summary.as_object().unwrap().len(), 0, "must be empty when counts is None");
    }

    #[test]
    fn test_summarize_fetch_counts_missing_or_non_numeric_total_defaults_to_zero() {
        let counts_missing = json!({});
        let s1 = summarize_fetch_counts(Some(&counts_missing));
        assert_eq!(s1["fetch_total"].as_u64().unwrap(), 0);

        let counts_string = json!({"total": "not-a-number"});
        let s2 = summarize_fetch_counts(Some(&counts_string));
        assert_eq!(s2["fetch_total"].as_u64().unwrap(), 0);
    }

    #[test]
    fn test_fetch_summary_merges_with_upload_summary() {
        let fetch_counts = json!({"total": 5u64});
        let upload_counts = json!({
            "total": 5u64, "sent_ok": 4u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 1u64
        });
        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(Some(&fetch_counts)));
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_counts)));
        assert_eq!(summary["fetch_total"].as_u64().unwrap(), 5);
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 5);
        assert_eq!(summary["upload_sent_ok"].as_u64().unwrap(), 4);
        assert_eq!(summary["upload_send_error"].as_u64().unwrap(), 1);
    }

    // --- read_convert_counts / summarize_convert_counts tests ---

    // Verifies that a well-formed convert.json with all four status values returns correct counts.
    #[test]
    fn test_read_convert_counts_all_statuses() {
        let content = r#"{"schema_version":1,"items":[
            {"status":"ok"},{"status":"ok"},
            {"status":"skip_exists"},
            {"status":"dry_run"},
            {"status":"ffmpeg_error"}
        ]}"#;
        let path = write_temp_json("convert_all_statuses", content);
        let (counts, warn) = read_convert_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_some(), "expected Some(counts)");
        assert!(warn.is_none(), "expected no warning");
        let c = counts.unwrap();
        assert_eq!(c["total"].as_u64().unwrap(), 5);
        assert_eq!(c["ok"].as_u64().unwrap(), 2);
        assert_eq!(c["skip_exists"].as_u64().unwrap(), 1);
        assert_eq!(c["dry_run"].as_u64().unwrap(), 1);
        assert_eq!(c["ffmpeg_error"].as_u64().unwrap(), 1);
    }

    // Verifies that an item with an unknown/missing status still increments total only.
    #[test]
    fn test_read_convert_counts_unknown_status_increments_total_only() {
        let content = r#"{"schema_version":1,"items":[
            {"status":"ok"},
            {"status":"unexpected_value"},
            {}
        ]}"#;
        let path = write_temp_json("convert_unknown_status", content);
        let (counts, warn) = read_convert_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_some(), "expected Some(counts)");
        assert!(warn.is_none(), "expected no warning for unknown status");
        let c = counts.unwrap();
        assert_eq!(c["total"].as_u64().unwrap(), 3);
        assert_eq!(c["ok"].as_u64().unwrap(), 1);
        assert_eq!(c["skip_exists"].as_u64().unwrap(), 0);
        assert_eq!(c["dry_run"].as_u64().unwrap(), 0);
        assert_eq!(c["ffmpeg_error"].as_u64().unwrap(), 0);
    }

    // Verifies that a nonexistent path returns None and a warning containing "missing".
    #[test]
    fn test_read_convert_counts_missing_file_warns() {
        let path = std::env::temp_dir()
            .join(format!("pipetest_conv_missing_{}_nonexistent.json", std::process::id()));
        let (counts, warn) = read_convert_counts(&path);
        assert!(counts.is_none(), "expected None counts for missing file");
        assert!(warn.is_some(), "expected a warning for missing file");
        assert!(warn.unwrap().contains("missing"), "warning must contain 'missing'");
    }

    // Verifies that invalid JSON content returns None and a warning containing "parse failed".
    #[test]
    fn test_read_convert_counts_invalid_json_warns() {
        let path = write_temp_json("convert_invalid_json", "not valid json {{{");
        let (counts, warn) = read_convert_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts for invalid JSON");
        assert!(warn.is_some(), "expected a warning for invalid JSON");
        assert!(warn.unwrap().contains("parse failed"), "warning must contain 'parse failed'");
    }

    // Verifies that valid JSON with no items field returns None and warning containing "items missing".
    #[test]
    fn test_read_convert_counts_items_missing_warns() {
        let path = write_temp_json("convert_no_items", r#"{"schema_version":1}"#);
        let (counts, warn) = read_convert_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts when items absent");
        assert!(warn.is_some(), "expected a warning when items absent");
        assert!(warn.unwrap().contains("items missing"), "warning must contain 'items missing'");
    }

    // Verifies that items field that is not an array returns None and warning containing "items not array".
    #[test]
    fn test_read_convert_counts_items_not_array_warns() {
        let path = write_temp_json("convert_items_null", r#"{"schema_version":1,"items":null}"#);
        let (counts, warn) = read_convert_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts when items is not array");
        assert!(warn.is_some(), "expected a warning when items is not array");
        assert!(warn.unwrap().contains("items not array"), "warning must contain 'items not array'");
    }

    // Verifies that a full counts object produces the correct convert_* summary fields.
    #[test]
    fn test_summarize_convert_counts_valid() {
        let counts = json!({
            "total": 5u64, "ok": 2u64,
            "skip_exists": 1u64, "dry_run": 1u64, "ffmpeg_error": 1u64
        });
        let summary = summarize_convert_counts(Some(&counts));
        assert_eq!(summary["convert_total"].as_u64().unwrap(), 5);
        assert_eq!(summary["convert_ok"].as_u64().unwrap(), 2);
        assert_eq!(summary["convert_skip_exists"].as_u64().unwrap(), 1);
        assert_eq!(summary["convert_dry_run"].as_u64().unwrap(), 1);
        assert_eq!(summary["convert_ffmpeg_error"].as_u64().unwrap(), 1);
    }

    // Verifies that None input to summarize_convert_counts returns an empty JSON object.
    #[test]
    fn test_summarize_convert_counts_none_returns_empty_object() {
        let summary = summarize_convert_counts(None);
        assert!(summary.is_object(), "result must be a JSON object");
        assert_eq!(
            summary.as_object().unwrap().len(),
            0,
            "object must be empty when counts is None"
        );
    }

    // Verifies that missing or non-numeric fields in counts default to 0 (no panic).
    #[test]
    fn test_summarize_convert_counts_missing_or_non_numeric_defaults_to_zero() {
        let counts_missing = json!({});
        let s1 = summarize_convert_counts(Some(&counts_missing));
        assert_eq!(s1["convert_total"].as_u64().unwrap(), 0);
        assert_eq!(s1["convert_ok"].as_u64().unwrap(), 0);
        assert_eq!(s1["convert_ffmpeg_error"].as_u64().unwrap(), 0);

        let counts_string = json!({"total": "not-a-number", "ok": 3u64});
        let s2 = summarize_convert_counts(Some(&counts_string));
        assert_eq!(s2["convert_total"].as_u64().unwrap(), 0);
        assert_eq!(s2["convert_ok"].as_u64().unwrap(), 3);
    }

    // Verifies that fetch, convert, and upload summary keys coexist when merged.
    #[test]
    fn test_fetch_convert_upload_summary_all_merge() {
        let fetch_counts = json!({"total": 4u64});
        let convert_counts = json!({
            "total": 4u64, "ok": 3u64, "skip_exists": 1u64, "dry_run": 0u64, "ffmpeg_error": 0u64
        });
        let upload_counts = json!({
            "total": 3u64, "sent_ok": 2u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 1u64, "send_error": 0u64
        });
        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(Some(&fetch_counts)));
        merge_json_object(&mut summary, &summarize_convert_counts(Some(&convert_counts)));
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_counts)));
        assert_eq!(summary["fetch_total"].as_u64().unwrap(), 4);
        assert_eq!(summary["convert_total"].as_u64().unwrap(), 4);
        assert_eq!(summary["convert_ok"].as_u64().unwrap(), 3);
        assert_eq!(summary["convert_skip_exists"].as_u64().unwrap(), 1);
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 3);
        assert_eq!(summary["upload_sent_ok"].as_u64().unwrap(), 2);
        assert_eq!(summary["upload_skipped_prepare"].as_u64().unwrap(), 1);
        assert_eq!(summary["upload_send_error"].as_u64().unwrap(), 0);
    }

    // --- read_match_counts / summarize_match_counts tests ---

    // Verifies that a well-formed match.json with boolean lookup_ok values returns correct counts.
    #[test]
    fn test_read_match_counts_valid_items() {
        let content = r#"{"schema_version":1,"items":[
            {"lookup_ok":true},
            {"lookup_ok":true},
            {"lookup_ok":false}
        ]}"#;
        let path = write_temp_json("match_valid", content);
        let (counts, warn) = read_match_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_some(), "expected Some(counts)");
        assert!(warn.is_none(), "expected no warning");
        let c = counts.unwrap();
        assert_eq!(c["total"].as_u64().unwrap(), 3);
        assert_eq!(c["lookup_ok"].as_u64().unwrap(), 2);
        assert_eq!(c["lookup_failed"].as_u64().unwrap(), 1);
    }

    // Verifies that missing or non-boolean lookup_ok increments total only (no warn).
    #[test]
    fn test_read_match_counts_missing_or_non_boolean_lookup_counts_total_only() {
        let content = r#"{"schema_version":1,"items":[
            {"lookup_ok":true},
            {"lookup_ok":"true"},
            {},
            {"lookup_ok":null}
        ]}"#;
        let path = write_temp_json("match_non_bool", content);
        let (counts, warn) = read_match_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_some(), "expected Some(counts)");
        assert!(warn.is_none(), "expected no warning for non-boolean lookup_ok");
        let c = counts.unwrap();
        assert_eq!(c["total"].as_u64().unwrap(), 4);
        assert_eq!(c["lookup_ok"].as_u64().unwrap(), 1);
        assert_eq!(c["lookup_failed"].as_u64().unwrap(), 0);
    }

    // Verifies that a nonexistent path returns None and a warning containing "missing".
    #[test]
    fn test_read_match_counts_missing_file_warns() {
        let path = std::env::temp_dir()
            .join(format!("pipetest_match_missing_{}_nonexistent.json", std::process::id()));
        let (counts, warn) = read_match_counts(&path);
        assert!(counts.is_none(), "expected None counts for missing file");
        assert!(warn.is_some(), "expected a warning for missing file");
        assert!(warn.unwrap().contains("missing"), "warning must contain 'missing'");
    }

    // Verifies that invalid JSON content returns None and a warning containing "parse failed".
    #[test]
    fn test_read_match_counts_invalid_json_warns() {
        let path = write_temp_json("match_invalid_json", "not valid json {{{");
        let (counts, warn) = read_match_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts for invalid JSON");
        assert!(warn.is_some(), "expected a warning for invalid JSON");
        assert!(warn.unwrap().contains("parse failed"), "warning must contain 'parse failed'");
    }

    // Verifies that valid JSON with no items field returns None and warning containing "items missing".
    #[test]
    fn test_read_match_counts_items_missing_warns() {
        let path = write_temp_json("match_no_items", r#"{"schema_version":1}"#);
        let (counts, warn) = read_match_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts when items absent");
        assert!(warn.is_some(), "expected a warning when items absent");
        assert!(warn.unwrap().contains("items missing"), "warning must contain 'items missing'");
    }

    // Verifies that items field that is not an array returns None and warning containing "items not array".
    #[test]
    fn test_read_match_counts_items_not_array_warns() {
        let path = write_temp_json("match_items_null", r#"{"schema_version":1,"items":null}"#);
        let (counts, warn) = read_match_counts(&path);
        let _ = std::fs::remove_file(&path);
        assert!(counts.is_none(), "expected None counts when items is not array");
        assert!(warn.is_some(), "expected a warning when items is not array");
        assert!(warn.unwrap().contains("items not array"), "warning must contain 'items not array'");
    }

    // Verifies that a full counts object produces the correct match_* summary fields.
    #[test]
    fn test_summarize_match_counts_valid() {
        let counts = json!({
            "total": 3u64,
            "lookup_ok": 2u64,
            "lookup_failed": 1u64
        });
        let summary = summarize_match_counts(Some(&counts));
        assert_eq!(summary["match_total"].as_u64().unwrap(), 3);
        assert_eq!(summary["match_lookup_ok"].as_u64().unwrap(), 2);
        assert_eq!(summary["match_lookup_failed"].as_u64().unwrap(), 1);
    }

    // Verifies that None input to summarize_match_counts returns an empty JSON object.
    #[test]
    fn test_summarize_match_counts_none_returns_empty() {
        let summary = summarize_match_counts(None);
        assert!(summary.is_object(), "result must be a JSON object");
        assert_eq!(
            summary.as_object().unwrap().len(),
            0,
            "object must be empty when counts is None"
        );
    }

    // Verifies that missing or non-numeric counts fields default to 0 without panic.
    #[test]
    fn test_summarize_match_counts_missing_or_non_numeric_fields_default_to_zero() {
        let counts = json!({"total": "bad", "lookup_ok": 2u64, "lookup_failed": "bad"});
        let summary = summarize_match_counts(Some(&counts));
        assert_eq!(summary["match_total"].as_u64().unwrap(), 0);
        assert_eq!(summary["match_lookup_ok"].as_u64().unwrap(), 2);
        assert_eq!(summary["match_lookup_failed"].as_u64().unwrap(), 0);
    }

    // Verifies that fetch, convert, match, and upload summaries coexist and partial
    // detection is based only on upload_send_error > 0.
    #[test]
    fn test_match_summary_merges_with_fetch_convert_and_upload_summary() {
        let fetch_counts = json!({"total": 5u64});
        let convert_counts = json!({
            "total": 5u64, "ok": 4u64, "skip_exists": 1u64, "dry_run": 0u64, "ffmpeg_error": 0u64
        });
        let match_counts = json!({
            "total": 4u64, "lookup_ok": 3u64, "lookup_failed": 1u64
        });
        let upload_counts = json!({
            "total": 3u64, "sent_ok": 1u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 2u64
        });
        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(Some(&fetch_counts)));
        merge_json_object(&mut summary, &summarize_convert_counts(Some(&convert_counts)));
        merge_json_object(&mut summary, &summarize_match_counts(Some(&match_counts)));
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_counts)));

        assert_eq!(summary["fetch_total"].as_u64().unwrap(), 5);
        assert_eq!(summary["convert_total"].as_u64().unwrap(), 5);
        assert_eq!(summary["convert_ok"].as_u64().unwrap(), 4);
        assert_eq!(summary["match_total"].as_u64().unwrap(), 4);
        assert_eq!(summary["match_lookup_ok"].as_u64().unwrap(), 3);
        assert_eq!(summary["match_lookup_failed"].as_u64().unwrap(), 1);
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 3);
        assert_eq!(summary["upload_sent_ok"].as_u64().unwrap(), 1);
        assert_eq!(summary["upload_send_error"].as_u64().unwrap(), 2);

        // Partial is triggered only by upload_send_error > 0, not by match counts.
        assert!(detect_partial_upload_success(&summary));
    }

    // --- Integrated pipeline summary composition tests ---

    // Verifies all 15 expected summary keys appear with correct values when all stages run.
    #[test]
    fn test_pipeline_summary_all_stage_keys_present() {
        let fetch_counts = json!({"total": 10u64});
        let convert_counts = json!({
            "total": 10u64, "ok": 8u64, "skip_exists": 1u64,
            "dry_run": 0u64, "ffmpeg_error": 1u64
        });
        let match_counts = json!({
            "total": 8u64, "lookup_ok": 7u64, "lookup_failed": 1u64
        });
        let upload_counts = json!({
            "total": 7u64, "sent_ok": 6u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 1u64
        });

        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(Some(&fetch_counts)));
        merge_json_object(&mut summary, &summarize_convert_counts(Some(&convert_counts)));
        merge_json_object(&mut summary, &summarize_match_counts(Some(&match_counts)));
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_counts)));

        assert_eq!(summary["fetch_total"].as_u64().unwrap(), 10);
        assert_eq!(summary["convert_total"].as_u64().unwrap(), 10);
        assert_eq!(summary["convert_ok"].as_u64().unwrap(), 8);
        assert_eq!(summary["convert_skip_exists"].as_u64().unwrap(), 1);
        assert_eq!(summary["convert_dry_run"].as_u64().unwrap(), 0);
        assert_eq!(summary["convert_ffmpeg_error"].as_u64().unwrap(), 1);
        assert_eq!(summary["match_total"].as_u64().unwrap(), 8);
        assert_eq!(summary["match_lookup_ok"].as_u64().unwrap(), 7);
        assert_eq!(summary["match_lookup_failed"].as_u64().unwrap(), 1);
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 7);
        assert_eq!(summary["upload_sent_ok"].as_u64().unwrap(), 6);
        assert_eq!(summary["upload_skipped_parse"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_skipped_validation"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_skipped_prepare"].as_u64().unwrap(), 0);
        assert_eq!(summary["upload_send_error"].as_u64().unwrap(), 1);

        assert!(detect_partial_upload_success(&summary), "upload_send_error == 1 must trigger partial");
    }

    // Verifies the merged summary contains no PII/path/payload keys and all values are numeric.
    #[test]
    fn test_pipeline_summary_no_pii_fields() {
        let fetch_counts  = json!({"total": 5u64});
        let convert_counts = json!({
            "total": 5u64, "ok": 5u64, "skip_exists": 0u64, "dry_run": 0u64, "ffmpeg_error": 0u64
        });
        let match_counts = json!({"total": 5u64, "lookup_ok": 5u64, "lookup_failed": 0u64});
        let upload_counts = json!({
            "total": 5u64, "sent_ok": 5u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 0u64
        });

        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(Some(&fetch_counts)));
        merge_json_object(&mut summary, &summarize_convert_counts(Some(&convert_counts)));
        merge_json_object(&mut summary, &summarize_match_counts(Some(&match_counts)));
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_counts)));

        let forbidden_keys = [
            "record_id", "url", "filename", "input", "output",
            "wav_path", "json_path", "telefono", "phone", "documento",
            "nombre", "client", "date", "run_id",
        ];
        if let Value::Object(map) = &summary {
            for key in map.keys() {
                assert!(
                    !forbidden_keys.contains(&key.as_str()),
                    "summary contains forbidden PII/path/payload key: {key}"
                );
            }
            for (key, val) in map.iter() {
                assert!(
                    val.is_number(),
                    "summary value for key '{key}' must be numeric, got: {val}"
                );
            }
        } else {
            panic!("summary must be a JSON object");
        }
    }

    // Verifies partial is controlled solely by upload_send_error and not by fetch/convert/match.
    #[test]
    fn test_partial_unaffected_by_fetch_convert_match_counts() {
        let fetch_counts   = json!({"total": 10u64});
        let convert_counts = json!({
            "total": 10u64, "ok": 8u64, "skip_exists": 0u64,
            "dry_run": 0u64, "ffmpeg_error": 2u64
        });
        let match_counts = json!({"total": 8u64, "lookup_ok": 5u64, "lookup_failed": 3u64});
        let upload_no_err = json!({
            "total": 5u64, "sent_ok": 5u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 0u64
        });

        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(Some(&fetch_counts)));
        merge_json_object(&mut summary, &summarize_convert_counts(Some(&convert_counts)));
        merge_json_object(&mut summary, &summarize_match_counts(Some(&match_counts)));
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_no_err)));

        assert_eq!(summary["convert_ffmpeg_error"].as_u64().unwrap(), 2,
            "convert_ffmpeg_error > 0 must not trigger partial");
        assert_eq!(summary["match_lookup_failed"].as_u64().unwrap(), 3,
            "match_lookup_failed > 0 must not trigger partial");
        assert!(!detect_partial_upload_success(&summary),
            "partial must be false when upload_send_error == 0");

        // Now re-merge with upload_send_error = 1; everything else stays the same.
        let upload_with_err = json!({
            "total": 5u64, "sent_ok": 4u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 1u64
        });
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_with_err)));

        assert!(detect_partial_upload_success(&summary),
            "partial must be true when upload_send_error == 1");
    }

    // Verifies that when all summarizers receive None, the merged summary is an empty object.
    #[test]
    fn test_pipeline_summary_empty_when_all_summarizers_none() {
        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(None));
        merge_json_object(&mut summary, &summarize_convert_counts(None));
        merge_json_object(&mut summary, &summarize_match_counts(None));
        merge_json_object(&mut summary, &summarize_upload_counts(None));

        assert!(summary.is_object(), "summary must be a JSON object");
        assert_eq!(
            summary.as_object().unwrap().len(),
            0,
            "summary must be empty when all summarizers receive None"
        );
        assert!(!detect_partial_upload_success(&summary),
            "partial must be false when summary is empty");
    }

    // Verifies schema_version remains 1 in PipelineReport even after aggregation fields exist.
    #[test]
    fn test_pipeline_summary_schema_version_remains_one() {
        let fetch_counts = json!({"total": 3u64});
        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(Some(&fetch_counts)));

        let mut report = synthetic_report();
        report.summary = summary;

        assert_eq!(report.schema_version, 1u32, "schema_version must remain 1");
        let v: Value = serde_json::to_value(&report).unwrap();
        assert_eq!(v["schema_version"].as_u64().unwrap(), 1u64,
            "schema_version must serialize as 1");
        assert_eq!(v["summary"]["fetch_total"].as_u64().unwrap(), 3,
            "fetch_total must survive serialization");
    }

    // Verifies that raw counts objects from all four summarizers contain only numeric values.
    #[test]
    fn test_stage_counts_objects_are_numeric_only() {
        let fetch_counts = json!({"total": 4u64});
        let convert_counts = json!({
            "total": 4u64, "ok": 3u64, "skip_exists": 1u64, "dry_run": 0u64, "ffmpeg_error": 0u64
        });
        let match_counts = json!({"total": 3u64, "lookup_ok": 3u64, "lookup_failed": 0u64});
        let upload_counts = json!({
            "total": 3u64, "sent_ok": 3u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 0u64
        });

        let summaries = [
            summarize_fetch_counts(Some(&fetch_counts)),
            summarize_convert_counts(Some(&convert_counts)),
            summarize_match_counts(Some(&match_counts)),
            summarize_upload_counts(Some(&upload_counts)),
        ];
        for (i, s) in summaries.iter().enumerate() {
            if let Value::Object(map) = s {
                for (key, val) in map.iter() {
                    assert!(
                        val.is_number(),
                        "counts object [{i}] key '{key}' must be numeric, got: {val}"
                    );
                }
            } else {
                panic!("counts object [{i}] must be a JSON object");
            }
        }
    }

    // Verifies that earlier summary keys survive when later summaries are merged,
    // and that a duplicate key in a later merge overwrites the earlier value.
    #[test]
    fn test_summary_merge_preserves_existing_keys() {
        let fetch_counts = json!({"total": 7u64});
        let convert_counts = json!({
            "total": 7u64, "ok": 6u64, "skip_exists": 1u64, "dry_run": 0u64, "ffmpeg_error": 0u64
        });
        let upload_counts = json!({
            "total": 5u64, "sent_ok": 5u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 0u64
        });

        let mut summary = Value::Object(serde_json::Map::new());
        merge_json_object(&mut summary, &summarize_fetch_counts(Some(&fetch_counts)));
        merge_json_object(&mut summary, &summarize_convert_counts(Some(&convert_counts)));
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_counts)));

        // Earlier keys survive subsequent merges.
        assert_eq!(summary["fetch_total"].as_u64().unwrap(), 7,
            "fetch_total must survive after convert and upload merges");
        assert_eq!(summary["convert_total"].as_u64().unwrap(), 7,
            "convert_total must survive after upload merge");
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 5);

        // A second upload merge with different values overwrites upload keys.
        let upload_counts2 = json!({
            "total": 9u64, "sent_ok": 8u64, "skipped_parse": 0u64,
            "skipped_validation": 0u64, "skipped_prepare": 0u64, "send_error": 1u64
        });
        merge_json_object(&mut summary, &summarize_upload_counts(Some(&upload_counts2)));
        assert_eq!(summary["upload_total"].as_u64().unwrap(), 9,
            "later merge must overwrite duplicate upload_total");
        assert_eq!(summary["upload_send_error"].as_u64().unwrap(), 1);
        // fetch_total must still be intact.
        assert_eq!(summary["fetch_total"].as_u64().unwrap(), 7,
            "fetch_total must not be disturbed by upload re-merge");
    }

    // --- extract_stderr_tail helper tests ---

    #[test]
    fn test_extract_stderr_tail_empty_returns_none() {
        assert!(extract_stderr_tail(b"").is_none(), "empty bytes must return None");
    }

    #[test]
    fn test_extract_stderr_tail_short_value_is_preserved() {
        let msg = b"error: connection refused\n";
        let result = extract_stderr_tail(msg).expect("non-empty stderr must return Some");
        assert_eq!(result, "error: connection refused\n");
    }

    #[test]
    fn test_extract_stderr_tail_long_value_is_truncated_to_tail() {
        let prefix = "X".repeat(STDERR_TAIL_LIMIT + 100);
        let suffix = "TAIL_MARKER_12345";
        let full = format!("{prefix}{suffix}");
        assert!(full.len() > STDERR_TAIL_LIMIT, "precondition: input longer than limit");
        let result = extract_stderr_tail(full.as_bytes())
            .expect("non-empty long stderr must return Some");
        // Truncation: result must be strictly shorter than the full input.
        assert!(
            result.len() < full.len(),
            "truncation must occur: result len {} must be less than full len {}",
            result.len(), full.len()
        );
        assert!(
            result.ends_with(suffix),
            "tail must preserve the end of input: got {result:?}"
        );
        assert!(
            result.len() <= STDERR_TAIL_LIMIT,
            "tail length {} must not exceed limit {}",
            result.len(), STDERR_TAIL_LIMIT
        );
    }

    #[test]
    fn test_extract_stderr_tail_exactly_at_limit_is_preserved() {
        let msg = "A".repeat(STDERR_TAIL_LIMIT);
        let result = extract_stderr_tail(msg.as_bytes()).expect("expected Some at exact limit");
        assert_eq!(result.len(), STDERR_TAIL_LIMIT);
    }

    // --- skipped stage stderr_tail invariant ---

    #[test]
    fn test_skipped_stage_stderr_tail_is_null() {
        let s = make_skipped_stage("convert", "audio-converter-rs", "manifests/convert.json");
        assert!(s.stderr_tail.is_none(), "skipped stage must have null stderr_tail");
    }

    // --- run_stage subprocess tests ---
    // Uses /bin/sh (available in the Rust test container on bookworm) with synthetic
    // commands only. No project pipeline stages, no external services, no PII.

    #[test]
    fn test_run_stage_failed_command_captures_stderr() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo 'stage error message' >&2; exit 1"]);
        let mut stage = make_pending_stage("test-fetch", "test-cmd", "manifests/test.json");
        let ok = run_stage(cmd, &mut stage);
        assert!(!ok, "run_stage must return false for non-zero exit");
        assert_eq!(stage.status, STAGE_STATUS_FAILED);
        let tail = stage.stderr_tail
            .expect("stderr_tail must be Some when stage fails with stderr output");
        assert!(
            tail.contains("stage error message"),
            "stderr_tail must contain the error text: got {tail:?}"
        );
    }

    #[test]
    fn test_run_stage_success_leaves_stderr_tail_null() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo 'stderr on success' >&2; exit 0"]);
        let mut stage = make_pending_stage("test-fetch", "test-cmd", "manifests/test.json");
        let ok = run_stage(cmd, &mut stage);
        assert!(ok, "run_stage must return true for zero exit");
        assert_eq!(stage.status, STAGE_STATUS_OK);
        assert!(
            stage.stderr_tail.is_none(),
            "stderr_tail must remain None when stage succeeds"
        );
    }

    #[test]
    fn test_run_stage_failed_no_stderr_leaves_tail_null() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exit 1"]);
        let mut stage = make_pending_stage("test-fetch", "test-cmd", "manifests/test.json");
        let ok = run_stage(cmd, &mut stage);
        assert!(!ok, "run_stage must return false for non-zero exit");
        assert_eq!(stage.status, STAGE_STATUS_FAILED);
        assert!(
            stage.stderr_tail.is_none(),
            "stderr_tail must remain None when no stderr output"
        );
    }
}

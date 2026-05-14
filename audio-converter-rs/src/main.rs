use anyhow::{Context, Result};
use audios_common::{paths, types::{ConvertItem, ConvertManifest, FetchManifest}, util};
use clap::Parser;
use chrono::Utc;
use std::{fs, path::{Path, PathBuf}, process::Command, sync::{Arc, Mutex, mpsc}};

const DEFAULT_CONVERSION_CONCURRENCY: usize = 2;

#[derive(Parser, Debug)]
#[command(name="audio-converter-rs")]
struct Args {
    #[arg(long)]
    client: String,
    #[arg(long)]
    date: String,
    #[arg(long, default_value="/shared")]
    shared_root: String,
    #[arg(long)]
    run_id: Option<String>,
    #[arg(long, default_value_t=false)]
    dry_run: bool,
    #[arg(long, default_value_t=8000)]
    sample_rate: u32,
    #[arg(long, default_value_t=1)]
    channels: u32,
    #[arg(long, default_value_t=DEFAULT_CONVERSION_CONCURRENCY)]
    conversion_concurrency: usize,
}

// parse_ffprobe_output parses the stdout bytes from a successful ffprobe run
// into a duration value. Extracted for unit-testability without requiring ffprobe.
fn parse_ffprobe_output(stdout: &[u8]) -> (bool, Option<f64>) {
    let s = String::from_utf8_lossy(stdout).trim().to_string();
    if s.is_empty() {
        return (false, None);
    }
    match s.parse::<f64>() {
        Ok(v) => (true, Some(v)),
        Err(_) => (false, None),
    }
}

fn probe_wav(path: &Path) -> (bool, Option<f64>) {
    // ffprobe must be available (installed with ffmpeg).
    let out = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output();

    let out = match out {
        Ok(o) => o,
        Err(_) => return (false, None),
    };
    if !out.status.success() {
        return (false, None);
    }
    parse_ffprobe_output(&out.stdout)
}

// output_wav_path derives the expected WAV output path for a fetched GSM filename.
// Extracted for unit-testability; preserves the exact same derivation as the main loop.
fn output_wav_path(wav_dir: &Path, filename: &str) -> PathBuf {
    let record_id = util::record_id_from_filename(filename);
    wav_dir.join(format!("{record_id}.wav"))
}

fn validate_conversion_concurrency(n: usize) -> Result<usize> {
    if n == 0 {
        anyhow::bail!("--conversion-concurrency must be >= 1; 0 is not a valid concurrency value");
    }
    Ok(n)
}

struct ConversionJob {
    index: usize,
    record_id: String,
    in_path: PathBuf,
    out_path: PathBuf,
    sample_rate: u32,
    channels: u32,
}

struct ConversionResult {
    index: usize,
    item: ConvertItem,
    ffprobe_failed: bool,
}

// convert_one processes a single conversion job and returns a ConversionResult.
// Checks skip_exists before dry_run, then invokes ffmpeg if needed.
// Invokes ffprobe when a valid output file is present after conversion.
// Never panics; ffmpeg/ffprobe failures produce error status values.
fn convert_one(
    index: usize,
    record_id: &str,
    in_path: &Path,
    out_path: &Path,
    sample_rate: u32,
    channels: u32,
    dry_run: bool,
) -> ConversionResult {
    let status: String;
    if out_path.exists() {
        status = "skip_exists".into();
    } else if dry_run {
        status = "dry_run".into();
    } else {
        let result = Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-i"])
            .arg(in_path)
            .args(["-ac", &channels.to_string(), "-ar", &sample_rate.to_string()])
            .arg(out_path)
            .status();
        status = match result {
            Ok(st) if st.success() => "ok".into(),
            _ => "ffmpeg_error".into(),
        };
    }

    let ffprobe_called = out_path.exists() && status != "dry_run" && status != "ffmpeg_error";
    let (ffprobe_ok, duration_sec, ffprobe_failed) = if ffprobe_called {
        let r = probe_wav(out_path);
        (r.0, r.1, !r.0)
    } else {
        (false, None, false)
    };

    ConversionResult {
        index,
        item: ConvertItem {
            record_id: record_id.to_string(),
            input: in_path.to_string_lossy().into_owned(),
            output: out_path.to_string_lossy().into_owned(),
            status,
            ffprobe_ok,
            duration_sec,
        },
        ffprobe_failed,
    }
}

// run_conversion_jobs dispatches conversion jobs to a bounded worker pool using only
// std primitives. Workers share a Mutex-guarded mpsc receiver and pull jobs until
// the queue drains. Results arrive in arbitrary completion order and are sorted by
// original index before returning, preserving deterministic manifest item ordering.
fn run_conversion_jobs(
    jobs: Vec<ConversionJob>,
    dry_run: bool,
    concurrency: usize,
    client_log: Arc<str>,
    date_log: Arc<str>,
) -> Vec<ConversionResult> {
    if jobs.is_empty() {
        return vec![];
    }
    let (job_tx, job_rx) = mpsc::channel::<ConversionJob>();
    let (res_tx, res_rx) = mpsc::channel::<ConversionResult>();
    let job_rx = Arc::new(Mutex::new(job_rx));
    let n_workers = concurrency.min(jobs.len());

    for _ in 0..n_workers {
        let job_rx = Arc::clone(&job_rx);
        let res_tx = res_tx.clone();
        let client_log = Arc::clone(&client_log);
        let date_log = Arc::clone(&date_log);
        std::thread::spawn(move || {
            loop {
                let job: ConversionJob = match job_rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => break,
                };
                let result = convert_one(
                    job.index,
                    &job.record_id,
                    &job.in_path,
                    &job.out_path,
                    job.sample_rate,
                    job.channels,
                    dry_run,
                );
                if result.item.status == "ffmpeg_error" {
                    tracing::warn!(
                        client = %client_log, date = %date_log,
                        "convert: ffmpeg error (1 file)"
                    );
                }
                let _ = res_tx.send(result);
            }
        });
    }
    // Drop the original sender; only worker-held clones remain.
    // When the last worker exits and drops its clone, res_rx exhausts.
    drop(res_tx);

    for job in jobs {
        let _ = job_tx.send(job);
    }
    // Signal workers: no more jobs once this sender is dropped and the queue drains.
    drop(job_tx);

    let mut results: Vec<ConversionResult> = res_rx.iter().collect();
    results.sort_by_key(|r| r.index);
    results
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = Args::parse();
    validate_conversion_concurrency(args.conversion_concurrency)?;

    let shared_root = PathBuf::from(&args.shared_root);
    let run_id = args.run_id.unwrap_or_else(|| format!("{}", Utc::now().format("%Y%m%dT%H%M%SZ")));
    let run_dir = paths::run_dir(&shared_root.join("runs"), &args.client, &args.date, &run_id);

    let manifests_dir = paths::manifests_dir(&run_dir);
    let raw_dir = paths::raw_dir(&run_dir);
    let wav_dir = paths::wav_dir(&run_dir);
    fs::create_dir_all(&wav_dir)?;
    fs::create_dir_all(&manifests_dir)?;

    let fetch_manifest_path = manifests_dir.join("fetch.json");
    let fetch_s = fs::read_to_string(&fetch_manifest_path)
        .with_context(|| format!("leyendo {}", fetch_manifest_path.display()))?;
    let fetch: FetchManifest = serde_json::from_str(&fetch_s).context("parse fetch.json")?;

    let fetch_total = fetch.items.len();
    let mut missing_input: usize = 0;
    let client_log: Arc<str> = Arc::from(args.client.as_str());
    let date_log: Arc<str> = Arc::from(args.date.as_str());

    tracing::info!(
        client = %args.client, date = %args.date,
        fetch_items = fetch_total, dry_run = args.dry_run,
        concurrency = args.conversion_concurrency,
        "convert: start"
    );

    let mut job_index = 0usize;
    let mut jobs: Vec<ConversionJob> = Vec::new();
    for it in fetch.items.iter() {
        let in_path = raw_dir.join(&it.filename);
        if !in_path.exists() {
            missing_input += 1;
            continue;
        }
        let record_id = util::record_id_from_filename(&it.filename);
        let out_path = output_wav_path(&wav_dir, &it.filename);
        jobs.push(ConversionJob {
            index: job_index,
            record_id,
            in_path,
            out_path,
            sample_rate: args.sample_rate,
            channels: args.channels,
        });
        job_index += 1;
    }

    let results = run_conversion_jobs(
        jobs,
        args.dry_run,
        args.conversion_concurrency,
        Arc::clone(&client_log),
        Arc::clone(&date_log),
    );

    let attempted = results.len();
    let mut count_ok: usize = 0;
    let mut count_skip_exists: usize = 0;
    let mut count_dry_run: usize = 0;
    let mut count_ffmpeg_error: usize = 0;
    let mut count_ffprobe_failed: usize = 0;
    let mut items: Vec<ConvertItem> = Vec::with_capacity(attempted);

    for r in results {
        match r.item.status.as_str() {
            "ok" => count_ok += 1,
            "skip_exists" => count_skip_exists += 1,
            "dry_run" => count_dry_run += 1,
            "ffmpeg_error" => count_ffmpeg_error += 1,
            _ => {}
        }
        if r.ffprobe_failed {
            count_ffprobe_failed += 1;
        }
        items.push(r.item);
    }

    if missing_input > 0 {
        tracing::warn!(
            client = %args.client, date = %args.date,
            count = missing_input,
            "convert: input files missing (not present in raw dir)"
        );
    }
    if count_ffprobe_failed > 0 {
        tracing::warn!(
            client = %args.client, date = %args.date,
            count = count_ffprobe_failed,
            "convert: ffprobe failed for some output files"
        );
    }
    tracing::info!(
        client = %args.client, date = %args.date,
        fetch_total, missing = missing_input, attempted,
        ok = count_ok, skip_exists = count_skip_exists,
        dry_run = count_dry_run, ffmpeg_error = count_ffmpeg_error,
        ffprobe_failed = count_ffprobe_failed,
        "convert: complete"
    );

    let manifest = ConvertManifest {
        schema_version: 1,
        client: args.client.clone(),
        date: args.date.clone(),
        run_id: run_id.clone(),
        generated_at: Utc::now(),
        items,
    };

    fs::write(manifests_dir.join("convert.json"), serde_json::to_string_pretty(&manifest)?)?;
    println!("run_dir={}", run_dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ─── parse_ffprobe_output ─────────────────────────────────────────────────────

    #[test]
    fn test_parse_ffprobe_output_valid_duration() {
        let (ok, dur) = parse_ffprobe_output(b"45.200000\n");
        assert!(ok);
        assert!((dur.unwrap() - 45.2).abs() < 1e-5);
    }

    #[test]
    fn test_parse_ffprobe_output_empty_bytes() {
        let (ok, dur) = parse_ffprobe_output(b"");
        assert!(!ok);
        assert!(dur.is_none());
    }

    #[test]
    fn test_parse_ffprobe_output_whitespace_only() {
        // Whitespace-only output (e.g. just a newline) trims to empty → (false, None).
        let (ok, dur) = parse_ffprobe_output(b"   \n");
        assert!(!ok);
        assert!(dur.is_none());
    }

    #[test]
    fn test_parse_ffprobe_output_invalid_float() {
        let (ok, dur) = parse_ffprobe_output(b"not-a-number\n");
        assert!(!ok);
        assert!(dur.is_none());
    }

    #[test]
    fn test_parse_ffprobe_output_zero_duration() {
        let (ok, dur) = parse_ffprobe_output(b"0.000000\n");
        assert!(ok);
        assert_eq!(dur, Some(0.0));
    }

    #[test]
    fn test_parse_ffprobe_output_integer_string() {
        // An integer string is a valid f64 parse target.
        let (ok, dur) = parse_ffprobe_output(b"120\n");
        assert!(ok);
        assert_eq!(dur, Some(120.0));
    }

    #[test]
    fn test_parse_ffprobe_output_trims_trailing_newline() {
        // ffprobe emits a trailing newline; trim must handle it.
        let (ok, dur) = parse_ffprobe_output(b"62.500000\n");
        assert!(ok);
        assert!((dur.unwrap() - 62.5).abs() < 1e-5);
    }

    // ─── output_wav_path ──────────────────────────────────────────────────────────

    #[test]
    fn test_output_wav_path_strips_gsm_extension() {
        let wav_dir = Path::new("/tmp/ops15-test-wav");
        let p = output_wav_path(wav_dir, "000000000_TEST_SYNTHETIC.gsm");
        assert_eq!(p.file_name().unwrap(), "000000000_TEST_SYNTHETIC.wav");
    }

    #[test]
    fn test_output_wav_path_strips_any_extension() {
        let wav_dir = Path::new("/tmp/ops15-test-wav");
        let p = output_wav_path(wav_dir, "synthetic-audio-001.OTHER");
        assert_eq!(p.file_name().unwrap(), "synthetic-audio-001.wav");
    }

    #[test]
    fn test_output_wav_path_no_extension() {
        // A filename with no dot produces record_id = the whole name → name.wav.
        let wav_dir = Path::new("/tmp/ops15-test-wav");
        let p = output_wav_path(wav_dir, "noextension");
        assert_eq!(p.file_name().unwrap(), "noextension.wav");
    }

    #[test]
    fn test_output_wav_path_result_is_under_wav_dir() {
        let wav_dir = Path::new("/tmp/ops15-test-wav");
        let p = output_wav_path(wav_dir, "synthetic-audio-001.gsm");
        assert!(p.starts_with(wav_dir));
    }

    #[test]
    fn test_output_wav_path_multiple_dots_strips_only_last_extension() {
        // record_id_from_filename uses rsplit_once, stripping only the last extension.
        let wav_dir = Path::new("/tmp/ops15-test-wav");
        let p = output_wav_path(wav_dir, "file.with.dots.gsm");
        assert_eq!(p.file_name().unwrap(), "file.with.dots.wav");
    }

    // ─── OPS-17: validate_conversion_concurrency ─────────────────────────────────

    #[test]
    fn test_conversion_concurrency_default_is_two() {
        assert_eq!(DEFAULT_CONVERSION_CONCURRENCY, 2);
    }

    #[test]
    fn test_validate_conversion_concurrency_accepts_one() {
        assert!(validate_conversion_concurrency(1).is_ok());
    }

    #[test]
    fn test_validate_conversion_concurrency_accepts_default() {
        assert!(validate_conversion_concurrency(DEFAULT_CONVERSION_CONCURRENCY).is_ok());
    }

    #[test]
    fn test_validate_conversion_concurrency_rejects_zero() {
        let err = validate_conversion_concurrency(0);
        assert!(err.is_err(), "concurrency=0 must be rejected");
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("must be >= 1"), "error must mention minimum: {msg}");
    }

    // ─── OPS-17: convert_one (no ffmpeg required) ────────────────────────────────

    #[test]
    fn test_convert_one_dry_run_produces_dry_run_status() {
        // dry_run=true with no out_path: status must be "dry_run", no output created.
        let dir = std::env::temp_dir()
            .join(format!("ops17_dryrun_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let in_path = dir.join("ops17-synthetic-input.gsm");
        let out_path = dir.join("ops17-synthetic-output.wav");

        let result = convert_one(0, "ops17-synthetic-001", &in_path, &out_path, 8000, 1, true);

        assert_eq!(result.item.status, "dry_run");
        assert_eq!(result.index, 0);
        assert!(!result.item.ffprobe_ok, "dry_run must not invoke ffprobe");
        assert!(result.item.duration_sec.is_none());
        assert!(!result.ffprobe_failed);
        assert!(!out_path.exists(), "dry_run must not create output file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_convert_one_skip_exists_when_output_present() {
        // When out_path already exists, skip_exists fires before dry_run or ffmpeg.
        let dir = std::env::temp_dir()
            .join(format!("ops17_skip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let in_path = dir.join("ops17-synthetic-input.gsm");
        let out_path = dir.join("ops17-synthetic-output.wav");
        std::fs::write(&out_path, b"synthetic wav placeholder").unwrap();

        // dry_run=false but out_path exists → skip_exists must fire before ffmpeg attempt
        let result = convert_one(1, "ops17-synthetic-002", &in_path, &out_path, 8000, 1, false);

        assert_eq!(result.item.status, "skip_exists",
            "existing output must produce skip_exists regardless of dry_run flag");
        assert_eq!(result.index, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_convert_one_skip_exists_takes_priority_over_dry_run() {
        // Even with dry_run=true, skip_exists fires first when out_path exists.
        let dir = std::env::temp_dir()
            .join(format!("ops17_skip_dryrun_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let in_path = dir.join("ops17-synthetic-input.gsm");
        let out_path = dir.join("ops17-synthetic-output.wav");
        std::fs::write(&out_path, b"synthetic wav placeholder").unwrap();

        let result = convert_one(0, "ops17-synthetic-003", &in_path, &out_path, 8000, 1, true);

        assert_eq!(result.item.status, "skip_exists");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── OPS-17: run_conversion_jobs ─────────────────────────────────────────────

    fn test_logs() -> (Arc<str>, Arc<str>) {
        (Arc::from("test-client"), Arc::from("2026-05-14"))
    }

    #[test]
    fn test_run_conversion_jobs_empty_returns_empty() {
        let (cl, dl) = test_logs();
        let results = run_conversion_jobs(vec![], false, 2, cl, dl);
        assert!(results.is_empty());
    }

    #[test]
    fn test_run_conversion_jobs_dry_run_all_produce_dry_run_status() {
        // dry_run=true, no out_paths exist → all items must have status "dry_run".
        let (cl, dl) = test_logs();
        let dir = std::env::temp_dir()
            .join(format!("ops17_jobs_dryrun_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let n = 5usize;
        let jobs: Vec<ConversionJob> = (0..n).map(|i| ConversionJob {
            index: i,
            record_id: format!("ops17-dr-{:03}", i),
            in_path: dir.join(format!("ops17-in-{:03}.gsm", i)),
            out_path: dir.join(format!("ops17-out-{:03}.wav", i)),
            sample_rate: 8000,
            channels: 1,
        }).collect();

        let results = run_conversion_jobs(jobs, true, 2, cl, dl);

        assert_eq!(results.len(), n);
        for r in &results {
            assert_eq!(r.item.status, "dry_run",
                "index {} must have dry_run status", r.index);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_conversion_jobs_deterministic_order_concurrent_workers() {
        // With concurrency > 1, worker completion order is non-deterministic.
        // Results must match discovery order (by index) after sort-by-index.
        let (cl, dl) = test_logs();
        let dir = std::env::temp_dir()
            .join(format!("ops17_order_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let filenames = [
            "ops17-zebra.gsm",
            "ops17-alpha.gsm",
            "ops17-middle.gsm",
            "ops17-delta.gsm",
            "ops17-echo.gsm",
            "ops17-foxtrot.gsm",
        ];
        let jobs: Vec<ConversionJob> = filenames.iter().enumerate().map(|(i, name)| ConversionJob {
            index: i,
            record_id: format!("ops17-order-{:03}", i),
            in_path: dir.join(name),
            out_path: dir.join(name.replace(".gsm", ".wav")),
            sample_rate: 8000,
            channels: 1,
        }).collect();

        // concurrency=4 so multiple workers compete, producing non-deterministic channel
        // arrival order. sort-by-index must restore discovery order.
        let results = run_conversion_jobs(jobs, true, 4, cl, dl);

        assert_eq!(results.len(), filenames.len());
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.index, i, "result[{i}].index must equal {i} (stable ordering)");
            assert_eq!(r.item.status, "dry_run");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_conversion_jobs_skip_existing_output() {
        // When out_path already exists for a job, status must be skip_exists.
        let (cl, dl) = test_logs();
        let dir = std::env::temp_dir()
            .join(format!("ops17_skip_existing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let out_path = dir.join("ops17-existing.wav");
        std::fs::write(&out_path, b"synthetic wav placeholder").unwrap();

        let jobs = vec![ConversionJob {
            index: 0,
            record_id: "ops17-skip-001".to_string(),
            in_path: dir.join("ops17-existing.gsm"),
            out_path: out_path.clone(),
            sample_rate: 8000,
            channels: 1,
        }];

        let results = run_conversion_jobs(jobs, false, 1, cl, dl);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item.status, "skip_exists");
        assert_eq!(results[0].index, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

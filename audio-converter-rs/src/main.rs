use anyhow::{Context, Result};
use audios_common::{paths, types::{ConvertItem, ConvertManifest, FetchManifest}, util};
use clap::Parser;
use chrono::Utc;
use std::{fs, path::{Path, PathBuf}, process::Command};

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

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = Args::parse();

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
    let mut attempted: usize = 0;
    let mut count_ok: usize = 0;
    let mut count_skip_exists: usize = 0;
    let mut count_dry_run: usize = 0;
    let mut count_ffmpeg_error: usize = 0;
    let mut count_ffprobe_failed: usize = 0;

    tracing::info!(
        client = %args.client, date = %args.date,
        fetch_items = fetch_total, dry_run = args.dry_run,
        "convert: start"
    );

    let mut items = vec![];
    for it in fetch.items.iter() {
        let in_path = raw_dir.join(&it.filename);
        if !in_path.exists() {
            missing_input += 1;
            continue;
        }
        attempted += 1;

        let record_id = util::record_id_from_filename(&it.filename);
        let out_path = output_wav_path(&wav_dir, &it.filename);

        let status: String;
        if out_path.exists() {
            status = "skip_exists".into();
            count_skip_exists += 1;
        } else if args.dry_run {
            status = "dry_run".into();
            count_dry_run += 1;
        } else {
            let st = Command::new("ffmpeg")
                .args(["-nostdin", "-y", "-i"])
                .arg(&in_path)
                .args(["-ac", &args.channels.to_string(), "-ar", &args.sample_rate.to_string()])
                .arg(&out_path)
                .status()
                .with_context(|| "ejecutando ffmpeg")?;
            if st.success() {
                status = "ok".into();
                count_ok += 1;
            } else {
                status = "ffmpeg_error".into();
                count_ffmpeg_error += 1;
            }
        }

        let ffprobe_called = out_path.exists() && status != "dry_run" && status != "ffmpeg_error";
        let (ffprobe_ok, duration_sec) = if ffprobe_called {
            let result = probe_wav(&out_path);
            if !result.0 {
                count_ffprobe_failed += 1;
            }
            result
        } else {
            (false, None)
        };

        items.push(ConvertItem{
            record_id,
            input: in_path.to_string_lossy().to_string(),
            output: out_path.to_string_lossy().to_string(),
            status,
            ffprobe_ok,
            duration_sec,
        });
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
}

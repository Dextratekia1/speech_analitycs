use anyhow::{Context, Result};
use audios_common::{paths, types::{ConvertItem, ConvertManifest, FetchManifest}, util};
use clap::Parser;
use chrono::Utc;
use std::{fs, path::PathBuf, process::Command};

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

fn probe_wav(path: &std::path::Path) -> (bool, Option<f64>) {
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
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return (false, None);
    }
    match s.parse::<f64>() {
        Ok(v) => (true, Some(v)),
        Err(_) => (false, None),
    }
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

    let mut items = vec![];
    for it in fetch.items.iter() {
        let in_path = raw_dir.join(&it.filename);
        if !in_path.exists() { continue; }

        let record_id = util::record_id_from_filename(&it.filename);
        let out_path = wav_dir.join(format!("{record_id}.wav"));

        let status: String;
        if out_path.exists() {
            status = "skip_exists".into();
        } else if args.dry_run {
            status = "dry_run".into();
        } else {
            let st = Command::new("ffmpeg")
                .args(["-y","-i"])
                .arg(&in_path)
                .args(["-ac", &args.channels.to_string(), "-ar", &args.sample_rate.to_string()])
                .arg(&out_path)
                .status()
                .with_context(|| "ejecutando ffmpeg")?;
            status = if st.success() { "ok" } else { "ffmpeg_error" }.into();
        }

        let (ffprobe_ok, duration_sec) = if out_path.exists() && status != "dry_run" && status != "ffmpeg_error" {
            probe_wav(&out_path)
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

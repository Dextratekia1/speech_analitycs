use anyhow::{Context, Result};
use audios_common::{paths, util};
use clap::Parser;
use chrono::Utc;
use std::{fs, path::PathBuf, process::Command};

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

fn run_cmd(mut cmd: Command, label: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("running {label}"))?;
    if !status.success() {
        anyhow::bail!("{label} failed: {status}");
    }
    Ok(())
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

    // 1) fetch
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
    run_cmd(c, "audio-fetcher-rs")?;

    // 2) convert
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
    run_cmd(c, "audio-converter-rs")?;

    // 3) match
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
    run_cmd(c, "metadata-matcher-rs")?;

    // 4) upload
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
    run_cmd(c, "audio-uploader-go")?;

    println!("run_dir={}", run_dir.display());
    Ok(())
}

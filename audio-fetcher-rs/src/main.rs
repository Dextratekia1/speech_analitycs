use anyhow::{Context, Result};
use audios_common::{config::ClientConfigFile, paths, types::{FetchItem, FetchManifest}, util};
use clap::Parser;
use chrono::Utc;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::{fs, path::{Path, PathBuf}};

#[derive(Parser, Debug)]
#[command(name="audio-fetcher-rs")]
struct Args {
    #[arg(long)]
    client: String,
    #[arg(long)]
    date: String,
    #[arg(long, default_value="/shared")]
    shared_root: String,
    #[arg(long, default_value="config/clients")]
    clients_dir: String,
    #[arg(long)]
    run_id: Option<String>,
    #[arg(long, default_value_t=false)]
    dry_run: bool,
}

fn load_client_cfg(shared_root: &Path, clients_dir: &str, client: &str) -> Result<ClientConfigFile> {
    let p = shared_root.join(clients_dir).join(format!("{client}.yml"));
    let s = fs::read_to_string(&p).with_context(|| format!("leyendo config cliente: {}", p.display()))?;
    let cfg: ClientConfigFile = serde_yaml::from_str(&s).context("parse yaml cliente")?;
    Ok(cfg)
}

fn list_links(html: &str, exts: &[String]) -> Vec<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("a").unwrap();
    let mut out = vec![];
    for a in doc.select(&sel) {
        if let Some(href) = a.value().attr("href") {
            if href.ends_with('/') { continue; }
            if exts.iter().any(|e| href.to_lowercase().ends_with(&format!(".{}", e.to_lowercase()))) {
                out.push(href.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn download_file(client: &Client, url: &str, out_path: &Path, dry_run: bool) -> Result<u64> {
    if out_path.exists() {
        return Ok(out_path.metadata().ok().map(|m| m.len()).unwrap_or(0));
    }
    if dry_run {
        return Ok(0);
    }
    if let Some(parent) = out_path.parent() { fs::create_dir_all(parent)?; }
    let resp = client.get(url).send().with_context(|| format!("GET {url}"))?;
    resp.error_for_status_ref().with_context(|| format!("status no OK {url}"))?;
    let bytes = resp.bytes().context("leer body")?;
    fs::write(out_path, &bytes)?;
    Ok(bytes.len() as u64)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = Args::parse();

    let shared_root = PathBuf::from(&args.shared_root);
    let cfg = load_client_cfg(&shared_root, &args.clients_dir, &args.client)?;
    let date = util::parse_date_ymd(&args.date)?;
    let run_id = args.run_id.unwrap_or_else(|| format!("{}", Utc::now().format("%Y%m%dT%H%M%SZ")));

    let run_dir = paths::run_dir(&shared_root.join("runs"), &args.client, &args.date, &run_id);
    let raw_dir = paths::raw_dir(&run_dir);
    let manifests_dir = paths::manifests_dir(&run_dir);
    fs::create_dir_all(&raw_dir)?;
    fs::create_dir_all(&manifests_dir)?;

    let base_url = cfg.fetch.base_url_template.replace("{client}", &args.client);
    let http = Client::builder().timeout(std::time::Duration::from_secs(60)).build()?;

    let mut items: Vec<FetchItem> = vec![];

    for src_t in cfg.fetch.sources.iter() {
        let src = util::expand_source_template(src_t, &date);
        let url_dir = format!("{base_url}{src}/");
        let html = match http.get(&url_dir).send() {
            Ok(r) => r.error_for_status().ok().and_then(|x| x.text().ok()).unwrap_or_default(),
            Err(_) => String::new(),
        };
        if html.is_empty() { continue; }

        for filename in list_links(&html, &cfg.fetch.extensions) {
            let url = format!("{url_dir}{filename}");
            let out_path = raw_dir.join(&filename);
            let bytes = download_file(&http, &url, &out_path, args.dry_run).unwrap_or(0);
            items.push(FetchItem{ url, filename, bytes: Some(bytes), status: "ok".into() });
        }
    }

    let manifest = FetchManifest {
        schema_version: 1,
        client: args.client.clone(),
        date: args.date.clone(),
        run_id: run_id.clone(),
        generated_at: Utc::now(),
        items,
    };
    fs::write(manifests_dir.join("fetch.json"), serde_json::to_string_pretty(&manifest)?)?;
    println!("run_dir={}", run_dir.display());
    Ok(())
}

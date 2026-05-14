use anyhow::{Context, Result};
use audios_common::{config::ClientConfigFile, paths, types::{FetchItem, FetchManifest}, util};
use clap::Parser;
use chrono::Utc;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::{fs, path::{Path, PathBuf}, sync::{Arc, Mutex, mpsc}};

const DEFAULT_DOWNLOAD_CONCURRENCY: usize = 4;

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
    #[arg(long, default_value_t=DEFAULT_DOWNLOAD_CONCURRENCY)]
    download_concurrency: usize,
}

struct DownloadJob {
    index: usize,
    url: String,
    filename: String,
    out_path: PathBuf,
    already_exists: bool,
}

struct JobResult {
    index: usize,
    item: FetchItem,
    already_existed: bool,
    was_downloaded: bool,
    was_failed: bool,
    bytes: u64,
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

fn validate_concurrency(n: usize) -> Result<usize> {
    if n == 0 {
        anyhow::bail!("--download-concurrency must be >= 1; 0 is not a valid concurrency value");
    }
    Ok(n)
}

// Dispatches download jobs to a bounded worker pool using only std primitives.
// Workers share `http` (Arc) and pull jobs from a Mutex-guarded mpsc channel.
// Results arrive in arbitrary completion order and are sorted by discovery index
// before returning, preserving deterministic manifest item ordering.
fn run_download_jobs(
    http: Arc<Client>,
    jobs: Vec<DownloadJob>,
    dry_run: bool,
    concurrency: usize,
    client_log: Arc<str>,
    date_log: Arc<str>,
) -> Vec<JobResult> {
    if jobs.is_empty() {
        return vec![];
    }
    let (job_tx, job_rx) = mpsc::channel::<DownloadJob>();
    let (res_tx, res_rx) = mpsc::channel::<JobResult>();
    let job_rx = Arc::new(Mutex::new(job_rx));
    let n_workers = concurrency.min(jobs.len());

    for _ in 0..n_workers {
        let http = Arc::clone(&http);
        let job_rx = Arc::clone(&job_rx);
        let res_tx = res_tx.clone();
        let client_log = Arc::clone(&client_log);
        let date_log = Arc::clone(&date_log);
        std::thread::spawn(move || {
            loop {
                let job: DownloadJob = match job_rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => break,
                };
                let already_existed = job.already_exists;
                let result = download_file(&http, &job.url, &job.out_path, dry_run);
                let (bytes, was_downloaded, was_failed) = match result {
                    Ok(b) => {
                        let downloaded = !already_existed && !dry_run;
                        (b, downloaded, false)
                    }
                    Err(_) => {
                        tracing::warn!(
                            client = %client_log, date = %date_log,
                            "fetch: download error (1 file)"
                        );
                        (0u64, false, true)
                    }
                };
                let _ = res_tx.send(JobResult {
                    index: job.index,
                    item: FetchItem {
                        url: job.url,
                        filename: job.filename,
                        bytes: Some(bytes),
                        status: "ok".to_string(),
                    },
                    already_existed,
                    was_downloaded,
                    was_failed,
                    bytes,
                });
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

    let mut results: Vec<JobResult> = res_rx.iter().collect();
    results.sort_by_key(|r| r.index);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ─── list_links ──────────────────────────────────────────────────────────────

    #[test]
    fn test_list_links_extracts_gsm_links() {
        let html = r#"<html><body>
            <a href="synthetic-audio-001.gsm">f1</a>
            <a href="synthetic-audio-002.gsm">f2</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string()]);
        assert_eq!(links, vec!["synthetic-audio-001.gsm", "synthetic-audio-002.gsm"]);
    }

    #[test]
    fn test_list_links_ignores_non_gsm() {
        let html = r#"<html><body>
            <a href="audio.gsm">gsm</a>
            <a href="audio.mp3">mp3</a>
            <a href="readme.txt">txt</a>
            <a href="audio.wav">wav</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string()]);
        assert_eq!(links, vec!["audio.gsm"]);
    }

    #[test]
    fn test_list_links_ignores_directory_links() {
        // href ending with '/' is a directory listing entry and must be excluded.
        let html = r#"<html><body>
            <a href="subdir/">directory link</a>
            <a href="tipo1_synthetic.gsm">file link</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string()]);
        assert_eq!(links, vec!["tipo1_synthetic.gsm"]);
    }

    #[test]
    fn test_list_links_case_insensitive_extension() {
        // Extension check is lowercase-normalised on both sides.
        let html = r#"<html><body>
            <a href="SYNTHETIC_AUDIO.GSM">uppercase</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string()]);
        assert_eq!(links, vec!["SYNTHETIC_AUDIO.GSM"]);
    }

    #[test]
    fn test_list_links_deduplicates() {
        let html = r#"<html><body>
            <a href="synthetic.gsm">first</a>
            <a href="synthetic.gsm">duplicate</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string()]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0], "synthetic.gsm");
    }

    #[test]
    fn test_list_links_empty_html() {
        let links = list_links("", &["gsm".to_string()]);
        assert!(links.is_empty());
    }

    #[test]
    fn test_list_links_sorted_output() {
        let html = r#"<html><body>
            <a href="zebra_synthetic.gsm">z</a>
            <a href="alpha_synthetic.gsm">a</a>
            <a href="middle_synthetic.gsm">m</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string()]);
        assert_eq!(links, vec![
            "alpha_synthetic.gsm",
            "middle_synthetic.gsm",
            "zebra_synthetic.gsm",
        ]);
    }

    #[test]
    fn test_list_links_multiple_extensions() {
        let html = r#"<html><body>
            <a href="audio.gsm">gsm</a>
            <a href="audio.wav">wav</a>
            <a href="audio.mp3">mp3</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string(), "wav".to_string()]);
        assert_eq!(links, vec!["audio.gsm", "audio.wav"]);
    }

    #[test]
    fn test_list_links_anchor_without_href_ignored() {
        let html = r#"<html><body>
            <a name="anchor">no href here</a>
            <a href="tipo1_synthetic.gsm">has href</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string()]);
        assert_eq!(links, vec!["tipo1_synthetic.gsm"]);
    }

    #[test]
    fn test_list_links_no_matching_extension() {
        // HTML with links but none matching the extension filter.
        let html = r#"<html><body>
            <a href="archive.zip">zip</a>
            <a href="index.html">html</a>
        </body></html>"#;
        let links = list_links(html, &["gsm".to_string()]);
        assert!(links.is_empty());
    }

    // ─── download_file: no-network paths ─────────────────────────────────────────

    #[test]
    fn test_download_file_existing_file_skips_download() {
        // When the output file already exists, download_file returns its size
        // without making any network request.
        let dir = std::env::temp_dir()
            .join(format!("ops15_fetcher_exists_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out_path = dir.join("synthetic-existing.gsm");
        let content = b"synthetic gsm placeholder bytes";
        std::fs::write(&out_path, content).unwrap();

        let client = reqwest::blocking::Client::new();
        // The URL is intentionally unreachable; the file-exists branch must fire first.
        let result = download_file(&client, "http://127.0.0.1:1/unreachable", &out_path, false);

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        assert_eq!(result.unwrap(), content.len() as u64);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_download_file_dry_run_returns_zero() {
        // When dry_run=true and the file does not exist, download_file returns 0
        // without creating the file or making any network request.
        let out_path = std::env::temp_dir()
            .join(format!("ops15_fetcher_dryrun_{}", std::process::id()))
            .join("synthetic-nonexistent.gsm");
        // Ensure the file does not exist from a previous run.
        let _ = std::fs::remove_file(&out_path);

        let client = reqwest::blocking::Client::new();
        let result = download_file(&client, "http://127.0.0.1:1/unreachable", &out_path, true);

        assert!(result.is_ok(), "expected Ok, got {:?}", result);
        assert_eq!(result.unwrap(), 0u64);
        assert!(!out_path.exists(), "dry_run must not create the output file");
    }

    // ─── download_file: localhost HTTP server ─────────────────────────────────────

    #[test]
    fn test_download_file_writes_content_from_localhost() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let payload = b"synthetic gsm bytes for ops15 download test";
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );

        // Bind before spawning so the port is known before the client attempts to connect.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let payload_owned = payload.to_vec();
        let header_owned = header;
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf); // consume the HTTP request
                let _ = stream.write_all(header_owned.as_bytes());
                let _ = stream.write_all(&payload_owned);
                // stream drops here, closing the connection
            }
        });

        let url = format!("http://127.0.0.1:{port}/synthetic-audio-001.gsm");
        let out_dir = std::env::temp_dir()
            .join(format!("ops15_fetcher_dl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);
        let out_path = out_dir.join("synthetic-audio-001.gsm");

        let client = reqwest::blocking::Client::new();
        let result = download_file(&client, &url, &out_path, false);

        assert!(result.is_ok(), "download_file returned error: {:?}", result);
        assert_eq!(result.unwrap(), payload.len() as u64);
        let written = std::fs::read(&out_path).unwrap();
        assert_eq!(written, payload, "written content must match the served payload");

        let _ = std::fs::remove_dir_all(&out_dir);
    }

    // ─── OPS-16: run_download_jobs ────────────────────────────────────────────────

    fn make_http() -> Arc<Client> {
        Arc::new(reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap())
    }

    fn test_logs() -> (Arc<str>, Arc<str>) {
        (Arc::from("test-client"), Arc::from("2026-05-13"))
    }

    #[test]
    fn test_download_concurrency_default_is_four() {
        assert_eq!(DEFAULT_DOWNLOAD_CONCURRENCY, 4);
    }

    #[test]
    fn test_validate_concurrency_accepts_one() {
        assert!(validate_concurrency(1).is_ok());
    }

    #[test]
    fn test_validate_concurrency_accepts_default() {
        assert!(validate_concurrency(DEFAULT_DOWNLOAD_CONCURRENCY).is_ok());
    }

    #[test]
    fn test_validate_concurrency_rejects_zero() {
        let err = validate_concurrency(0);
        assert!(err.is_err(), "concurrency=0 must be rejected");
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("must be >= 1"), "error must mention minimum: {msg}");
    }

    #[test]
    fn test_run_download_jobs_empty_jobs_returns_empty() {
        let (cl, dl) = test_logs();
        let results = run_download_jobs(make_http(), vec![], false, 4, cl, dl);
        assert!(results.is_empty());
    }

    #[test]
    fn test_run_download_jobs_dry_run_creates_no_files() {
        // dry_run=true: no network contact, no files written, bytes=0 for all items.
        let (cl, dl) = test_logs();
        let out_dir = std::env::temp_dir()
            .join(format!("ops16_dryrun_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);

        let jobs: Vec<DownloadJob> = (0..4usize).map(|i| {
            let filename = format!("ops16-synthetic-{:03}.gsm", i);
            DownloadJob {
                index: i,
                url: "http://127.0.0.1:1/unreachable".to_string(),
                filename: filename.clone(),
                out_path: out_dir.join(&filename),
                already_exists: false,
            }
        }).collect();

        let results = run_download_jobs(make_http(), jobs, true, 2, cl, dl);

        assert_eq!(results.len(), 4);
        for i in 0..4usize {
            assert_eq!(results[i].index, i, "items must be in index order");
            assert_eq!(results[i].bytes, 0, "dry_run bytes must be 0");
            assert!(!results[i].was_downloaded, "dry_run must not mark as downloaded");
            assert!(!results[i].was_failed, "dry_run must not mark as failed");
            let path = out_dir.join(format!("ops16-synthetic-{:03}.gsm", i));
            assert!(!path.exists(), "dry_run must not create file {}", path.display());
        }
    }

    #[test]
    fn test_run_download_jobs_deterministic_order_with_concurrent_workers() {
        // Jobs submitted in intentional non-alphabetical/non-index order.
        // With concurrency > 1, worker completion order is non-deterministic.
        // Results must match discovery order (by index) after sort-by-index.
        let (cl, dl) = test_logs();
        let filenames = [
            "zebra-synthetic.gsm",
            "alpha-synthetic.gsm",
            "ops16-middle.gsm",
            "ops16-d.gsm",
            "ops16-e.gsm",
        ];
        let out_dir = std::env::temp_dir()
            .join(format!("ops16_order_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);

        let jobs: Vec<DownloadJob> = filenames.iter().enumerate().map(|(i, name)| {
            DownloadJob {
                index: i,
                url: "http://127.0.0.1:1/unreachable".to_string(),
                filename: name.to_string(),
                out_path: out_dir.join(name),
                already_exists: false,
            }
        }).collect();

        // concurrency=3 so multiple workers compete for jobs, producing non-deterministic
        // channel arrival order. sort-by-index must restore discovery order.
        let results = run_download_jobs(make_http(), jobs, true, 3, cl, dl);

        assert_eq!(results.len(), filenames.len());
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.index, i, "result[{i}].index must equal {i}");
            assert_eq!(r.item.filename, filenames[i],
                "result[{i}].filename must be {:?} (discovery order), not alphabetical",
                filenames[i]);
        }
    }

    #[test]
    fn test_run_download_jobs_skip_existing_returns_size() {
        // When the output file already exists, download_file returns its size.
        // JobResult must reflect already_existed=true, was_downloaded=false.
        let (cl, dl) = test_logs();
        let dir = std::env::temp_dir()
            .join(format!("ops16_skip_existing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let content = b"synthetic existing gsm content";
        let out_path = dir.join("ops16-existing.gsm");
        std::fs::write(&out_path, content).unwrap();

        let jobs = vec![DownloadJob {
            index: 0,
            url: "http://127.0.0.1:1/unreachable".to_string(),
            filename: "ops16-existing.gsm".to_string(),
            out_path: out_path.clone(),
            already_exists: true,
        }];

        let results = run_download_jobs(make_http(), jobs, false, 1, cl, dl);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].bytes, content.len() as u64);
        assert!(results[0].already_existed, "already_existed must be true");
        assert!(!results[0].was_downloaded, "was_downloaded must be false for skip");
        assert!(!results[0].was_failed, "was_failed must be false for skip");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_download_jobs_bounded_concurrency() {
        // Verify that at most `concurrency_limit` downloads are active simultaneously.
        // Server tracks concurrent active connections via AtomicUsize high-watermark.
        // Workers are bounded so the high-watermark must not exceed the limit.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let concurrency_limit = 2usize;
        let n_jobs = 6usize;
        let active = Arc::new(AtomicUsize::new(0));
        let high_watermark = Arc::new(AtomicUsize::new(0));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let active_srv = Arc::clone(&active);
        let hw_srv = Arc::clone(&high_watermark);
        let payload = b"synthetic gsm bytes for ops16 bounded concurrency test";
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );

        std::thread::spawn(move || {
            for _ in 0..n_jobs {
                if let Ok((mut stream, _)) = listener.accept() {
                    let active = Arc::clone(&active_srv);
                    let hw = Arc::clone(&hw_srv);
                    let header = header.clone();
                    let payload = payload.to_vec();
                    std::thread::spawn(move || {
                        // Count this connection as active and update high-watermark.
                        let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                        hw.fetch_max(cur, Ordering::SeqCst);
                        // Delay so concurrent workers accumulate measurably.
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&payload);
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            }
        });

        let out_dir = std::env::temp_dir()
            .join(format!("ops16_bounded_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir).unwrap();

        let (cl, dl) = test_logs();
        let jobs: Vec<DownloadJob> = (0..n_jobs).map(|i| {
            let filename = format!("ops16-bounded-{:03}.gsm", i);
            DownloadJob {
                index: i,
                url: format!("http://127.0.0.1:{port}/{filename}"),
                filename: filename.clone(),
                out_path: out_dir.join(&filename),
                already_exists: false,
            }
        }).collect();

        let results = run_download_jobs(make_http(), jobs, false, concurrency_limit, cl, dl);

        assert_eq!(results.len(), n_jobs, "all {n_jobs} jobs must produce a result");
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.index, i, "result[{i}] must be in discovery order");
        }

        let hw = high_watermark.load(Ordering::SeqCst);
        assert!(
            hw <= concurrency_limit,
            "concurrent server connections ({hw}) must not exceed download concurrency limit ({concurrency_limit})"
        );

        let _ = std::fs::remove_dir_all(&out_dir);
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = Args::parse();
    validate_concurrency(args.download_concurrency)?;

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
    let http = Arc::new(Client::builder().timeout(std::time::Duration::from_secs(60)).build()?);
    let client_log: Arc<str> = Arc::from(args.client.as_str());
    let date_log: Arc<str> = Arc::from(args.date.as_str());

    let mut all_jobs: Vec<DownloadJob> = vec![];
    let mut job_index = 0usize;
    let mut sources_processed = 0usize;
    let mut links_discovered = 0usize;

    tracing::info!(
        client = %args.client, date = %args.date,
        sources = cfg.fetch.sources.len(), dry_run = args.dry_run,
        concurrency = args.download_concurrency,
        "fetch: start"
    );

    for src_t in cfg.fetch.sources.iter() {
        let src = util::expand_source_template(src_t, &date);
        let url_dir = format!("{base_url}{src}/");
        let html = match http.get(&url_dir).send() {
            Err(_) => {
                tracing::warn!(client = %args.client, date = %args.date, "fetch: request error on source directory listing");
                String::new()
            }
            Ok(r) => match r.error_for_status() {
                Err(_) => {
                    tracing::warn!(client = %args.client, date = %args.date, "fetch: HTTP error on source directory listing");
                    String::new()
                }
                Ok(resp) => resp.text().unwrap_or_default(),
            },
        };
        if html.is_empty() { continue; }
        sources_processed += 1;

        let links = list_links(&html, &cfg.fetch.extensions);
        let src_links = links.len();
        links_discovered += src_links;
        tracing::info!(client = %args.client, date = %args.date, links = src_links, "fetch: source links discovered");

        for filename in links {
            let url = format!("{url_dir}{filename}");
            let out_path = raw_dir.join(&filename);
            let already_exists = out_path.exists();
            all_jobs.push(DownloadJob {
                index: job_index,
                url,
                filename,
                out_path,
                already_exists,
            });
            job_index += 1;
        }
    }

    let results = run_download_jobs(
        http,
        all_jobs,
        args.dry_run,
        args.download_concurrency,
        client_log,
        date_log,
    );

    let mut items: Vec<FetchItem> = Vec::with_capacity(results.len());
    let mut files_downloaded = 0usize;
    let mut files_skipped = 0usize;
    let mut files_failed = 0usize;
    let mut total_bytes = 0u64;

    for r in results {
        if r.was_failed {
            files_failed += 1;
        } else if r.already_existed {
            files_skipped += 1;
        } else if r.was_downloaded {
            files_downloaded += 1;
        }
        total_bytes += r.bytes;
        items.push(r.item);
    }

    tracing::info!(
        client = %args.client, date = %args.date,
        sources = sources_processed, links = links_discovered,
        downloaded = files_downloaded, skipped = files_skipped,
        failed = files_failed, bytes = total_bytes,
        "fetch: complete"
    );

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

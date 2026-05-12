use std::path::{Path, PathBuf};

pub fn run_dir(runs_root: &Path, client: &str, date: &str, run_id: &str) -> PathBuf {
    runs_root.join(client).join(date).join(run_id)
}

pub fn raw_dir(run_dir: &Path) -> PathBuf { run_dir.join("raw") }
pub fn wav_dir(run_dir: &Path) -> PathBuf { run_dir.join("wav") }
pub fn matched_dir(run_dir: &Path) -> PathBuf { run_dir.join("matched") }
pub fn manifests_dir(run_dir: &Path) -> PathBuf { run_dir.join("manifests") }

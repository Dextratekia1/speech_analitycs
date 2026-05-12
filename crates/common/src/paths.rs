use std::path::{Path, PathBuf};

pub fn run_dir(runs_root: &Path, client: &str, date: &str, run_id: &str) -> PathBuf {
    runs_root.join(client).join(date).join(run_id)
}

pub fn raw_dir(run_dir: &Path) -> PathBuf { run_dir.join("raw") }
pub fn wav_dir(run_dir: &Path) -> PathBuf { run_dir.join("wav") }
pub fn matched_dir(run_dir: &Path) -> PathBuf { run_dir.join("matched") }
pub fn manifests_dir(run_dir: &Path) -> PathBuf { run_dir.join("manifests") }

#[cfg(test)]
mod tests {
    use super::*;

    fn test_run_dir() -> PathBuf {
        run_dir(Path::new("/shared"), "natura", "2026-01-08", "bulk_natura_20260108")
    }

    #[test]
    fn test_run_dir_composition() {
        assert_eq!(
            test_run_dir(),
            PathBuf::from("/shared/natura/2026-01-08/bulk_natura_20260108")
        );
    }

    #[test]
    fn test_raw_dir_composition() {
        assert_eq!(
            raw_dir(&test_run_dir()),
            PathBuf::from("/shared/natura/2026-01-08/bulk_natura_20260108/raw")
        );
    }

    #[test]
    fn test_wav_dir_composition() {
        assert_eq!(
            wav_dir(&test_run_dir()),
            PathBuf::from("/shared/natura/2026-01-08/bulk_natura_20260108/wav")
        );
    }

    #[test]
    fn test_matched_dir_composition() {
        assert_eq!(
            matched_dir(&test_run_dir()),
            PathBuf::from("/shared/natura/2026-01-08/bulk_natura_20260108/matched")
        );
    }

    #[test]
    fn test_manifests_dir_composition() {
        assert_eq!(
            manifests_dir(&test_run_dir()),
            PathBuf::from("/shared/natura/2026-01-08/bulk_natura_20260108/manifests")
        );
    }
}

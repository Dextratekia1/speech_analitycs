use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FetchItem {
    pub url: String,
    pub filename: String,
    pub bytes: Option<u64>,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FetchManifest {
    pub schema_version: u32,
    pub client: String,
    pub date: String,
    pub run_id: String,
    pub generated_at: DateTime<Utc>,
    pub items: Vec<FetchItem>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConvertItem {
    pub record_id: String,
    pub input: String,
    pub output: String,
    pub status: String,
    pub ffprobe_ok: bool,
    pub duration_sec: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConvertManifest {
    pub schema_version: u32,
    pub client: String,
    pub date: String,
    pub run_id: String,
    pub generated_at: DateTime<Utc>,
    pub items: Vec<ConvertItem>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MatchItem {
    pub record_id: String,
    pub wav_path: String,
    pub json_path: String,
    pub lookup_ok: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MatchManifest {
    pub schema_version: u32,
    pub client: String,
    pub date: String,
    pub run_id: String,
    pub generated_at: DateTime<Utc>,
    pub items: Vec<MatchItem>,
}

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const ROWS: usize = 150_000;
pub const DIM: usize = 768;
pub const BATCH: usize = 4_096;
pub const TOP_K: usize = 10;
pub const QUERY_IDS: [u64; 10] = [
    0, 17, 1_337, 9_973, 24_011, 49_999, 75_001, 100_003, 125_009, 149_999,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timing {
    pub operation: String,
    pub rows: usize,
    pub millis: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryMetrics {
    pub mode: String,
    pub samples_ms: Vec<f64>,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub results: Vec<Vec<u64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashMetrics {
    pub before_commit_old_visible: bool,
    pub after_commit_new_visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseMetrics {
    pub engine: String,
    pub rows: usize,
    pub dimensions: usize,
    pub ingest_ms: f64,
    pub initial_publish_ms: f64,
    pub deltas: Vec<Timing>,
    pub exact: QueryMetrics,
    pub ann: Option<QueryMetrics>,
    pub ann_recall_at_10: Option<f64>,
    pub peak_rss_kib: u64,
    pub disk_bytes: u64,
    pub crash: CrashMetrics,
    pub active_revision: u64,
    pub active_native_version: Option<u64>,
    pub publication_primitive: String,
    pub native_expected_version_cas: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildMetrics {
    pub elapsed_seconds: f64,
    pub peak_rss_kib: u64,
    pub binary_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comparison {
    pub workload: String,
    pub platform: String,
    pub sqlite: CaseMetrics,
    pub lance: CaseMetrics,
    pub build: BuildMetrics,
    pub exact_parity: bool,
    pub recommendation: String,
}

pub fn synthetic_vector(id: u64, revision: u64) -> Vec<f32> {
    let mut state = id
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(revision.wrapping_mul(0xd1b5_4a32_d192_ed03))
        .wrapping_add(0x94d0_49bb_1331_11eb);
    let mut values = Vec::with_capacity(DIM);
    let mut norm = 0.0_f64;
    for _ in 0..DIM {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let bits = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let value = ((bits >> 40) as f32 / 8_388_607.5) - 1.0;
        norm += f64::from(value) * f64::from(value);
        values.push(value);
    }
    let inverse_norm = (norm.sqrt() as f32).recip();
    for value in &mut values {
        *value *= inverse_norm;
    }
    values
}

pub fn vector_bytes(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * size_of::<f32>());
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

pub fn cosine_distance_bytes(query: &[f32], bytes: &[u8]) -> Result<f32> {
    anyhow::ensure!(
        bytes.len() == DIM * size_of::<f32>(),
        "invalid vector byte length {}",
        bytes.len()
    );
    let dot = query
        .iter()
        .zip(bytes.chunks_exact(4))
        .map(|(left, right)| {
            let value = f32::from_le_bytes([right[0], right[1], right[2], right[3]]);
            left * value
        })
        .sum::<f32>();
    Ok(1.0 - dot)
}

pub fn top_k(mut values: Vec<(u64, f32)>) -> Vec<u64> {
    values.sort_unstable_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    values.into_iter().take(TOP_K).map(|(id, _)| id).collect()
}

pub fn query_metrics(mode: &str, timings: Vec<Duration>, results: Vec<Vec<u64>>) -> QueryMetrics {
    let samples_ms = timings
        .into_iter()
        .map(|duration| duration.as_secs_f64() * 1_000.0)
        .collect::<Vec<_>>();
    QueryMetrics {
        mode: mode.to_owned(),
        p50_ms: percentile(&samples_ms, 0.50),
        p95_ms: percentile(&samples_ms, 0.95),
        samples_ms,
        results,
    }
}

pub fn recall_at_10(exact: &[Vec<u64>], approximate: &[Vec<u64>]) -> f64 {
    let hits = exact
        .iter()
        .zip(approximate)
        .map(|(truth, observed)| observed.iter().filter(|id| truth.contains(id)).count())
        .sum::<usize>();
    hits as f64 / (exact.len() * TOP_K) as f64
}

pub fn percentile(samples: &[f64], quantile: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted[index]
}

pub fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0;
    let mut pending = vec![path.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).with_context(|| format!("read {}", next.display()))? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    Ok(total)
}

pub fn peak_rss_kib() -> u64 {
    let usage = unsafe {
        let mut usage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    usage.ru_maxrss as u64
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

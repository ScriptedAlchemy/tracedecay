//! Restore one already-published generation the way a daemon restart does.
//!
//! The corpus bench (`partitioned_codec`) builds a generation before it decodes
//! one, so it cannot answer what restoring an operator-scale store costs on the
//! bytes that store actually holds. This target takes a directory holding a
//! revision-7 generation manifest plus its file and evidence segments, decodes
//! it once through the production entry point, and reports the wall time, the
//! process CPU the decode consumed, and the byte rate per segment. The Hotpath
//! timing report decomposes the phases inside one segment decode.
//!
//! ```text
//! cargo bench -p tracedecay-code-index --features hotpath \
//!   --bench restore_generation -- <dir>
//! ```
//!
//! `<dir>/manifest.json` is the generation file copied from
//! `code-generations-v1/`, and `<dir>/segments/segment-<digest>.json` are the
//! segments it names, copied from `code-generation-segments-v1/`.

use std::{
    error::Error,
    fs::File,
    hint::black_box,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Instant,
};

use serde::Serialize;
use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1, SealedGenerationSegmentReadV1,
};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

const DEFAULT_HOTPATH_PATH: &str = "/tmp/tracedecay-restore-generation.json";

#[derive(Serialize)]
struct Measurement {
    schema_version: u32,
    manifest_bytes: usize,
    file_segments: usize,
    file_segment_bytes: u64,
    evidence_bytes: u64,
    restored_files: usize,
    decode_wall_ns: u64,
    decode_cpu_ns: u64,
    /// Aggregate CPU demand divided by the file segments decoded: the number
    /// the daemon's `code_index.restore.segment_decode` average must match
    /// once contention is removed.
    cpu_ns_per_file_segment: u64,
    cpu_mib_per_second: f64,
    /// CPU demand over wall time: how many cores the restore actually kept
    /// busy. Segment decoding fans out over the indexing pool, so a ratio far
    /// below that width means the restore's serial stages, not the codec,
    /// decide how long a retained generation takes to come back.
    decode_cores_busy: f64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: restore_generation <generation-directory>")?;
    configure_hotpath();
    let manifest = std::fs::read(root.join("manifest.json"))?;
    let segments = root.join("segments");

    let guard = hotpath::HotpathGuardBuilder::new("restore-generation-bench")
        .format(hotpath::Format::Json)
        .output_path(hotpath_output_path())
        .build();
    let cpu_before = process_cpu_ns()?;
    let started = Instant::now();
    let restored = CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
        &manifest,
        |request, buffer| read_segment(&segments, request, buffer),
    )?
    .ok_or("generation manifest is not a revision-7 partitioned manifest")?;
    let decode_wall_ns = u64::try_from(started.elapsed().as_nanos())?;
    let decode_cpu_ns = process_cpu_ns()?.saturating_sub(cpu_before);
    drop(guard);

    let restored_files = restored.analysis_coverage().count();
    let (file_segment_bytes, evidence_bytes) = directory_bytes(&segments)?;
    let measurement = Measurement {
        schema_version: 1,
        manifest_bytes: manifest.len(),
        file_segments: restored_files,
        file_segment_bytes,
        evidence_bytes,
        restored_files,
        decode_wall_ns,
        decode_cpu_ns,
        cpu_ns_per_file_segment: decode_cpu_ns / u64::try_from(restored_files.max(1))?,
        cpu_mib_per_second: (file_segment_bytes + evidence_bytes) as f64
            / 1_048_576.0
            / (decode_cpu_ns as f64 / 1e9),
        decode_cores_busy: decode_cpu_ns as f64 / decode_wall_ns as f64,
    };
    println!("{}", serde_json::to_string_pretty(&measurement)?);
    black_box(restored);
    Ok(())
}

fn read_segment(
    segments: &Path,
    request: SealedGenerationSegmentReadV1<'_>,
    buffer: &mut Vec<u8>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let (digest, offset, length) = match request {
        SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => (digest, 0, size_bytes),
        SealedGenerationSegmentReadV1::Range {
            digest,
            offset,
            length,
            ..
        } => (digest, offset, length),
    };
    let name = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| contract("segment digest is not a sha256 identity"))?;
    let length = usize::try_from(length).map_err(|_| contract("segment range exceeds memory"))?;
    let mut file = File::open(segments.join(format!("segment-{name}.json")))
        .map_err(|error| contract(&format!("segment open failed: {error}")))?;
    buffer.clear();
    buffer.resize(length, 0);
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(buffer))
        .map_err(|error| contract(&format!("segment read failed: {error}")))
}

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

/// Total bytes of the copied segments, split into the file segments and the
/// single largest one, which is the generation evidence pack.
fn directory_bytes(segments: &Path) -> Result<(u64, u64), Box<dyn Error>> {
    let mut sizes = std::fs::read_dir(segments)?
        .map(|entry| Ok::<u64, Box<dyn Error>>(entry?.metadata()?.len()))
        .collect::<Result<Vec<_>, _>>()?;
    sizes.sort_unstable();
    let evidence = sizes.pop().unwrap_or(0);
    Ok((sizes.iter().sum(), evidence))
}

/// Process CPU nanoseconds across every thread, from the scheduler's own
/// per-task accounting rather than a clock-tick approximation. `schedstat`
/// under `/proc/self` covers the calling thread alone, so the decode's worker
/// pool has to be summed task by task.
fn process_cpu_ns() -> Result<u64, Box<dyn Error>> {
    let mut total = 0_u64;
    for task in std::fs::read_dir("/proc/self/task")? {
        let schedstat = match std::fs::read_to_string(task?.path().join("schedstat")) {
            Ok(schedstat) => schedstat,
            // A thread that exits between the listing and the read has no
            // remaining accounting to add.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        total = total.saturating_add(
            schedstat
                .split_whitespace()
                .next()
                .ok_or("empty thread schedstat")?
                .parse::<u64>()?,
        );
    }
    Ok(total)
}

fn configure_hotpath() {
    unsafe {
        std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        if std::env::var_os("HOTPATH_REPORT").is_none() {
            std::env::set_var("HOTPATH_REPORT", "functions-timing");
        }
        // The default report keeps only the costliest handful of spans, which
        // hides the cheap phases a decode decomposition has to account for.
        if std::env::var_os("HOTPATH_FUNCTIONS_LIMIT").is_none() {
            std::env::set_var("HOTPATH_FUNCTIONS_LIMIT", "64");
        }
    }
}

fn hotpath_output_path() -> PathBuf {
    std::env::var_os("HOTPATH_OUTPUT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_HOTPATH_PATH))
}

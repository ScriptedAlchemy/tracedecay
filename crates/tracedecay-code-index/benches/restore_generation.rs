//! Restore one copied, already-published generation through the full codec.
//!
//! The corpus bench (`partitioned_codec`) builds a generation before it decodes
//! one, so it cannot answer what restoring an operator-scale store costs on the
//! bytes that store actually holds. This target takes a directory holding a
//! current-format generation manifest plus its content and evidence packs, decodes
//! it once through the production entry point, and reports the wall time, the
//! process CPU the decode consumed, and the byte rate per segment; tracing
//! spans decompose the phases inside one segment decode.
//!
//! ```text
//! bazel run //crates/tracedecay-code-index:restore_generation -- <dir> [restores]
//! ```
//!
//! `<dir>/manifest.json` is the generation file copied from
//! `code-generations-v1/`, and `<dir>/segments/segment-<digest>.json` are the
//! physical packs it names, copied from `code-generation-segments-v1/`. The
//! benchmark directory reader opens each requested range directly; it does not
//! measure the daemon's operation-owned handle admission or retention locks.
//!
//! `[restores]` restores that generation from that many threads at once, which
//! is what a daemon does when several retained scope roots come back together.
//! It reports when the first and the last restore finished, so bounding
//! concurrency shows up as the first root serving sooner rather than as less
//! total work.

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
    CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1, SealedGenerationPackLayoutV1,
    SealedGenerationSegmentReadV1, SharedDecodedContentPoolV1,
};

#[derive(Serialize)]
struct Measurement {
    schema_version: u32,
    manifest_bytes: usize,
    /// Logical source-file segments restored from the physical content pack.
    file_segments: usize,
    content_pack_bytes: u64,
    evidence_bytes: u64,
    restored_files: usize,
    decode_wall_ns: u64,
    decode_cpu_ns: u64,
    /// Aggregate CPU demand divided by the file segments decoded: the number
    /// the daemon's `code_index.restore.segment_decode` average must match
    /// once contention is removed.
    cpu_ns_per_file_segment: u64,
    /// Manifest-declared payload bytes per CPU second, not measured I/O throughput.
    cpu_mib_per_second: f64,
    /// CPU demand over wall time: how many cores the restore actually kept
    /// busy. Segment decoding fans out over the indexing pool, so a ratio far
    /// below that width means the restore's serial stages, not the codec,
    /// decide how long a retained generation takes to come back.
    decode_cores_busy: f64,
    /// How many restores ran at once. Several retained scope roots come back
    /// together on a daemon restart, so this is what a restart costs, not what
    /// one root costs.
    concurrent_restores: usize,
    /// Wall time from the batch starting to the first and to the last restore
    /// completing. Both near the total means the restores overlapped freely;
    /// the first well below the last means they queued behind each other.
    first_restore_ns: u64,
    last_restore_ns: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: restore_generation <generation-directory> [restores]")?;
    let concurrent_restores = match arguments.next() {
        Some(count) => count
            .to_str()
            .ok_or("restore count is not valid UTF-8")?
            .parse::<usize>()?
            .max(1),
        None => 1,
    };
    let manifest = std::fs::read(root.join("manifest.json"))?;
    let segments = root.join("segments");

    let cpu_before = process_cpu_ns()?;
    let started = Instant::now();
    let completions = restore_concurrently(concurrent_restores, &manifest, &segments, started)?;
    let decode_wall_ns = u64::try_from(started.elapsed().as_nanos())?;
    let decode_cpu_ns = process_cpu_ns()?.saturating_sub(cpu_before);

    let restored_files = completions.restored_files;
    let (content_pack_bytes, evidence_bytes) = payload_bytes(&manifest, &segments)?;
    let measurement = Measurement {
        schema_version: 3,
        manifest_bytes: manifest.len(),
        file_segments: restored_files,
        content_pack_bytes,
        evidence_bytes,
        restored_files,
        decode_wall_ns,
        decode_cpu_ns,
        cpu_ns_per_file_segment: decode_cpu_ns / u64::try_from(restored_files.max(1))?,
        cpu_mib_per_second: (content_pack_bytes + evidence_bytes) as f64
            * concurrent_restores as f64
            / 1_048_576.0
            / (decode_cpu_ns as f64 / 1e9),
        decode_cores_busy: decode_cpu_ns as f64 / decode_wall_ns as f64,
        concurrent_restores,
        first_restore_ns: completions.first_restore_ns,
        last_restore_ns: completions.last_restore_ns,
    };
    println!("{}", serde_json::to_string_pretty(&measurement)?);
    Ok(())
}

struct RestoreBatchV1 {
    restored_files: usize,
    first_restore_ns: u64,
    last_restore_ns: u64,
}

/// Restore the same generation from `count` threads and report when the first
/// and last of them finished, refusing a batch whose restores disagree on the
/// files they recovered.
///
/// Every restore verified the same manifest's per-segment and aggregate
/// digests, so a divergent restore would already have been refused. Comparing
/// the recovered coverage pins the records that survived those checks to each
/// other as well.
fn restore_concurrently(
    count: usize,
    manifest: &[u8],
    segments: &Path,
    started: Instant,
) -> Result<RestoreBatchV1, Box<dyn Error>> {
    let restores = std::thread::scope(|scope| {
        let threads = (0..count)
            .map(|_| scope.spawn(|| restore_once(manifest, segments, started)))
            .collect::<Vec<_>>();
        threads
            .into_iter()
            .map(|thread| {
                thread
                    .join()
                    .map_err(|_| "a restore thread panicked".to_owned())?
            })
            .collect::<Result<Vec<_>, String>>()
    })?;
    let (expected, _) = &restores[0];
    for (index, (coverage, _)) in restores.iter().enumerate() {
        if coverage != expected {
            return Err(format!("restore {index} recovered different records").into());
        }
    }
    let mut completions = restores.iter().map(|(_, at)| *at).collect::<Vec<_>>();
    completions.sort_unstable();
    Ok(RestoreBatchV1 {
        restored_files: expected.len(),
        first_restore_ns: completions.first().copied().unwrap_or(0),
        last_restore_ns: completions.last().copied().unwrap_or(0),
    })
}

/// One restore's recovered coverage, every restored file's logical path in
/// canonical order, and the elapsed time at which it completed.
fn restore_once(
    manifest: &[u8],
    segments: &Path,
    started: Instant,
) -> Result<(Vec<String>, u64), String> {
    let restored = CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
        manifest,
        &SharedDecodedContentPoolV1::default(),
        |request, buffer| read_segment(segments, request, buffer),
        // Measure the full codec without adding a resident-admission boundary.
        &mut |_| Ok(()),
    )
    .map_err(|error| error.to_string())?;
    let mut coverage = restored
        .analysis_coverage()
        .map(|(path, _)| path.to_owned())
        .collect::<Vec<_>>();
    coverage.sort_unstable();
    black_box(restored);
    Ok((
        coverage,
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
    ))
}

fn read_segment(
    segments: &Path,
    request: SealedGenerationSegmentReadV1<'_>,
    buffer: &mut Vec<u8>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let (digest, size_bytes, offset, length) = match request {
        SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
            (digest, size_bytes, 0, size_bytes)
        }
        SealedGenerationSegmentReadV1::Range {
            digest,
            offset,
            length,
            size_bytes,
        } => (digest, size_bytes, offset, length),
    };
    let name = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| contract("segment digest is not a sha256 identity"))?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| contract("pack range overflowed"))?;
    if end > size_bytes {
        return Err(contract("pack range exceeds its authenticated extent"));
    }
    let length = usize::try_from(length).map_err(|_| contract("segment range exceeds memory"))?;
    let mut file = File::open(segments.join(format!("segment-{name}.json")))
        .map_err(|error| contract(&format!("segment open failed: {error}")))?;
    let actual_size = file
        .metadata()
        .map_err(|error| contract(&format!("pack metadata failed: {error}")))?
        .len();
    if actual_size != size_bytes {
        return Err(contract("pack size does not match its manifest"));
    }
    buffer.clear();
    buffer.resize(length, 0);
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(buffer))
        .map_err(|error| contract(&format!("segment read failed: {error}")))
}

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

/// Physical payload bytes by their canonical roles; directory size order
/// cannot identify the evidence pack and may include unrelated generations.
fn payload_bytes(manifest: &[u8], segments: &Path) -> Result<(u64, u64), Box<dyn Error>> {
    let layout = SealedGenerationPackLayoutV1::open(manifest)?;
    let content = layout.content_pack();
    let evidence = layout.evidence_pack();
    for (digest, size_bytes) in [
        (&content.digest, content.size_bytes),
        (&evidence.digest, evidence.size_bytes),
    ] {
        let name = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or("pack digest is not a sha256 identity")?;
        let actual_size = std::fs::metadata(segments.join(format!("segment-{name}.json")))?.len();
        if actual_size != size_bytes {
            return Err("pack size does not match its manifest".into());
        }
    }
    Ok((content.size_bytes, evidence.size_bytes))
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

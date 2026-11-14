use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use tracedecay_code_index::graph_projection::{
    CodeGraphLayeredReportV1, CodeGraphProjectionError, SealedCodeGraphRowsError,
    build_layered_code_graph_rows, build_sealed_code_graph_rows,
};
use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, SealedGenerationFileWindowsV1, SealedGenerationSegmentReadV1,
};
use tracedecay_code_index_retention::code_index_generations::code_generation_segments_root;
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_domain::{ManifestDigest, ProjectId, RepositoryId, sha256_hex_suffix};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphDbError, GraphGenerationManifestProvider, GraphGenerationRowSpill,
    GraphGenerationRows, GraphLayeredRowSpill, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphProjectorRevision, GraphSealedBaseAbsenceV1,
    GraphSiblingSealedBaseV1, SealedCodeGenerationReplay, SealedGraphStateDigest,
    SpilledGraphGeneration,
};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessSharedMemoryReservationV1, ResidentMemoryComponentIdV1,
    ResidentMemoryOperationV1, ResidentMemoryPressureV1,
};
use tracedecay_store::{GraphProjectionIdentityV1, StoreShardIdV1};

use super::verified_sealed_segments::{
    SealedSegmentFingerprintV1, SealedSegmentWitnessV1, VerifiedSealedSegmentsV1,
};

const SEAL_READ_CHECK_BYTES: usize = 64 * 1024;

/// One graph operation keeps its allocation charge while the owned bytes live.
pub(super) struct CodeGraphMemoryV1 {
    operation: ResidentMemoryOperationV1,
    component: ResidentMemoryComponentIdV1,
    charge: Option<ProcessSharedMemoryReservationV1>,
}

impl CodeGraphMemoryV1 {
    pub(super) fn new(
        authority: &Arc<ProcessResidentMemoryV1>,
        component: &'static str,
    ) -> Result<Self, GraphDbError> {
        Ok(Self {
            operation: authority.begin_operation(),
            component: ResidentMemoryComponentIdV1::new(component)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            charge: None,
        })
    }

    pub(super) fn resize(&mut self, bytes: u64) -> Result<(), GraphDbError> {
        let limit = self.operation.authority().limit_bytes();
        let refused = || GraphDbError::budget_exhausted(GraphBudgetKind::ResidentMemory, limit);
        let requested = bytes
            .checked_add(std::mem::size_of::<Self>() as u64)
            .ok_or_else(refused)?;
        let current = self
            .charge
            .as_ref()
            .map_or(0, |charge| charge.reserved_bytes());
        if requested > current {
            let additional = NonZeroU64::new(requested - current).ok_or_else(refused)?;
            match self.charge.as_mut() {
                Some(charge) => charge.try_grow(additional).map_err(|_| refused())?,
                None => {
                    self.charge = Some(
                        self.operation
                            .reserve_process_shared(self.component, additional)
                            .map_err(|_| refused())?,
                    );
                }
            }
        } else if let Some(charge) = self.charge.as_mut() {
            charge
                .shrink_to(requested)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static SEGMENT_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn classify_sealed_generation_decode_error(
    error: CodeIndexProductionErrorV1,
    sealed_state_digest: &ManifestDigest,
) -> GraphDbError {
    match error {
        // A row shape or an envelope revision this build no longer reads is
        // the same typed state to replay: the sealed bytes are intact, this
        // build just cannot derive a graph from them, so replay reports
        // unavailability and the generation is rebuilt rather than declared
        // corrupt.
        error @ (CodeIndexProductionErrorV1::SealedRowContractRefused { .. }
        | CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(_)) => {
            GraphDbError::SealedRevisionIncompatible {
                sealed_state_digest: sealed_state_digest.as_str().to_owned(),
                message: error.to_string(),
            }
        }
        error => GraphDbError::Corrupt {
            message: format!("sealed code generation replay is invalid: {error}"),
        },
    }
}

fn validate_sealed_generation_metadata(metadata: &std::fs::Metadata) -> Result<u64, GraphDbError> {
    if !metadata.file_type().is_file() {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation replay target is not a regular file".to_owned(),
        });
    }
    if metadata.len() > tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1 {
        return Err(GraphDbError::ResetRequired {
            message: "sealed code generation exceeds the canonical byte limit".to_owned(),
        });
    }
    Ok(metadata.len())
}

/// Readers hold no store lock, so retention may hard-link or rename a seal
/// while it is read. Both advance ctime without touching the bytes, so ctime
/// is not part of a seal's identity.
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        // Volume and file-index equality is checked separately through the
        // stable handle authority (`same_windows_handle_identity`); metadata
        // only carries the stable fields here.
        left.file_size() == right.file_size()
            && left.last_write_time() == right.last_write_time()
            && left.creation_time() == right.creation_time()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// Confirms the opened handle and the path still denote the same file, via
/// the stable GetFileInformationByHandle authority instead of the unstable
/// `windows_by_handle` metadata surface.
#[cfg(windows)]
fn same_windows_handle_identity(
    file: &File,
    path: &std::path::Path,
    record_open_attempt: impl FnOnce(),
) -> Result<bool, GraphDbError> {
    record_open_attempt();
    let path_file =
        File::open(path).map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let path_identity = tracedecay_private_fs::windows_file::information(&path_file)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let handle_identity = tracedecay_private_fs::windows_file::information(file)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    Ok(
        path_identity.volume_serial_number == handle_identity.volume_serial_number
            && path_identity.file_index == handle_identity.file_index,
    )
}

struct CheckedSealReader<'a> {
    reader: BufReader<File>,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    bytes_read: u64,
    digest: Sha256,
    failure: Option<GraphDbError>,
}

impl CheckedSealReader<'_> {
    fn retain_failure(&mut self, error: GraphDbError) -> std::io::Error {
        self.failure = Some(error);
        std::io::Error::other("sealed code generation checked read failed")
    }

    /// Returns the authenticated handle. Only the bytes hashed by this
    /// reader are proven; a later read of the same inode is not that proof.
    fn finish(
        self,
        path: &std::path::Path,
        opened_metadata: &std::fs::Metadata,
        admitted_len: u64,
        expected_digest: &str,
    ) -> Result<File, GraphDbError> {
        (self.check)()?;
        // Endpoint authorization may finish after the caller's deadline, even
        // on an I/O failure. Preserve typed interruption before classifying
        // final handle or pathname validation as a damaged/retired seal.
        let final_file_metadata = self.reader.get_ref().metadata();
        (self.check)()?;
        let final_file_metadata = final_file_metadata.map_err(|error| GraphDbError::Corrupt {
            message: format!("sealed code generation metadata cannot be revalidated: {error}"),
        })?;
        if !same_file_identity(opened_metadata, &final_file_metadata)
            || self.bytes_read != admitted_len
        {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation identity or length changed while it was read"
                    .to_owned(),
            });
        }
        let left_path = seal_left_path(path, opened_metadata);
        (self.check)()?;
        if left_path? {
            return Err(GraphDbError::SealSuperseded);
        }
        #[cfg(windows)]
        {
            let same_handle = same_windows_handle_identity(self.reader.get_ref(), path, || {});
            (self.check)()?;
            if !same_handle? {
                return Err(GraphDbError::Corrupt {
                    message: "sealed code generation identity or length changed while it was read"
                        .to_owned(),
                });
            }
        }
        if encode_lowercase_hex(&self.digest.finalize()) != expected_digest {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation filename digest does not match its bytes"
                    .to_owned(),
            });
        }
        Ok(self.reader.into_inner())
    }
}

/// Retention retires seals without waiting for readers. A read that outlives
/// its path reports the seal superseded; the bytes it read are not evidence
/// of corruption.
fn seal_left_path(
    path: &std::path::Path,
    opened_metadata: &std::fs::Metadata,
) -> Result<bool, GraphDbError> {
    match path.symlink_metadata() {
        Ok(path_metadata) => Ok(!same_file_identity(opened_metadata, &path_metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(GraphDbError::Corrupt {
            message: format!("sealed code generation path cannot be revalidated: {error}"),
        }),
    }
}

/// Whether any of `seal_paths` still names the seal. The segment sweep
/// collects a seal's segments only once no root names the seal.
fn seal_still_named(seal_paths: &[PathBuf]) -> Result<bool, GraphDbError> {
    for path in seal_paths {
        if seal_is_present(path)? {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Read for CheckedSealReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Err(error) = (self.check)() {
            return Err(self.retain_failure(error));
        }
        let read_len = buffer.len().min(SEAL_READ_CHECK_BYTES);
        let read = self.reader.read(&mut buffer[..read_len]);
        // A scanned read may return after the caller's deadline, including
        // on an I/O failure. Do not classify that interruption as corruption
        // or expose bytes read after the deadline.
        if let Err(error) = (self.check)() {
            buffer[..read_len].fill(0);
            return Err(self.retain_failure(error));
        }
        let read = match read {
            Ok(read) => read,
            Err(error) => {
                buffer[..read_len].fill(0);
                let error = GraphDbError::Corrupt {
                    message: format!("sealed code generation replay read failed: {error}"),
                };
                return Err(self.retain_failure(error));
            }
        };
        let read = u64::try_from(read).map_err(|_| {
            self.retain_failure(GraphDbError::ResetRequired {
                message: "sealed code generation read length exceeds u64".to_owned(),
            })
        })?;
        let next_len = self.bytes_read.checked_add(read).ok_or_else(|| {
            self.retain_failure(GraphDbError::ResetRequired {
                message: "sealed code generation byte length overflowed".to_owned(),
            })
        })?;
        if next_len > tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1 {
            return Err(self.retain_failure(GraphDbError::ResetRequired {
                message: "sealed code generation grew beyond the canonical byte limit".to_owned(),
            }));
        }
        let read = usize::try_from(read).map_err(|_| {
            self.retain_failure(GraphDbError::ResetRequired {
                message: "sealed code generation read length exceeds addressable memory".to_owned(),
            })
        })?;
        self.digest.update(&buffer[..read]);
        self.bytes_read = next_len;
        Ok(read)
    }
}

fn open_checked_seal_reader<'a>(
    path: &std::path::Path,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    memory: &mut CodeGraphMemoryV1,
) -> Result<(CheckedSealReader<'a>, std::fs::Metadata, u64), GraphDbError> {
    // Metadata and endpoint authorization may finish after the caller expires.
    // Preserve the typed interruption even when the filesystem operation fails.
    let path_metadata = path.symlink_metadata();
    (check)()?;
    let path_metadata = path_metadata.map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation is unavailable for replay: {error}"
        ))
    })?;
    let admitted_len = validate_sealed_generation_metadata(&path_metadata)?;
    let opened = File::open(path);
    (check)()?;
    let file = opened.map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation cannot be opened for replay: {error}"
        ))
    })?;
    let opened_metadata = file.metadata();
    (check)()?;
    let opened_metadata = opened_metadata.map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed code generation metadata cannot be read: {error}"),
    })?;
    if !same_file_identity(&path_metadata, &opened_metadata) {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity changed while it was opened".to_owned(),
        });
    }
    #[cfg(windows)]
    {
        // The second pathname open can cross the caller's deadline on
        // endpoint-scanned hosts. Classify interruption before identity errors.
        let same_handle = same_windows_handle_identity(&file, path, || {});
        (check)()?;
        if !same_handle? {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation identity changed while it was opened".to_owned(),
            });
        }
    }
    memory.resize(SEAL_READ_CHECK_BYTES as u64)?;
    Ok((
        CheckedSealReader {
            reader: BufReader::with_capacity(SEAL_READ_CHECK_BYTES, file),
            check,
            bytes_read: 0,
            digest: Sha256::new(),
            failure: None,
        },
        opened_metadata,
        admitted_len,
    ))
}

/// Resolve a sealed generation from its canonical `code-generations-v1/` root
/// first, then from the graph replay pool. Retirement moves a sealed file
/// strictly canonical->pool by atomic rename, so probing in that order
/// observes a live seal in at least one root; every read is digest-verified,
/// which makes recovery from either root equally trustworthy. Readers take no
/// store or pool lock: a seal is immutable and content-addressed, so the
/// digest-verified bytes of the opened handle are the seal, and a seal that
/// retires mid-read is unavailable. Typed interruptions from the caller's
/// probe are transport states and must surface immediately instead of
/// triggering a second full read.
fn with_verified_seal_from_roots<T>(
    canonical: &std::path::Path,
    pool: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    mut read: impl FnMut(
        &std::path::Path,
        &str,
        &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<T, GraphDbError>,
) -> Result<T, GraphDbError> {
    let canonical_absent = matches!(
        std::fs::symlink_metadata(canonical),
        Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
    );
    if !canonical_absent {
        match read(canonical, expected_digest, check) {
            Ok(value) => return Ok(value),
            Err(error) if is_seal_read_refusal(&error) => {
                return Err(error);
            }
            Err(canonical_error) => {
                // A concurrent retirement rename can move the seal mid-read;
                // the pool copy is digest-verified, so recovering there is
                // sound. A pool failure reports the canonical error, which
                // names the authoritative copy.
                return match read(pool, expected_digest, check) {
                    Ok(value) => Ok(value),
                    // A late endpoint-scanned fallback may cross the deadline
                    // after the canonical copy failed. Do not misreport that
                    // interruption as the earlier source corruption.
                    Err(error) if is_seal_read_refusal(&error) => Err(error),
                    Err(_) => Err(canonical_error),
                };
            }
        }
    }
    read(pool, expected_digest, check)
}

fn is_seal_read_refusal(error: &GraphDbError) -> bool {
    matches!(
        error,
        GraphDbError::Cancelled
            | GraphDbError::DeadlineExceeded
            | GraphDbError::BudgetExhausted {
                kind: GraphBudgetKind::ResidentMemory,
                ..
            }
    )
}

/// Only NotFound abstains; malformed or inaccessible seal authority fails closed.
fn seal_is_present(path: &std::path::Path) -> Result<bool, GraphDbError> {
    match path.symlink_metadata() {
        Ok(metadata) => validate_sealed_generation_metadata(&metadata).map(|_| true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "sealed generation metadata cannot be read: {error}"
        ))),
    }
}

/// Reads the partitioned manifest at `path` and proves it is the seal named
/// by `expected_digest`. The digest is checked over the bytes read from the
/// opened handle, so a retirement that unlinks the path after the open
/// cannot change them.
#[tracing::instrument(
    name = "daemon.session_registry.seal.read_manifest",
    level = "trace",
    skip_all
)]
fn read_verified_seal_manifest(
    path: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    memory: &mut CodeGraphMemoryV1,
) -> Result<Vec<u8>, GraphDbError> {
    (check)()?;
    // Endpoint authorization can spend the caller's budget on metadata or
    // open as well as reads. Recheck even when an OS operation fails.
    let metadata = path.symlink_metadata();
    (check)()?;
    let path_metadata = metadata.map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation is unavailable for replay: {error}"
        ))
    })?;
    let admitted_len = validate_sealed_generation_metadata(&path_metadata)?;
    let opened = File::open(path);
    (check)()?;
    let mut file = opened.map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation cannot be opened for replay: {error}"
        ))
    })?;
    let metadata = file.metadata();
    (check)()?;
    let opened_metadata = metadata.map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed code generation metadata cannot be read: {error}"),
    })?;
    if !same_file_identity(&path_metadata, &opened_metadata) {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity changed while it was opened".to_owned(),
        });
    }
    #[cfg(windows)]
    {
        // The second pathname open can cross the caller's deadline on
        // endpoint-scanned hosts. Classify interruption before identity errors.
        let same_handle = same_windows_handle_identity(&file, path, || {});
        (check)()?;
        if !same_handle? {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation identity changed while it was opened".to_owned(),
            });
        }
    }

    memory.resize(admitted_len)?;
    let length = usize::try_from(admitted_len)
        .map_err(|_| GraphDbError::invalid("sealed manifest length does not fit memory"))?;
    let mut manifest = vec![0; length];
    let read = file.read_exact(&mut manifest);
    // Endpoint scanning may finish this I/O after a caller deadline, even
    // when the read failed. The interruption is not source corruption.
    (check)()?;
    read.map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed generation manifest read failed: {error}"),
    })?;
    if u64::try_from(manifest.len()).ok() != Some(admitted_len)
        || encode_lowercase_hex(&Sha256::digest(&manifest)) != expected_digest
    {
        return Err(GraphDbError::Corrupt {
            message: "sealed generation manifest filename digest does not match its bytes"
                .to_owned(),
        });
    }
    (check)()?;
    Ok(manifest)
}

/// The paths that can name one seal and the roots holding its segments.
struct SealRoutes {
    seal_paths: Vec<PathBuf>,
    segment_roots: Vec<PathBuf>,
}

struct AdmittedSealV1 {
    source: SealedGenerationFileWindowsV1,
    sealed_state_digest: ManifestDigest,
    memory: CodeGraphMemoryV1,
}

struct VerifiedSealGraphSourceV1 {
    admitted: AdmittedSealV1,
    routes: SealRoutes,
}

fn retained_source_bytes(
    source: &SealedGenerationFileWindowsV1,
    sealed_state_digest: &ManifestDigest,
) -> usize {
    source
        .retained_bytes()
        .saturating_add(std::mem::size_of::<ManifestDigest>())
        .saturating_add(sealed_state_digest.as_str().len())
        .saturating_add(std::mem::size_of::<GraphSegmentReadPassV1<'_>>())
        .saturating_add(
            MAX_GRAPH_SEGMENT_OPEN_HANDLES
                .saturating_mul(std::mem::size_of::<PinnedPartitionedSegmentV1>()),
        )
        .saturating_add(source.content_pack().digest.as_str().len())
        .saturating_add(source.evidence_pack().0.as_str().len())
}

fn page_build_bound(
    source: &SealedGenerationFileWindowsV1,
    mut bound: tracedecay_code_index::production::CodeGraphBuildBoundV1,
) -> Result<tracedecay_code_index::production::CodeGraphBuildBoundV1, GraphDbError> {
    let retained = source.retained_pack_bytes() as u64;
    let invalid = || GraphDbError::invalid("graph build bound omits its retained pack map");
    // The complete source is already charged through its owned reservation.
    // The existing page bound includes the pack map in these two stages.
    bound.decode_window_bytes = bound
        .decode_window_bytes
        .checked_sub(retained)
        .ok_or_else(invalid)?;
    bound.store_bytes = bound
        .store_bytes
        .checked_sub(retained)
        .ok_or_else(invalid)?;
    Ok(bound)
}

/// Runs a graph build over `source`'s file segments, read from the segment
/// roots of `routes`, and classifies its failure. A cancellation or deadline
/// the segment reader observed is the build's outcome, whatever error the
/// build reported after it. A build whose seal no route names any more read
/// a retired seal: it is superseded, not corrupt.
fn with_verified_segments<T>(
    sealed_state_digest: &ManifestDigest,
    routes: &SealRoutes,
    segments: &mut GraphSegmentReadPassV1<'_>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    build: impl FnOnce(
        &mut tracedecay_code_index::production::SealedGenerationSegmentReaderV1<'_>,
    ) -> Result<T, SealedCodeGraphRowsError>,
) -> Result<T, GraphDbError> {
    let mut interruption = None;
    let started = std::time::Instant::now();
    let prior_opens = segments.open_attempts;
    let prior_open_wall = segments.open_wall_time;
    let prior_reads = segments.reads;
    let mut read_segment = |request: SealedGenerationSegmentReadV1<'_>,
                            buffer: &mut Vec<u8>|
     -> Result<(), CodeIndexProductionErrorV1> {
        // Cancellation must not expose bytes from a previously verified page.
        buffer.clear();
        if let Err(error) = (check)() {
            if matches!(
                error,
                GraphDbError::Cancelled | GraphDbError::DeadlineExceeded
            ) {
                interruption = Some(error.clone());
            }
            return Err(CodeIndexProductionErrorV1::Contract(error.to_string()));
        }
        let read = segments.read(request, buffer);
        // An endpoint-scanned open or read can outlive the caller's budget.
        // Recheck even on I/O failure so an interruption is not corruption.
        if let Err(error) = (check)() {
            buffer.clear();
            if matches!(
                error,
                GraphDbError::Cancelled | GraphDbError::DeadlineExceeded
            ) {
                interruption = Some(error.clone());
            }
            return Err(CodeIndexProductionErrorV1::Contract(error.to_string()));
        }
        read
    };
    let built = build(&mut read_segment).and_then(|value| {
        segments
            .revalidate(false)
            .map(|()| value)
            .map_err(SealedCodeGraphRowsError::Source)
    });
    tracing::info!(
        event = "code_graph_segment_read_pass",
        segment_open_attempts = segments.open_attempts - prior_opens,
        segment_open_wall_seconds = (segments.open_wall_time - prior_open_wall).as_secs_f64(),
        segment_reads = segments.reads - prior_reads,
        graph_pass_wall_seconds = started.elapsed().as_secs_f64(),
        "finished the pass-scoped sealed graph segment reader"
    );
    if let Some(interruption) = interruption {
        return Err(interruption);
    }
    // A graph projection may keep computing or spilling rows after its final
    // segment read. Its last successful I/O probe is not publication authority.
    // Refuse a result that finished after the caller's deadline.
    (check)()?;
    let error = match built {
        Ok(built) => return Ok(built),
        Err(SealedCodeGraphRowsError::Source(error)) => {
            classify_sealed_generation_decode_error(error, sealed_state_digest)
        }
        Err(SealedCodeGraphRowsError::Projection(error)) => {
            classify_sealed_projection_build_error(error)
        }
    };
    if matches!(error, GraphDbError::Corrupt { .. }) {
        // Endpoint security can delay the last pathname check too. Preserve
        // a deadline or cancellation observed after that I/O, even if the
        // seal was retired or the pathname probe itself failed.
        let still_named = seal_still_named(&routes.seal_paths);
        (check)()?;
        if !still_named? {
            return Err(GraphDbError::SealSuperseded);
        }
    }
    Err(error)
}

/// Builds the code graph of the authenticated seal `manifest` into `spill`,
/// streaming its file segments from `routes` one window at a time.
#[tracing::instrument(
    name = "daemon.session_registry.seal.spill_graph",
    level = "trace",
    skip_all
)]
#[allow(clippy::too_many_arguments)]
fn spill_verified_seal_graph(
    seal: &VerifiedSealGraphSourceV1,
    segments: &mut GraphSegmentReadPassV1<'_>,
    projection: GraphProjectionIdentity,
    projector_revision: &GraphProjectorRevision,
    spill: GraphGenerationRowSpill,
    admit: &mut dyn FnMut(
        tracedecay_code_index::production::CodeGraphBuildBoundV1,
    ) -> Result<(), GraphDbError>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SpilledGraphGeneration, GraphDbError> {
    with_verified_segments(
        &seal.admitted.sealed_state_digest,
        &seal.routes,
        segments,
        check,
        |read_pack| {
            let mut read_segment = |request: SealedGenerationSegmentReadV1<'_>,
                                    buffer: &mut Vec<u8>| {
                seal.admitted
                    .source
                    .read_segment(request, buffer, &mut *read_pack)
            };
            let mut admit_pages = |bound| admit(page_build_bound(&seal.admitted.source, bound)?);
            build_sealed_code_graph_rows(
                projection,
                &seal.admitted.source,
                &mut read_segment,
                projector_revision,
                spill,
                &mut admit_pages,
                check,
            )
        },
    )
}

/// Authenticates a seal's partitioned manifest for a streaming graph build.
fn open_verified_seal(
    manifest: Vec<u8>,
    expected_digest: &str,
    memory: CodeGraphMemoryV1,
) -> Result<AdmittedSealV1, GraphDbError> {
    // Owned allocations unwind before their reservation on every refusal.
    let mut memory = memory;
    let manifest = manifest;
    let raw_bytes = manifest.capacity() as u64;
    let digest_len = "sha256:".len().saturating_add(expected_digest.len());
    let digest_bytes = std::mem::size_of::<ManifestDigest>().saturating_add(digest_len) as u64;
    memory.resize(raw_bytes.saturating_add(digest_bytes))?;
    let mut digest = String::with_capacity(digest_len);
    digest.push_str("sha256:");
    digest.push_str(expected_digest);
    let sealed_state_digest =
        ManifestDigest::new(digest).map_err(|error| GraphDbError::Corrupt {
            message: format!("sealed code generation filename digest is not canonical: {error}"),
        })?;
    let mut admission_error = None;
    let source = SealedGenerationFileWindowsV1::open_admitted(&manifest, &mut |decoded| {
        let required = raw_bytes
            .saturating_add(digest_bytes)
            .saturating_add(decoded as u64);
        memory.resize(required).map_err(|error| {
            admission_error = Some(error.clone());
            CodeIndexProductionErrorV1::Contract(error.to_string())
        })
    });
    if let Some(error) = admission_error {
        return Err(error);
    }
    let source = source
        .map_err(|error| classify_sealed_generation_decode_error(error, &sealed_state_digest))?;
    drop(manifest);
    memory.resize(retained_source_bytes(&source, &sealed_state_digest) as u64)?;
    Ok(AdmittedSealV1 {
        source,
        sealed_state_digest,
        memory,
    })
}

/// Builds the code graph of the seal `sealed_state_digest` names, read from
/// the canonical generations root or, once retention moved it, the replay
/// pool, into `spill`. The seal must hold `generation`.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn spill_sealed_generation_graph_from_roots(
    generations_root: &std::path::Path,
    replay_root: &std::path::Path,
    resident_memory: &Arc<ProcessResidentMemoryV1>,
    sealed_state_digest: &SealedGraphStateDigest,
    generation: &tracedecay_domain::CodeGenerationId,
    projection: GraphProjectionIdentity,
    projector_revision: &GraphProjectorRevision,
    spill: GraphGenerationRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SpilledGraphGeneration, GraphDbError> {
    let seal = open_seal_from_roots(
        generations_root,
        replay_root,
        resident_memory,
        sealed_state_digest,
        generation,
        check,
        "code_graph_manifest.spill_sealed_generation_graph",
    )?;
    let mut segments = GraphSegmentReadPassV1::new(&seal.routes.segment_roots);
    let mut build_memory = CodeGraphMemoryV1::new(resident_memory, "code_graph.build.v1")?;
    spill_verified_seal_graph(
        &seal,
        &mut segments,
        projection,
        projector_revision,
        spill,
        &mut |bound| build_memory.resize(bound.peak_bytes()),
        check,
    )
}

/// Authenticates the seal `sealed_state_digest` names, read from the
/// canonical generations root or, once retention moved it, the replay pool,
/// and requires it to hold `generation`; a different generation is a
/// conflict at `site`.
fn open_seal_from_roots(
    generations_root: &std::path::Path,
    replay_root: &std::path::Path,
    resident_memory: &Arc<ProcessResidentMemoryV1>,
    sealed_state_digest: &SealedGraphStateDigest,
    generation: &tracedecay_domain::CodeGenerationId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    site: &'static str,
) -> Result<VerifiedSealGraphSourceV1, GraphDbError> {
    let digest = sha256_hex_suffix(sealed_state_digest.as_str())
        .ok_or_else(|| GraphDbError::invalid("sealed state digest is not sha256"))?;
    let seal_file = format!("generation-{digest}.json");
    let segments_root = code_generation_segments_root(
        generations_root
            .parent()
            .ok_or_else(|| GraphDbError::invalid("generation root has no store parent"))?,
    );
    let canonical = generations_root.join(&seal_file);
    let pool = replay_root.join(&seal_file);
    let mut memory = CodeGraphMemoryV1::new(resident_memory, "code_graph.manifest.v1")?;
    let manifest = with_verified_seal_from_roots(
        &canonical,
        &pool,
        digest,
        check,
        |path, expected_digest, check| {
            read_verified_seal_manifest(path, expected_digest, check, &mut memory)
        },
    )?;
    let admitted = open_verified_seal(manifest, digest, memory)?;
    if admitted.source.generation_id() != generation {
        return Err(GraphDbError::conflict(site));
    }
    let routes = SealRoutes {
        seal_paths: vec![canonical, pool],
        segment_roots: vec![segments_root],
    };
    Ok(VerifiedSealGraphSourceV1 { admitted, routes })
}

/// A layered row spill over a parent's sealed graph, or why it has none.
pub(super) type LayeredRowSpillV1 =
    Result<Result<GraphLayeredRowSpill, GraphSealedBaseAbsenceV1>, GraphDbError>;

/// The sealed graph a layered refresh builds its delta over.
pub(super) enum LayeredBaseV1<'a> {
    /// The graph the scope sealed for the seal's parent code generation.
    Parent(&'a tracedecay_domain::CodeGenerationId),
    /// A cold graph another scope of the same store sealed, the base of a
    /// scope's first generation: a linked worktree at its sibling's tree.
    Sibling(GraphSiblingSealedBaseV1),
}

/// A seal's graph rows as its publication seals them: a delta over the
/// sealed graph of the seal's parent code generation, or for a seal with no
/// parent over a sibling scope's sealed graph, when `layered_spill` resolves
/// one and the base's inputs admit it, the cold rows otherwise.
///
/// A layered attempt that fails for any reason but an interruption is
/// reported and replaced by the cold build, which is the authority a
/// layered generation must equal.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "daemon.session_registry.seal.graph_rows",
    level = "trace",
    skip_all
)]
pub(super) fn graph_rows_from_roots(
    generations_root: &std::path::Path,
    replay_root: &std::path::Path,
    resident_memory: &Arc<ProcessResidentMemoryV1>,
    sealed_state_digest: &SealedGraphStateDigest,
    generation: &tracedecay_domain::CodeGenerationId,
    projection: GraphProjectionIdentity,
    projector_revision: &GraphProjectorRevision,
    layered_spill: &dyn Fn(LayeredBaseV1<'_>) -> LayeredRowSpillV1,
    sibling_bases: &dyn Fn() -> Result<Vec<GraphSiblingSealedBaseV1>, GraphDbError>,
    cold_spill: &dyn Fn() -> Result<GraphGenerationRowSpill, GraphDbError>,
    admit: &mut dyn FnMut(
        tracedecay_code_index::production::CodeGraphBuildBoundV1,
    ) -> Result<(), GraphDbError>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(GraphGenerationRows, Option<CodeGraphLayeredReportV1>), GraphDbError> {
    let seal = open_seal_from_roots(
        generations_root,
        replay_root,
        resident_memory,
        sealed_state_digest,
        generation,
        check,
        "code_graph_manifest.graph_rows",
    )?;
    let mut segments = GraphSegmentReadPassV1::new(&seal.routes.segment_roots);
    let mut layered = |base: &str, spill: LayeredRowSpillV1| {
        layered_graph_rows(
            &seal,
            &mut segments,
            generation,
            &projection,
            projector_revision,
            base,
            spill,
            admit,
            check,
        )
    };
    if let Some(parent) = seal.admitted.source.manifest().parent_generation.clone() {
        if let Some(built) = layered(
            parent.as_str(),
            layered_spill(LayeredBaseV1::Parent(&parent)),
        )? {
            return Ok(built);
        }
    } else {
        let siblings = match sibling_bases() {
            Ok(siblings) => siblings,
            Err(error) if is_interruption(&error) => return Err(error),
            Err(error) => {
                tracing::warn!(
                    event = "code_graph_layered_refresh_unavailable",
                    generation = %generation,
                    error = %error,
                    "sibling sealed graphs could not be listed; sealing the generation cold"
                );
                Vec::new()
            }
        };
        for sibling in siblings {
            let base = sibling.base().generation().as_str().to_owned();
            if let Some(built) = layered(&base, layered_spill(LayeredBaseV1::Sibling(sibling)))? {
                return Ok(built);
            }
        }
    }
    let spilled = spill_verified_seal_graph(
        &seal,
        &mut segments,
        projection,
        projector_revision,
        cold_spill()?,
        admit,
        check,
    )?;
    Ok((spilled.into(), None))
}

fn is_interruption(error: &GraphDbError) -> bool {
    matches!(
        error,
        GraphDbError::Cancelled | GraphDbError::DeadlineExceeded | GraphDbError::SealSuperseded
    )
}

/// The seal's rows as a delta over `base`, or `None` when the base declines
/// or the attempt fails for any reason but an interruption.
#[allow(clippy::too_many_arguments)]
fn layered_graph_rows(
    seal: &VerifiedSealGraphSourceV1,
    segments: &mut GraphSegmentReadPassV1<'_>,
    generation: &tracedecay_domain::CodeGenerationId,
    projection: &GraphProjectionIdentity,
    projector_revision: &GraphProjectorRevision,
    base: &str,
    spill: LayeredRowSpillV1,
    admit: &mut dyn FnMut(
        tracedecay_code_index::production::CodeGraphBuildBoundV1,
    ) -> Result<(), GraphDbError>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Option<(GraphGenerationRows, Option<CodeGraphLayeredReportV1>)>, GraphDbError> {
    let layered = spill.and_then(|spill| match spill {
        Ok(spill) => with_verified_segments(
            &seal.admitted.sealed_state_digest,
            &seal.routes,
            segments,
            check,
            |read_pack| {
                let mut read_segment =
                    |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
                        seal.admitted
                            .source
                            .read_segment(request, buffer, &mut *read_pack)
                    };
                let mut admit_pages =
                    |bound| admit(page_build_bound(&seal.admitted.source, bound)?);
                build_layered_code_graph_rows(
                    projection.clone(),
                    &seal.admitted.source,
                    &mut read_segment,
                    projector_revision,
                    spill,
                    &mut admit_pages,
                    check,
                )
            },
        )
        .map(|built| built.map_err(|decline| format!("{decline:?}"))),
        Err(absence) => Ok(Err(format!("{absence:?}"))),
    });
    match layered {
        Ok(Ok(built)) => return Ok(Some((built.generation.into(), Some(built.report)))),
        Ok(Err(reason)) => tracing::info!(
            event = "code_graph_layered_refresh_declined",
            generation = %generation,
            base = %base,
            reason = %reason,
            "the refresh seals cold rather than as a delta over this base's graph"
        ),
        Err(error) if is_interruption(&error) => return Err(error),
        Err(error) => tracing::warn!(
            event = "code_graph_layered_refresh_unavailable",
            generation = %generation,
            base = %base,
            error = %error,
            "a layered graph refresh could not be built; sealing the generation cold"
        ),
    }
    Ok(None)
}

struct PinnedPartitionedSegmentV1 {
    digest: String,
    size_bytes: u64,
    file: File,
    fingerprint: Option<SealedSegmentFingerprintV1>,
    read_succeeded: bool,
}

/// A generation has one content pack and one evidence pack. Handles remain
/// owned by this operation across layered attempts and the cold fallback.
const MAX_GRAPH_SEGMENT_OPEN_HANDLES: usize = 2;

struct GraphSegmentReadPassV1<'a> {
    roots: &'a [PathBuf],
    pinned: Vec<PinnedPartitionedSegmentV1>,
    open_attempts: usize,
    open_wall_time: std::time::Duration,
    reads: usize,
}

impl<'a> GraphSegmentReadPassV1<'a> {
    fn new(roots: &'a [PathBuf]) -> Self {
        Self {
            roots,
            pinned: Vec::with_capacity(MAX_GRAPH_SEGMENT_OPEN_HANDLES),
            open_attempts: 0,
            open_wall_time: std::time::Duration::ZERO,
            reads: 0,
        }
    }

    fn pin(
        &mut self,
        request: SealedGenerationSegmentReadV1<'_>,
    ) -> Result<usize, CodeIndexProductionErrorV1> {
        let (digest, expected_size, _, _) = partitioned_segment_request(request)?;
        if let Some(position) = self
            .pinned
            .iter()
            .position(|pinned| pinned.digest == digest)
        {
            if self.pinned[position].size_bytes != expected_size {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed graph pack length differs from its pinned identity".to_owned(),
                ));
            }
            return Ok(position);
        }
        if self.pinned.len() == MAX_GRAPH_SEGMENT_OPEN_HANDLES {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed graph generation requested more than its two physical packs".to_owned(),
            ));
        }
        let started = std::time::Instant::now();
        let mut record_open = || {
            self.open_attempts += 1;
            #[cfg(test)]
            SEGMENT_OPENS.with(|opens| opens.set(opens.get() + 1));
        };
        let opened = select_partitioned_segment_root(self.roots, request, &mut record_open)
            .and_then(|(root, path_metadata)| {
                open_partitioned_segment(root, &path_metadata, request, &mut record_open)
            });
        self.open_wall_time += started.elapsed();
        let position = self.pinned.len();
        self.pinned.push(opened?);
        Ok(position)
    }

    fn read(
        &mut self,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        buffer.clear();
        let position = self.pin(request)?;
        self.reads += 1;
        let pinned = &mut self.pinned[position];
        let read = read_pinned_partitioned_segment(pinned, request, buffer);
        if read.is_ok() {
            pinned.read_succeeded = true;
        } else if !pinned.read_succeeded {
            // An unsuccessful first read never displaces a healthy role or
            // pins a failed cold handle across the caller's retry.
            self.pinned.remove(position);
        }
        read
    }

    fn witness(
        &self,
        request: SealedGenerationSegmentReadV1<'_>,
    ) -> Result<Option<SealedSegmentWitnessV1>, CodeIndexProductionErrorV1> {
        let (_, size_bytes, offset, length) = partitioned_segment_request(request)?;
        let digest = match request {
            SealedGenerationSegmentReadV1::Whole { digest, .. }
            | SealedGenerationSegmentReadV1::Range { digest, .. } => digest,
        };
        let pinned = self
            .pinned
            .iter()
            .find(|pinned| pinned.digest == digest.as_str() && pinned.size_bytes == size_bytes)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed pack witness has no admitted physical handle".to_owned(),
                )
            })?;
        Ok(SealedSegmentWitnessV1::new(
            digest,
            pinned.fingerprint,
            offset,
            length,
        ))
    }

    /// Revalidate a fixed number of handles once the operation finishes,
    /// without a metadata syscall for every logical extent. Cache reuse also
    /// requires the change-sensitive fingerprint to remain unchanged.
    fn revalidate(&self, proof: bool) -> Result<(), CodeIndexProductionErrorV1> {
        for pinned in &self.pinned {
            let metadata = pinned.file.metadata().map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation pack metadata cannot be revalidated: {error}"
                ))
            })?;
            if !metadata.is_file() || metadata.len() != pinned.size_bytes {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed generation pack length changed during the operation".to_owned(),
                ));
            }
            if proof
                && pinned.fingerprint.is_some()
                && SealedSegmentFingerprintV1::of(&metadata) != pinned.fingerprint
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed generation pack identity changed during authentication".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

fn partitioned_segment_request(
    request: tracedecay_code_index::production::SealedGenerationSegmentReadV1<'_>,
) -> Result<(&str, u64, u64, u64), tracedecay_code_index::production::CodeIndexProductionErrorV1> {
    use tracedecay_code_index::production::SealedGenerationSegmentReadV1;
    let (digest, expected_size, offset, length) = match request {
        SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
            (digest.as_str(), size_bytes, 0, size_bytes)
        }
        SealedGenerationSegmentReadV1::Range {
            digest,
            size_bytes,
            offset,
            length,
        } => (digest.as_str(), size_bytes, offset, length),
    };
    if offset
        .checked_add(length)
        .is_none_or(|end| end > expected_size)
    {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment range exceeds its manifest identity".to_owned(),
        ));
    }
    Ok((digest, expected_size, offset, length))
}

fn select_partitioned_segment_root<'a>(
    roots: &'a [PathBuf],
    request: tracedecay_code_index::production::SealedGenerationSegmentReadV1<'_>,
    record_open_attempt: &mut impl FnMut(),
) -> Result<(&'a std::path::Path, std::fs::Metadata), CodeIndexProductionErrorV1> {
    let (digest, _, _, _) = partitioned_segment_request(request)?;
    let digest = sha256_hex_suffix(digest).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed segment digest is not sha256".to_owned())
    })?;
    for root in roots {
        // Windows pathname metadata performs an open even when the candidate
        // is missing or its metadata will be refused. Unix probes do not.
        if cfg!(windows) {
            record_open_attempt();
        }
        match root
            .join(format!("segment-{digest}.json"))
            .symlink_metadata()
        {
            Ok(path_metadata) => return Ok((root, path_metadata)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CodeIndexProductionErrorV1::Contract(format!(
                    "sealed segment metadata cannot be read: {error}"
                )));
            }
        }
    }
    Err(CodeIndexProductionErrorV1::Contract(
        "sealed generation segment is absent from active routes".to_owned(),
    ))
}

#[tracing::instrument(name = "code_index.graph.segment.open", level = "trace", skip_all)]
fn open_partitioned_segment(
    segments_root: &std::path::Path,
    path_metadata: &std::fs::Metadata,
    request: tracedecay_code_index::production::SealedGenerationSegmentReadV1<'_>,
    mut record_open_attempt: impl FnMut(),
) -> Result<PinnedPartitionedSegmentV1, tracedecay_code_index::production::CodeIndexProductionErrorV1>
{
    use tracedecay_code_index::production::CodeIndexProductionErrorV1;

    let (digest, expected_size, _, _) = partitioned_segment_request(request)?;
    let digest_hex = sha256_hex_suffix(digest).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed segment digest is not sha256".to_owned())
    })?;
    let path = segments_root.join(format!("segment-{digest_hex}.json"));
    // Route selection already obtained this pathname identity. Re-statting
    // every graph page repeats an endpoint-scanned filesystem operation.
    if !path_metadata.file_type().is_file() || path_metadata.len() != expected_size {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment identity does not match its manifest".to_owned(),
        ));
    }
    record_open_attempt();
    let file = File::open(&path).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation segment cannot be opened: {error}"
        ))
    })?;
    let file_metadata = file.metadata().map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation segment metadata cannot be read: {error}"
        ))
    })?;
    if !same_file_identity(path_metadata, &file_metadata) {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment identity changed while it was opened".to_owned(),
        ));
    }
    #[cfg(windows)]
    if !same_windows_handle_identity(&file, &path, &mut record_open_attempt)
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?
    {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment identity changed while it was opened".to_owned(),
        ));
    }
    Ok(PinnedPartitionedSegmentV1 {
        digest: digest.to_owned(),
        size_bytes: expected_size,
        file,
        fingerprint: SealedSegmentFingerprintV1::of(&file_metadata),
        read_succeeded: false,
    })
}

fn read_pinned_partitioned_segment(
    pinned: &mut PinnedPartitionedSegmentV1,
    request: tracedecay_code_index::production::SealedGenerationSegmentReadV1<'_>,
    buffer: &mut Vec<u8>,
) -> Result<(), tracedecay_code_index::production::CodeIndexProductionErrorV1> {
    use tracedecay_code_index::production::CodeIndexProductionErrorV1;

    let (digest, expected_size, offset, length) = partitioned_segment_request(request)?;
    if digest != pinned.digest || expected_size != pinned.size_bytes {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation page does not match its pinned segment identity".to_owned(),
        ));
    }
    let length = usize::try_from(length).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation segment range exceeds addressable memory".to_owned(),
        )
    })?;
    read_segment_range(&mut pinned.file, offset, length, buffer).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation segment read failed: {error}"
        ))
    })
}

/// An interrupted or short page read must not publish any partially filled bytes.
fn read_segment_range<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    length: usize,
    buffer: &mut Vec<u8>,
) -> std::io::Result<()> {
    buffer.clear();
    buffer.resize(length, 0);
    let read = reader
        .seek(SeekFrom::Start(offset))
        .and_then(|_| reader.read_exact(buffer));
    if read.is_err() {
        buffer.clear();
    }
    read
}

#[cfg(test)]
fn segment_path(
    segments_root: &std::path::Path,
    digest: &ManifestDigest,
) -> Result<PathBuf, CodeIndexProductionErrorV1> {
    let digest_hex = sha256_hex_suffix(digest.as_str()).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed segment digest is not sha256".to_owned())
    })?;
    Ok(segments_root.join(format!("segment-{digest_hex}.json")))
}

#[tracing::instrument(
    name = "daemon.session_registry.seal.verify",
    level = "trace",
    skip_all
)]
fn verify_checked_seal(
    path: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    memory: &mut CodeGraphMemoryV1,
) -> Result<(File, Option<Vec<u8>>), GraphDbError> {
    let (mut reader, opened_metadata, admitted_len) =
        open_checked_seal_reader(path, check, memory)?;
    // Component proof must consume the very bytes this reader authenticates,
    // not a later reread of the same potentially changed inode.
    let manifest = read_checked_seal_manifest_for_proof(&mut reader, admitted_len, check, memory);

    if let Some(error) = reader.failure.take() {
        return Err(error);
    }
    let manifest = manifest?;
    let file = reader.finish(path, &opened_metadata, admitted_len, expected_digest)?;
    Ok((file, manifest))
}

#[tracing::instrument(
    name = "daemon.session_registry.seal.verify_bundle",
    level = "trace",
    skip_all
)]
fn verify_checked_seal_bundle(
    path: &std::path::Path,
    segments_root: &std::path::Path,
    expected_digest: &str,
    verified: &VerifiedSealedSegmentsV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    resident_memory: &Arc<ProcessResidentMemoryV1>,
) -> Result<(), GraphDbError> {
    verify_checked_seal_bundle_with_evidence_barrier(
        path,
        segments_root,
        expected_digest,
        verified,
        check,
        resident_memory,
        || {},
    )
}

/// Consume one forward-only stream, retaining a partitioned manifest for
/// component proof. Other revisions still drain through the checked reader
/// so their complete bytes are hashed without materializing the whole seal.
fn read_checked_seal_manifest_for_proof<R: Read>(
    file: &mut R,
    admitted_len: u64,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    memory: &mut CodeGraphMemoryV1,
) -> Result<Option<Vec<u8>>, GraphDbError> {
    // A successful file read may be short. Consume the entire bounded prefix
    // so a partial read cannot make a new-format seal bypass component proof.
    let prefix_len = admitted_len.min(SEAL_READ_CHECK_BYTES as u64) as usize;
    memory.resize((SEAL_READ_CHECK_BYTES + prefix_len) as u64)?;
    let mut prefix = vec![0; prefix_len];
    let read = file.read_exact(&mut prefix);
    // A scanned manifest read can finish after the deadline, even on I/O error.
    // Prefer that typed interruption to a misleading corruption result.
    (check)()?;
    read.map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed generation manifest prefix read failed: {error}"),
    })?;
    let revision_key = b"\"format_revision\":";
    let revision = prefix
        .windows(revision_key.len())
        .position(|window| window == revision_key)
        .and_then(|start| {
            let digits = &prefix[start + revision_key.len()..];
            let end = digits
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .unwrap_or(digits.len());
            std::str::from_utf8(&digits[..end])
                .ok()?
                .parse::<u32>()
                .ok()
        });
    let partitioned =
        revision == Some(tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1);
    let read = if partitioned {
        let length = usize::try_from(admitted_len)
            .map_err(|_| GraphDbError::invalid("sealed manifest length does not fit memory"))?;
        // A moving reallocation can briefly retain both buffers.
        memory.resize(
            admitted_len.saturating_add((SEAL_READ_CHECK_BYTES + prefix.capacity()) as u64),
        )?;
        prefix.reserve_exact(length.saturating_sub(prefix.len()));
        prefix.resize(length, 0);
        memory.resize((SEAL_READ_CHECK_BYTES + prefix.capacity()) as u64)?;
        file.read_exact(&mut prefix[prefix_len..])
    } else {
        std::io::copy(file, &mut std::io::sink()).map(|_| ())
    };
    (check)()?;
    read.map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed generation manifest read failed: {error}"),
    })?;
    Ok(partitioned.then_some(prefix))
}

fn verify_checked_seal_bundle_with_evidence_barrier(
    path: &std::path::Path,
    segments_root: &std::path::Path,
    expected_digest: &str,
    verified: &VerifiedSealedSegmentsV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    resident_memory: &Arc<ProcessResidentMemoryV1>,
    evidence_barrier: impl FnOnce(),
) -> Result<(), GraphDbError> {
    let started = std::time::Instant::now();
    let mut memory = CodeGraphMemoryV1::new(resident_memory, "code_graph.manifest.v1")?;
    let (file, manifest) = verify_checked_seal(path, expected_digest, check, &mut memory)?;
    let Some(manifest) = manifest else {
        return Ok(());
    };
    let mut admitted = open_verified_seal(manifest, expected_digest, memory)?;
    admitted.memory.resize(
        (retained_source_bytes(&admitted.source, &admitted.sealed_state_digest) as u64)
            .saturating_add(admitted.source.verification_scratch_bytes() as u64),
    )?;
    let source = &admitted.source;
    let roots = [segments_root.to_path_buf()];
    let segments = std::cell::RefCell::new(GraphSegmentReadPassV1::new(&roots));
    let mut evidence_barrier = Some(evidence_barrier);
    let mut interruption = None;
    let proof = std::cell::RefCell::new(verified.begin(segments_root));
    // Physical pack and logical extent authentication share one bounded set
    // of file handles. Only a complete proof may publish their witnesses.
    let reused_segments = std::cell::Cell::new(0_u64);
    let hashed_segments = std::cell::Cell::new(0_u64);
    let range_reads = std::cell::Cell::new(0_u64);
    let range_bytes = std::cell::Cell::new(0_u64);
    let verified = source
        .verify_segments(
            |digest, expected_size| {
                let witness = source
                    .resolve_pack_request(SealedGenerationSegmentReadV1::Whole {
                        digest,
                        size_bytes: expected_size,
                    })
                    .and_then(|request| {
                        let mut segments = segments.borrow_mut();
                        segments.pin(request)?;
                        segments.witness(request)
                    })
                    .ok()
                    .flatten();
                let reused = proof.borrow_mut().already_verified(digest, witness);
                if reused {
                    reused_segments.set(reused_segments.get().saturating_add(1));
                } else {
                    hashed_segments.set(hashed_segments.get().saturating_add(1));
                }
                reused
            },
            |request, buffer| {
                buffer.clear();
                if let Err(error) = (check)() {
                    if matches!(
                        error,
                        GraphDbError::Cancelled | GraphDbError::DeadlineExceeded
                    ) {
                        interruption = Some(error.clone());
                    }
                    return Err(CodeIndexProductionErrorV1::Contract(error.to_string()));
                }
                let read = (|| -> Result<(), CodeIndexProductionErrorV1> {
                    let (digest, _, _, length) = partitioned_segment_request(request)?;
                    if digest == source.evidence_pack().0.as_str() {
                        // The barrier observes the real retained evidence handle.
                        segments.borrow_mut().pin(request)?;
                        if let Some(barrier) = evidence_barrier.take() {
                            barrier();
                        }
                    }
                    segments.borrow_mut().read(request, buffer)?;
                    range_reads.set(range_reads.get().saturating_add(1));
                    range_bytes.set(range_bytes.get().saturating_add(length));
                    Ok(())
                })();
                if let Err(error) = (check)() {
                    buffer.clear();
                    if matches!(
                        error,
                        GraphDbError::Cancelled | GraphDbError::DeadlineExceeded
                    ) {
                        interruption = Some(error.clone());
                    }
                    return Err(CodeIndexProductionErrorV1::Contract(error.to_string()));
                }
                read
            },
        )
        .and_then(|()| {
            let segments = segments.borrow();
            segments.revalidate(true)?;
            let mut proof = proof.borrow_mut();
            let content = source.content_pack();
            for (digest, size_bytes) in std::iter::once((&content.digest, content.size_bytes))
                .chain(std::iter::once((
                    source.evidence_pack().0,
                    source.evidence_pack().1,
                )))
                .chain(
                    content
                        .extents
                        .iter()
                        .map(|extent| (&extent.digest, extent.size_bytes)),
                )
            {
                let request =
                    source.resolve_pack_request(SealedGenerationSegmentReadV1::Whole {
                        digest,
                        size_bytes,
                    })?;
                proof.hashed(digest, segments.witness(request)?);
            }
            Ok(())
        });
    if let Some(interruption) = interruption {
        return Err(interruption);
    }
    (check)()?;
    if let Err(error) = verified {
        // The segment sweep collects a seal's segments only once no root
        // names the seal, so a missing segment of a retired seal is its
        // retirement, and of a live seal is corruption.
        let opened_metadata = file.metadata();
        (check)()?;
        let opened_metadata = opened_metadata.map_err(|error| GraphDbError::Corrupt {
            message: format!("sealed code generation metadata cannot be revalidated: {error}"),
        })?;
        let left_path = seal_left_path(path, &opened_metadata);
        (check)()?;
        if left_path? {
            return Err(GraphDbError::SealSuperseded);
        }
        return Err(GraphDbError::Corrupt {
            message: format!("sealed generation component verification failed: {error}"),
        });
    }
    (check)()?;
    proof.into_inner().commit();
    tracing::info!(
        target: "tracedecay::sealed_source_proof",
        event = %"sealed_source_proof",
        seal_sha256 = expected_digest,
        reused_segments = reused_segments.get(),
        hashed_segments = hashed_segments.get(),
        range_reads = range_reads.get(),
        range_bytes = range_bytes.get(),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "verified sealed source authentication work"
    );
    Ok(())
}

/// Proves that the durable source backing an already-decoded generation still
/// exists under its canonical-or-retained authority with the exact digest.
/// This reads and hashes the bounded source without decoding or projecting it.
pub(super) fn verify_sealed_generation_source_from_roots(
    generations_root: &std::path::Path,
    replay_root: &std::path::Path,
    resident_memory: &Arc<ProcessResidentMemoryV1>,
    sealed_state_digest: &SealedGraphStateDigest,
    verified: &VerifiedSealedSegmentsV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let digest = sha256_hex_suffix(sealed_state_digest.as_str())
        .ok_or_else(|| GraphDbError::invalid("sealed state digest is not sha256"))?;
    let seal_file = format!("generation-{digest}.json");
    let segments_root = code_generation_segments_root(
        generations_root
            .parent()
            .ok_or_else(|| GraphDbError::invalid("generation root has no store parent"))?,
    );
    with_verified_seal_from_roots(
        &generations_root.join(&seal_file),
        &replay_root.join(&seal_file),
        digest,
        check,
        |path, expected_digest, check| {
            verify_checked_seal_bundle(
                path,
                &segments_root,
                expected_digest,
                verified,
                check,
                resident_memory,
            )
        },
    )
}

/// The exact repository and worktree source retained by a runtime.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CodeGenerationRouteV1 {
    repository: RepositoryId,
    generations_root: PathBuf,
}

#[derive(Clone)]
struct BoundCodeGenerationSourceV1 {
    project_shard: StoreShardIdV1,
    project_id: ProjectId,
    replay_root: PathBuf,
    resident_memory: Arc<ProcessResidentMemoryV1>,
    routes: BTreeMap<CodeGenerationRouteV1, usize>,
}

/// A runtime owns one reference; equal routes retire only with their last owner.
#[must_use = "retain the route guard for the runtime's lifetime"]
pub(super) struct CodeGraphManifestRouteV1 {
    provider: Arc<DaemonCodeGraphManifestProviderV1>,
    shard: StoreShardIdV1,
    route: CodeGenerationRouteV1,
}

impl Drop for CodeGraphManifestRouteV1 {
    fn drop(&mut self) {
        let mut sources = match self.provider.sources.write() {
            Ok(sources) => sources,
            Err(error) => {
                tracing::error!(%error, "cannot retire poisoned graph replay route registry");
                return;
            }
        };
        if let Some(binding) = sources.get_mut(&self.shard) {
            if let Some(references) = binding.routes.get_mut(&self.route) {
                *references -= 1;
                if *references == 0 {
                    binding.routes.remove(&self.route);
                }
            }
            if binding.routes.is_empty() {
                sources.remove(&self.shard);
            }
        }
    }
}

pub(super) struct DaemonCodeGraphManifestProviderV1 {
    sources: RwLock<BTreeMap<StoreShardIdV1, BoundCodeGenerationSourceV1>>,
    /// The measured-RSS cell sealed publication answers to, so the one
    /// admission authority governs the corpus-sized build.
    pressure: Arc<ResidentMemoryPressureV1>,
}

impl Default for DaemonCodeGraphManifestProviderV1 {
    fn default() -> Self {
        Self::with_pressure(
            tracedecay_runtime_core::resident_memory::process_resident_memory_pressure_v1(),
        )
    }
}

impl DaemonCodeGraphManifestProviderV1 {
    /// Bind the provider to a measured-RSS pressure cell.
    ///
    /// Production passes the process cell fed by the daemon's `VmRSS` sampler.
    /// Tests pass an isolated cell so a fake RSS series drives the refusal
    /// without touching `/proc` or other cases.
    pub(super) fn with_pressure(pressure: &Arc<ResidentMemoryPressureV1>) -> Self {
        Self {
            sources: RwLock::new(BTreeMap::new()),
            pressure: Arc::clone(pressure),
        }
    }

    /// The measured-RSS pressure cell this provider was bound to.
    pub(super) fn resident_memory_pressure(&self) -> &Arc<ResidentMemoryPressureV1> {
        &self.pressure
    }

    pub(super) fn bind(
        self: &Arc<Self>,
        project_shard: StoreShardIdV1,
        project_id: ProjectId,
        repository: RepositoryId,
        generations_root: PathBuf,
        replay_root: PathBuf,
        resident_memory: Arc<ProcessResidentMemoryV1>,
    ) -> Result<CodeGraphManifestRouteV1, GraphDbError> {
        let mut sources = self.sources.write().map_err(|_| {
            GraphDbError::unavailable("code generation manifest provider lock is poisoned")
        })?;
        let route = CodeGenerationRouteV1 {
            repository,
            generations_root,
        };
        if let Some(existing) = sources.get_mut(&project_shard) {
            if existing.project_id != project_id
                || existing.replay_root != replay_root
                || !Arc::ptr_eq(&existing.resident_memory, &resident_memory)
            {
                return Err(GraphDbError::conflict("code_graph_manifest.bind"));
            }
            let references = existing.routes.entry(route.clone()).or_default();
            *references = references.checked_add(1).ok_or_else(|| {
                GraphDbError::unavailable("code graph replay route reference count overflow")
            })?;
        } else {
            sources.insert(
                project_shard.clone(),
                BoundCodeGenerationSourceV1 {
                    project_shard: project_shard.clone(),
                    project_id,
                    replay_root,
                    resident_memory,
                    routes: BTreeMap::from([(route.clone(), 1)]),
                },
            );
        }
        Ok(CodeGraphManifestRouteV1 {
            provider: Arc::clone(self),
            shard: project_shard,
            route,
        })
    }
}

impl GraphGenerationManifestProvider for DaemonCodeGraphManifestProviderV1 {
    fn hydrate_sealed_code_generation(
        &self,
        owner: &GraphProjectionIdentityV1,
        source: &SealedCodeGenerationReplay,
        spill: GraphGenerationRowSpill,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<SpilledGraphGeneration, GraphDbError> {
        check()?;
        let binding = self
            .sources
            .read()
            .map_err(|_| {
                GraphDbError::unavailable("code generation manifest provider lock is poisoned")
            })?
            .get(&owner.shard_id)
            .cloned()
            .ok_or_else(|| {
                GraphDbError::unavailable(
                    "sealed code generation replay source is not mounted for this projection",
                )
            })?;
        if owner.shard_id != binding.project_shard
            || !binding
                .routes
                .keys()
                .any(|route| route.repository == source.repository)
        {
            return Err(GraphDbError::conflict(
                "code_graph_manifest.hydrate_sealed_code_generation",
            ));
        }
        let tracedecay_store::StoreShardScopeV1::Project { project_id } =
            &binding.project_shard.scope
        else {
            return Err(GraphDbError::conflict(
                "code_graph_manifest.hydrate_sealed_code_generation",
            ));
        };
        if project_id != &binding.project_id {
            return Err(GraphDbError::conflict(
                "code_graph_manifest.hydrate_sealed_code_generation",
            ));
        }
        let digest = sha256_hex_suffix(source.sealed_state_digest.as_str())
            .ok_or_else(|| GraphDbError::invalid("sealed state digest is not sha256"))?;
        let seal_file = format!("generation-{digest}.json");
        let routes = binding
            .routes
            .keys()
            .filter(|route| route.repository == source.repository)
            .collect::<Vec<_>>();
        let segment_roots = routes
            .iter()
            .map(|route| {
                route
                    .generations_root
                    .parent()
                    .map(code_generation_segments_root)
                    .ok_or_else(|| {
                        GraphDbError::invalid("canonical generation root has no store parent")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut memory =
            CodeGraphMemoryV1::new(&binding.resident_memory, "code_graph.manifest.v1")?;
        let mut manifest = None;
        let mut canonical_error = None;
        for route in &routes {
            check()?;
            let canonical = route.generations_root.join(&seal_file);
            // Presence is only a prefilter: retention may move the seal before
            // the read opens it, and the single replay-pool probe below
            // resolves that move.
            if !seal_is_present(&canonical)? {
                continue;
            }
            match read_verified_seal_manifest(&canonical, digest, check, &mut memory) {
                Ok(bytes) => manifest = Some(bytes),
                Err(error) if is_seal_read_refusal(&error) => {
                    return Err(error);
                }
                Err(error) => canonical_error = Some(error),
            }
            break;
        }
        let manifest = match manifest {
            Some(manifest) => manifest,
            None => {
                let pool = binding.replay_root.join(&seal_file);
                if !seal_is_present(&pool)? {
                    return Err(canonical_error.unwrap_or_else(|| {
                        GraphDbError::unavailable(
                            "sealed code generation is absent from all active routes and replay pool",
                        )
                    }));
                }
                read_verified_seal_manifest(&pool, digest, check, &mut memory).map_err(|error| {
                    if is_seal_read_refusal(&error) {
                        error
                    } else {
                        canonical_error.unwrap_or(error)
                    }
                })?
            }
        };
        let admitted = open_verified_seal(manifest, digest, memory)?;
        if admitted.source.manifest().project_id != binding.project_id
            || admitted.source.snapshot().repository != source.repository
            || admitted.source.generation_id() != &source.generation
        {
            return Err(GraphDbError::conflict(
                "code_graph_manifest.hydrate_sealed_code_generation",
            ));
        }
        let seal = VerifiedSealGraphSourceV1 {
            admitted,
            routes: SealRoutes {
                seal_paths: routes
                    .iter()
                    .map(|route| route.generations_root.join(&seal_file))
                    .chain(std::iter::once(binding.replay_root.join(&seal_file)))
                    .collect(),
                segment_roots,
            },
        };
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new(owner.namespace.as_str())?,
            GraphProjectionId::new(owner.projection.as_str())?,
        );
        // The replay, not the current reader, owns the projector revision at
        // this boundary. An interrupted historical publication must be able
        // to reconstruct its exact rows so the ordered journal can advance;
        // the registry compares the rebuilt digests with the durable replay
        // before any row is served.
        let mut segments = GraphSegmentReadPassV1::new(&seal.routes.segment_roots);
        let mut build_memory =
            CodeGraphMemoryV1::new(&binding.resident_memory, "code_graph.build.v1")?;
        spill_verified_seal_graph(
            &seal,
            &mut segments,
            projection,
            &source.projector_revision,
            spill,
            &mut |bound| build_memory.resize(bound.peak_bytes()),
            check,
        )
    }
}

/// Interruptions from the caller's `check` probe are transport states, not
/// evidence about the sealed payload. Classifying them as corruption would
/// fault-retain the graph slot in the shared capacity-bounded registry and
/// poison later retries of the same immutable artifact.
fn classify_sealed_projection_build_error(error: CodeGraphProjectionError) -> GraphDbError {
    match error {
        CodeGraphProjectionError::Cancelled => GraphDbError::Cancelled,
        CodeGraphProjectionError::DeadlineExceeded => GraphDbError::DeadlineExceeded,
        CodeGraphProjectionError::BudgetExhausted { budget, limit } => {
            // Preserve the exact budget identity across the round-trip; an
            // unrecognized name is a projection-local budget, reported under
            // the read class with its real limit rather than a fabricated one.
            let kind = GraphBudgetKind::from_name(&budget).unwrap_or(GraphBudgetKind::Read);
            GraphDbError::budget_exhausted(kind, limit)
        }
        other => GraphDbError::Corrupt {
            message: format!("sealed code generation graph projection is invalid: {other}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::num::NonZeroU64;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_code_index::graph_projection::build_sealed_code_graph_rows;
    use tracedecay_code_index::production::{
        CodeIndexProductionErrorV1, SealedGenerationPackLayoutV1, SealedGenerationSegmentReadV1,
    };
    use tracedecay_code_index_retention::code_index_generations::{
        DurablePublicationPointerV1, acquire_code_generation_store_lock,
        code_generation_segments_root,
    };
    use tracedecay_domain::{
        CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, sha256_hex_suffix,
    };
    use tracedecay_graph_db::{
        GraphDbError, GraphGenerationManifestProvider, GraphGenerationRowSpill, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
        SealedCodeGenerationReplay, SealedGraphStateDigest, SpilledGraphGeneration,
    };
    use tracedecay_runtime_core::resident_memory::{
        ProcessResidentMemoryV1, ResidentMemoryPressureV1,
    };
    use tracedecay_store::{
        BrainId, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1, StoreShardIdV1,
        UserProfileId,
    };

    use super::{
        DaemonCodeGraphManifestProviderV1, SEAL_READ_CHECK_BYTES, SEGMENT_OPENS, SealRoutes,
        VerifiedSealedSegmentsV1, open_seal_from_roots, read_verified_seal_manifest,
        spill_sealed_generation_graph_from_roots, validate_sealed_generation_metadata,
        verify_checked_seal, verify_checked_seal_bundle,
        verify_checked_seal_bundle_with_evidence_barrier,
        verify_sealed_generation_source_from_roots,
    };
    use tracedecay_code_index_runtime::code_index_scheduler::{
        CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1, scoped_code_index_store_root,
    };

    fn test_resident_memory() -> Arc<ProcessResidentMemoryV1> {
        test_resident_memory_with_limit(1024 * 1024 * 1024)
    }

    fn test_resident_memory_with_limit(limit: u64) -> Arc<ProcessResidentMemoryV1> {
        let limit = NonZeroU64::new(limit).unwrap();
        Arc::new(ProcessResidentMemoryV1::with_pressure(
            limit,
            Arc::new(ResidentMemoryPressureV1::with_sampler(
                limit,
                Arc::new(|| None),
            )),
        ))
    }

    fn test_manifest_memory() -> super::CodeGraphMemoryV1 {
        super::CodeGraphMemoryV1::new(&test_resident_memory(), "code_graph.manifest.v1").unwrap()
    }

    /// Exercise one complete reader operation with the same owned lifetime as hydration.
    fn with_verified_test_segments<T>(
        digest: &ManifestDigest,
        routes: &SealRoutes,
        check: &dyn Fn() -> Result<(), GraphDbError>,
        build: impl FnOnce(
            &mut tracedecay_code_index::production::SealedGenerationSegmentReaderV1<'_>,
        ) -> Result<
            T,
            tracedecay_code_index::graph_projection::SealedCodeGraphRowsError,
        >,
    ) -> Result<T, GraphDbError> {
        let mut segments = super::GraphSegmentReadPassV1::new(&routes.segment_roots);
        super::with_verified_segments(digest, routes, &mut segments, check, build)
    }

    #[test]
    fn checked_seal_reader_refuses_late_interruption_after_nonzero_file_read() {
        let temporary = TempDir::new().expect("seal reader fixture");
        let path = temporary.path().join("generation.json");
        let expected = b"{\"format_revision\":1,\"pages\":[1]}";
        std::fs::write(&path, expected).expect("write real sealed bytes");

        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            let probes = Cell::new(0_usize);
            let check = || {
                let next = probes.get() + 1;
                probes.set(next);
                if next == 2 {
                    Err(interruption.clone())
                } else {
                    Ok(())
                }
            };
            let mut reader = super::CheckedSealReader {
                reader: std::io::BufReader::with_capacity(
                    SEAL_READ_CHECK_BYTES,
                    std::fs::File::open(&path).expect("open sealed bytes"),
                ),
                check: &check,
                bytes_read: 0,
                digest: Sha256::new(),
                failure: None,
            };
            let mut output = vec![0xa5; expected.len()];
            assert!(
                reader.read(&mut output).is_err(),
                "interrupted read cannot return verified bytes"
            );
            assert_eq!(
                reader
                    .reader
                    .stream_position()
                    .expect("actual file position"),
                expected.len() as u64,
                "the late probe must occur after real nonempty file I/O"
            );
            assert_eq!(probes.get(), 2, "one pre-read and one post-read probe");
            assert_eq!(reader.bytes_read, 0, "interrupted bytes are not hashed");
            assert!(
                output.iter().all(|byte| *byte == 0),
                "interrupted data must not escape through the caller buffer"
            );
            assert!(matches!(
                (&reader.failure, &interruption),
                (Some(GraphDbError::Cancelled), GraphDbError::Cancelled)
                    | (
                        Some(GraphDbError::DeadlineExceeded),
                        GraphDbError::DeadlineExceeded
                    )
            ));
        }

        let check = || Ok(());
        let mut reader = super::CheckedSealReader {
            reader: std::io::BufReader::with_capacity(
                SEAL_READ_CHECK_BYTES,
                std::fs::File::open(&path).expect("reopen sealed bytes"),
            ),
            check: &check,
            bytes_read: 0,
            digest: Sha256::new(),
            failure: None,
        };
        let mut output = vec![0; expected.len()];
        assert_eq!(
            reader.read(&mut output).expect("uninterrupted file read"),
            expected.len()
        );
        assert_eq!(output, expected);
        assert_eq!(reader.bytes_read, expected.len() as u64);
    }

    #[test]
    fn checked_seal_finish_preserves_deadline_after_final_file_and_path_io() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("generation.json");
        let expected = b"real nonempty sealed generation for final identity checks";
        let digest = hex::encode(Sha256::digest(expected));
        std::fs::write(&path, expected).unwrap();

        // The stream is already authenticated. Expiry during the final
        // handle-metadata or pathname witness must not return a verified seal.
        for (late_probe, interruption) in [
            (2, GraphDbError::Cancelled),
            (3, GraphDbError::DeadlineExceeded),
        ] {
            let finishing = Cell::new(false);
            let finish_checks = Cell::new(0_usize);
            let check = || {
                if finishing.get() {
                    let next = finish_checks.get() + 1;
                    finish_checks.set(next);
                    if next == 2 && late_probe == 3 && cfg!(unix) {
                        // Retire the pathname after the handle metadata probe.
                        // A deadline at the path recheck outranks supersession.
                        std::fs::remove_file(&path).unwrap();
                    }
                    if next == late_probe {
                        return Err(interruption.clone());
                    }
                }
                Ok(())
            };
            let mut memory = test_manifest_memory();
            let (mut reader, opened_metadata, admitted_len) =
                super::open_checked_seal_reader(&path, &check, &mut memory).unwrap();
            let mut actual = Vec::new();
            reader.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, expected, "finish must follow real nonempty I/O");
            assert_eq!(reader.bytes_read, admitted_len);

            finishing.set(true);
            assert_eq!(
                reader
                    .finish(&path, &opened_metadata, admitted_len, &digest)
                    .unwrap_err(),
                interruption
            );
            assert_eq!(
                finish_checks.get(),
                late_probe,
                "the interruption must be observed after final filesystem I/O"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn checked_seal_open_preserves_deadline_after_windows_identity_io() {
        let temporary = TempDir::new().expect("Windows seal identity fixture");
        let path = temporary.path().join("generation.json");
        std::fs::write(&path, b"actual nonempty seal").expect("create real seal file");

        // The fourth probe belongs after the additional Windows pathname
        // handle check. On the predecessor that operation has no post-I/O
        // check, so this returns a seemingly admitted reader instead.
        let probes = Cell::new(0_usize);
        let check = || {
            let next = probes.get() + 1;
            probes.set(next);
            if next == 4 {
                Err(GraphDbError::DeadlineExceeded)
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            super::open_checked_seal_reader(&path, &check, &mut test_manifest_memory()),
            Err(GraphDbError::DeadlineExceeded)
        ));
        assert_eq!(probes.get(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn checked_seal_reader_preserves_late_deadline_after_real_io_failure() {
        let temporary = TempDir::new().expect("failed read fixture");
        let mut direct = std::fs::File::open(temporary.path()).expect("open directory handle");
        assert!(
            direct.read(&mut [0_u8; 1]).is_err(),
            "a directory read must actually fail on this platform"
        );

        let probes = Cell::new(0_usize);
        let check = || {
            let next = probes.get() + 1;
            probes.set(next);
            if next == 2 {
                Err(GraphDbError::DeadlineExceeded)
            } else {
                Ok(())
            }
        };
        let mut reader = super::CheckedSealReader {
            reader: std::io::BufReader::with_capacity(
                SEAL_READ_CHECK_BYTES,
                std::fs::File::open(temporary.path()).expect("open second directory handle"),
            ),
            check: &check,
            bytes_read: 0,
            digest: Sha256::new(),
            failure: None,
        };
        let mut output = [0xa5; 8];
        assert!(reader.read(&mut output).is_err());
        assert_eq!(probes.get(), 2, "the failure must be followed by a probe");
        assert!(matches!(
            reader.failure,
            Some(GraphDbError::DeadlineExceeded)
        ));
        assert_eq!(output, [0; 8], "failed reads must expose no stale bytes");
        assert_eq!(reader.bytes_read, 0);
    }

    /// A fresh row spill for `owner`'s projection, removed with the spill.
    fn spill_for(owner: &GraphProjectionIdentityV1) -> GraphGenerationRowSpill {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        GraphGenerationRowSpill::create(
            std::env::temp_dir().join(format!(
                "tracedecay-provider-spill-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )),
            GraphProjectionIdentity::new(
                GraphNamespace::new(owner.namespace.as_str()).unwrap(),
                GraphProjectionId::new(owner.projection.as_str()).unwrap(),
            ),
        )
        .unwrap()
    }

    /// Hydrates `source` the way the registry does, into a fresh spill.
    fn hydrate(
        provider: &DaemonCodeGraphManifestProviderV1,
        owner: &GraphProjectionIdentityV1,
        source: &SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<SpilledGraphGeneration, GraphDbError> {
        provider.hydrate_sealed_code_generation(owner, source, spill_for(owner), check)
    }

    fn fixture(
        generations_root: std::path::PathBuf,
        replay_root: std::path::PathBuf,
    ) -> (
        Arc<DaemonCodeGraphManifestProviderV1>,
        super::CodeGraphManifestRouteV1,
        GraphProjectionIdentityV1,
        SealedCodeGenerationReplay,
    ) {
        let project = ProjectId::new("project.provider").unwrap();
        let repository = RepositoryId::new("repository.provider").unwrap();
        let shard = StoreShardIdV1::project(
            BrainId::new("brain.provider").unwrap(),
            UserProfileId::new("profile.provider").unwrap(),
            project.clone(),
        );
        let provider = Arc::new(DaemonCodeGraphManifestProviderV1::default());
        let route = provider
            .bind(
                shard.clone(),
                project,
                repository.clone(),
                generations_root,
                replay_root,
                test_resident_memory(),
            )
            .unwrap();
        (
            provider,
            route,
            GraphProjectionIdentityV1 {
                shard_id: shard,
                namespace: GraphNamespaceV1::new("namespace.provider").unwrap(),
                projection: GraphProjectionIdV1::new("code-generation").unwrap(),
            },
            SealedCodeGenerationReplay {
                repository,
                generation: CodeGenerationId::new("generation.provider").unwrap(),
                sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                    "sha256:{}",
                    "a".repeat(64)
                ))
                .unwrap(),
                projector_revision: GraphProjectorRevision::try_from(
                    tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION
                        .to_owned(),
                )
                .unwrap(),
            },
        )
    }

    #[test]
    fn exact_seal_provider_rejects_missing_corrupt_and_foreign_sources() {
        let temp = TempDir::new().unwrap();
        let generations_root = temp.path().join("generations");
        let replay_root = temp.path().join("replay");
        std::fs::create_dir_all(&generations_root).unwrap();
        std::fs::create_dir_all(&replay_root).unwrap();
        let (provider, _route, owner, source) =
            fixture(generations_root.clone(), replay_root.clone());
        let seal_file = format!(
            "generation-{}.json",
            sha256_hex_suffix(source.sealed_state_digest.as_str()).unwrap()
        );

        assert!(matches!(
            hydrate(&provider, &owner, &source, &|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));

        let mut foreign = source.clone();
        foreign.repository = RepositoryId::new("repository.foreign").unwrap();
        assert!(matches!(
            hydrate(&provider, &owner, &foreign, &|| Ok(())).unwrap_err(),
            GraphDbError::Conflict { .. }
        ));

        // A retired seal that only survives in the replay pool is still read.
        std::fs::write(replay_root.join(&seal_file), b"corrupt").unwrap();
        assert!(matches!(
            hydrate(&provider, &owner, &source, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));

        // A canonical read failure is authoritative over a failing pool probe.
        std::fs::remove_file(replay_root.join(&seal_file)).unwrap();
        std::fs::write(generations_root.join(&seal_file), b"corrupt").unwrap();
        assert!(matches!(
            hydrate(&provider, &owner, &source, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

    /// A linked worktree shares its project's shard while sealing into its own
    /// code-index store. Rebinding that shard with the worktree's roots used to
    /// be refused as `code_graph_manifest.bind`, which failed every branch
    /// publication; the roots are a lookup route, not the source identity.
    #[test]
    fn one_shard_admits_every_worktree_route_and_reads_the_seal_from_each() {
        let temp = TempDir::new().unwrap();
        let primary_generations = temp.path().join("primary/generations");
        let primary_replay = temp.path().join("primary/replay");
        let branch_generations = temp.path().join("branch/generations");
        let branch_replay = primary_replay.clone();
        for root in [
            &primary_generations,
            &primary_replay,
            &branch_generations,
            &branch_replay,
        ] {
            std::fs::create_dir_all(root).unwrap();
        }
        let (provider, _route, owner, source) = fixture(primary_generations, primary_replay);
        let resident_memory = Arc::clone(
            &provider
                .sources
                .read()
                .unwrap()
                .get(&owner.shard_id)
                .unwrap()
                .resident_memory,
        );

        let _branch_route = provider
            .bind(
                owner.shard_id.clone(),
                ProjectId::new("project.provider").unwrap(),
                source.repository.clone(),
                branch_generations.clone(),
                branch_replay.clone(),
                Arc::clone(&resident_memory),
            )
            .unwrap();

        // Neither route holds the seal: the shard abstains rather than claiming
        // corruption.
        assert!(matches!(
            hydrate(&provider, &owner, &source, &|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));

        // Only the branch worktree's store holds it, and the read reaches there.
        let seal_file = format!(
            "generation-{}.json",
            sha256_hex_suffix(source.sealed_state_digest.as_str()).unwrap()
        );
        std::fs::write(branch_generations.join(&seal_file), b"corrupt").unwrap();
        assert!(matches!(
            hydrate(&provider, &owner, &source, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));

        // A genuinely different source under the same shard stays fatal.
        assert!(matches!(
            provider.bind(
                owner.shard_id.clone(),
                ProjectId::new("project.foreign").unwrap(),
                source.repository.clone(),
                branch_generations,
                branch_replay,
                Arc::clone(&resident_memory),
            ),
            Err(GraphDbError::Conflict { .. })
        ));
    }

    #[test]
    fn seal_interruptions_surface_without_probing_the_replay_pool() {
        let temp = TempDir::new().unwrap();
        let generations_root = temp.path().join("generations");
        let replay_root = temp.path().join("replay");
        std::fs::create_dir_all(&generations_root).unwrap();
        std::fs::create_dir_all(&replay_root).unwrap();
        let (provider, _route, owner, source) =
            fixture(generations_root.clone(), replay_root.clone());
        let seal_file = format!(
            "generation-{}.json",
            sha256_hex_suffix(source.sealed_state_digest.as_str()).unwrap()
        );
        std::fs::write(generations_root.join(&seal_file), b"canonical").unwrap();
        std::fs::write(replay_root.join(&seal_file), b"pool").unwrap();

        // Pass the entry probe, then cancel during the canonical read: the
        // typed interruption must surface without a second read against the
        // pool copy (which would probe the closure again).
        let probes = AtomicUsize::new(0);
        assert_eq!(
            hydrate(&provider, &owner, &source, &|| {
                if probes.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(())
                } else {
                    Err(GraphDbError::Cancelled)
                }
            })
            .unwrap_err(),
            GraphDbError::Cancelled
        );
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn sealed_generation_metadata_rejects_oversized_sparse_source_before_allocation() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("oversized.json");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(tracedecay_code_index::production::MAX_SEALED_CODE_GENERATION_BYTES_V1 + 1)
            .unwrap();
        let metadata = path.symlink_metadata().unwrap();

        assert!(matches!(
            validate_sealed_generation_metadata(&metadata),
            Err(GraphDbError::ResetRequired { .. })
        ));
    }

    #[test]
    fn checked_seal_open_preserves_interruption_after_metadata_io() {
        let temporary = TempDir::new().unwrap();
        let directory = temporary.path().join("sealed-directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("nonempty-evidence"),
            b"real filesystem evidence",
        )
        .unwrap();
        let missing = temporary.path().join("absent-seal");
        let digest = hex::encode(Sha256::digest(b"real filesystem evidence"));

        // Without interruption, both actual filesystem failures retain
        // their existing classification.
        assert!(matches!(
            verify_checked_seal(&directory, &digest, &|| Ok(()), &mut test_manifest_memory()),
            Err(GraphDbError::Corrupt { .. })
        ));
        assert!(matches!(
            verify_checked_seal(&missing, &digest, &|| Ok(()), &mut test_manifest_memory()),
            Err(GraphDbError::Unavailable { .. })
        ));

        for (path, interruption) in [
            (&directory, GraphDbError::Cancelled),
            (&missing, GraphDbError::DeadlineExceeded),
        ] {
            let probes = Cell::new(0_usize);
            let result = verify_checked_seal(
                path,
                &digest,
                &|| {
                    probes.set(probes.get() + 1);
                    Err(interruption.clone())
                },
                &mut test_manifest_memory(),
            );
            assert_eq!(result.unwrap_err(), interruption);
            assert_eq!(
                probes.get(),
                1,
                "interruption must follow an actual pathname metadata operation"
            );
        }
    }

    #[test]
    fn sealed_generation_read_rejects_same_length_mutation() {
        let temp = TempDir::new().unwrap();
        let bytes = vec![b'a'; SEAL_READ_CHECK_BYTES * 2];
        let digest = hex::encode(Sha256::digest(&bytes));
        let path = temp.path().join(format!("generation-{digest}.json"));
        std::fs::write(&path, bytes).unwrap();
        let checks = AtomicUsize::new(0);

        let error = verify_checked_seal(
            &path,
            &digest,
            &|| {
                // Change the second chunk after the first real checked read,
                // not during the new metadata/open admission probes.
                if checks.fetch_add(1, Ordering::SeqCst) == 4 {
                    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                    file.seek(SeekFrom::Start(SEAL_READ_CHECK_BYTES as u64))
                        .unwrap();
                    file.write_all(b"z").unwrap();
                    file.sync_all().unwrap();
                }
                Ok(())
            },
            &mut test_manifest_memory(),
        )
        .unwrap_err();

        assert!(matches!(error, GraphDbError::Corrupt { .. }));
    }

    #[test]
    fn sealed_generation_read_preserves_deadline_error() {
        let temp = TempDir::new().unwrap();
        let bytes = vec![b'a'; SEAL_READ_CHECK_BYTES * 3];
        let digest = hex::encode(Sha256::digest(&bytes));
        let path = temp.path().join(format!("generation-{digest}.json"));
        std::fs::write(&path, bytes).unwrap();
        let checks = AtomicUsize::new(0);

        assert_eq!(
            verify_checked_seal(
                &path,
                &digest,
                &|| {
                    // Expire after the first completed nonempty file read.
                    if checks.fetch_add(1, Ordering::SeqCst) >= 4 {
                        Err(GraphDbError::DeadlineExceeded)
                    } else {
                        Ok(())
                    }
                },
                &mut test_manifest_memory()
            )
            .unwrap_err(),
            GraphDbError::DeadlineExceeded
        );
    }

    #[test]
    fn sealed_manifest_metadata_denial_preserves_late_interruption() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("not-a-sealed-file");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("nonempty-evidence"), b"real directory contents").unwrap();
        let digest = hex::encode(Sha256::digest(b"real directory contents"));

        // A directory cannot impersonate a regular sealed generation.
        assert!(
            read_verified_seal_manifest(&path, &digest, &|| Ok(()), &mut test_manifest_memory())
                .is_err()
        );
        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            let probes = Cell::new(0_usize);
            let result = read_verified_seal_manifest(
                &path,
                &digest,
                &|| {
                    let next = probes.get() + 1;
                    probes.set(next);
                    if next == 2 {
                        Err(interruption.clone())
                    } else {
                        Ok(())
                    }
                },
                &mut test_manifest_memory(),
            );
            assert_eq!(result.unwrap_err(), interruption);
            assert_eq!(
                probes.get(),
                2,
                "a real metadata probe must precede typed interruption"
            );
        }
    }

    #[test]
    fn sealed_manifest_read_preserves_late_interruption_over_corrupt_bytes() {
        let temp = TempDir::new().unwrap();
        let bytes = b"nonempty canonical seal manifest payload";
        let valid_digest = hex::encode(Sha256::digest(bytes));
        let wrong_digest = hex::encode(Sha256::digest(b"a different seal payload"));
        let path = temp.path().join("generation.json");
        std::fs::write(&path, bytes).unwrap();

        // The real file read succeeds with its exact digest, while identical
        // I/O with a wrong manifest identity is still rejected as corruption.
        assert_eq!(
            read_verified_seal_manifest(
                &path,
                &valid_digest,
                &|| Ok(()),
                &mut test_manifest_memory()
            )
            .unwrap(),
            bytes
        );
        assert!(matches!(
            read_verified_seal_manifest(
                &path,
                &wrong_digest,
                &|| Ok(()),
                &mut test_manifest_memory()
            ),
            Err(GraphDbError::Corrupt { .. })
        ));

        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            let probes = AtomicUsize::new(0);
            let error = read_verified_seal_manifest(
                &path,
                &wrong_digest,
                &|| {
                    if probes.fetch_add(1, Ordering::SeqCst) == 4 {
                        Err(interruption.clone())
                    } else {
                        Ok(())
                    }
                },
                &mut test_manifest_memory(),
            )
            .unwrap_err();
            assert_eq!(error, interruption);
            assert_eq!(
                probes.load(Ordering::SeqCst),
                5,
                "a nonzero real manifest read must finish before interruption is reported",
            );
        }
    }

    #[test]
    fn retained_seal_fallback_preserves_late_interruption_after_real_reads() {
        let temp = TempDir::new().unwrap();
        let canonical = temp.path().join("canonical-seal");
        let pool = temp.path().join("retained-seal");
        let expected = b"nonempty retained seal bytes";
        let digest = hex::encode(Sha256::digest(expected));
        std::fs::write(&canonical, b"corrupt canonical bytes").unwrap();
        std::fs::write(&pool, expected).unwrap();

        // Prove that fallback is otherwise usable: the canonical mismatch
        // alone must not prevent an intact retained copy from being read.
        let mut memory = test_manifest_memory();
        let recovered = super::with_verified_seal_from_roots(
            &canonical,
            &pool,
            &digest,
            &|| Ok(()),
            |path, digest, check| read_verified_seal_manifest(path, digest, check, &mut memory),
        )
        .unwrap();
        assert_eq!(recovered.as_slice(), &expected[..]);

        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            let probes = AtomicUsize::new(0);
            let error = super::with_verified_seal_from_roots(
                &canonical,
                &pool,
                &digest,
                &|| {
                    if probes.fetch_add(1, Ordering::SeqCst) >= 9 {
                        Err(interruption.clone())
                    } else {
                        Ok(())
                    }
                },
                |path, digest, check| {
                    read_verified_seal_manifest(path, digest, check, &mut test_manifest_memory())
                },
            )
            .unwrap_err();
            assert_eq!(error, interruption);
            assert_eq!(
                probes.load(Ordering::SeqCst),
                10,
                "the retained copy must complete its real read before the late interruption",
            );
        }
    }

    #[test]
    fn sealed_source_verification_rejects_corrupt_bytes_and_types_missing_as_unavailable() {
        let temp = TempDir::new().unwrap();
        let generations_root = temp.path().join("generations");
        let replay_root = temp.path().join("replay");
        std::fs::create_dir_all(&generations_root).unwrap();
        std::fs::create_dir_all(&replay_root).unwrap();
        let bytes = vec![b'a'; SEAL_READ_CHECK_BYTES + 17];
        let digest = hex::encode(Sha256::digest(&bytes));
        let sealed_state_digest =
            SealedGraphStateDigest::try_from(format!("sha256:{digest}")).unwrap();
        let seal_file = format!("generation-{digest}.json");
        let verify = |check: &dyn Fn() -> Result<(), GraphDbError>| {
            verify_sealed_generation_source_from_roots(
                &generations_root,
                &replay_root,
                &test_resident_memory(),
                &sealed_state_digest,
                &VerifiedSealedSegmentsV1::default(),
                check,
            )
        };

        // Absent from both roots is the typed missing state, not corruption.
        assert!(matches!(
            verify(&|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));

        // Same-length corrupt bytes under the digest-named file must reject,
        // from the canonical root and from a pool-only survivor alike.
        let mut corrupt = bytes.clone();
        corrupt[SEAL_READ_CHECK_BYTES] ^= 1;
        std::fs::write(generations_root.join(&seal_file), &corrupt).unwrap();
        assert!(matches!(
            verify(&|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));
        std::fs::remove_file(generations_root.join(&seal_file)).unwrap();
        std::fs::write(replay_root.join(&seal_file), &corrupt).unwrap();
        assert!(matches!(
            verify(&|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));

        // The intact payload verifies from either root, proving the
        // rejections above are digest-driven rather than fixture artifacts.
        std::fs::write(replay_root.join(&seal_file), &bytes).unwrap();
        verify(&|| Ok(())).unwrap();
        std::fs::remove_file(replay_root.join(&seal_file)).unwrap();
        std::fs::write(generations_root.join(&seal_file), &bytes).unwrap();
        verify(&|| Ok(())).unwrap();
    }

    fn two_second_check() -> impl Fn() -> Result<(), GraphDbError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        move || {
            if std::time::Instant::now() >= deadline {
                Err(GraphDbError::DeadlineExceeded)
            } else {
                Ok(())
            }
        }
    }

    /// A graph publisher holds the project's replay-pool lock while it reads
    /// seals. Seal reads take neither that lock nor the canonical store lock,
    /// so they answer instead of polling a lock their own thread holds until
    /// the deadline.
    #[test]
    fn sealed_source_verification_answers_while_the_store_and_pool_locks_are_held() {
        let temp = TempDir::new().unwrap();
        let store_root = temp.path().join("store");
        let generations_root = store_root.join("code-generations-v1");
        let replay_root = temp.path().join("replay");
        std::fs::create_dir_all(&generations_root).unwrap();
        tracedecay_private_fs::create_private_directory(&replay_root).unwrap();
        let digest = "772b0ae89b16929b615e26a584f5f492ba6a29d94903f45490df8de3a7d06d36";
        let sealed_state_digest =
            SealedGraphStateDigest::try_from(format!("sha256:{digest}")).unwrap();
        let pool_seal = replay_root.join(format!("generation-{digest}.json"));
        let _store_lock = acquire_code_generation_store_lock(&store_root).unwrap();
        let _pool_lock = acquire_code_generation_store_lock(&replay_root).unwrap();
        let verify = || {
            verify_sealed_generation_source_from_roots(
                &generations_root,
                &replay_root,
                &test_resident_memory(),
                &sealed_state_digest,
                &VerifiedSealedSegmentsV1::default(),
                &two_second_check(),
            )
        };

        std::fs::write(
            &pool_seal,
            b"pool-only seal read under a held replay-pool lock\n",
        )
        .unwrap();
        assert_eq!(verify(), Ok(()));
        std::fs::write(
            &pool_seal,
            b"pool-only seal read under a held replay-pool lock!",
        )
        .unwrap();
        assert!(matches!(verify(), Err(GraphDbError::Corrupt { .. })));
        std::fs::remove_file(&pool_seal).unwrap();
        assert!(matches!(verify(), Err(GraphDbError::Unavailable { .. })));
    }

    #[test]
    fn hydration_answers_while_the_store_and_pool_locks_are_held() {
        let fixture = partitioned_seal_fixture("held-locks");
        let replay_root = fixture.pool_manifest.parent().unwrap().to_path_buf();
        let generations_root = fixture.scope_root.join("code-generations-v1");
        let shard = StoreShardIdV1::project(
            BrainId::new("brain.held-locks").unwrap(),
            UserProfileId::new("profile.held-locks").unwrap(),
            fixture.project.clone(),
        );
        let provider = Arc::new(DaemonCodeGraphManifestProviderV1::default());
        let _route = provider
            .bind(
                shard.clone(),
                fixture.project.clone(),
                fixture.repository.clone(),
                generations_root.clone(),
                replay_root.clone(),
                Arc::clone(&fixture.resident_memory),
            )
            .unwrap();
        let owner = GraphProjectionIdentityV1 {
            shard_id: shard,
            namespace: GraphNamespaceV1::new("namespace.held-locks").unwrap(),
            projection: GraphProjectionIdV1::new("code-generation").unwrap(),
        };
        let source = SealedCodeGenerationReplay {
            repository: fixture.repository.clone(),
            generation: fixture.generation.clone(),
            sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                "sha256:{}",
                fixture.digest
            ))
            .unwrap(),
            projector_revision: GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .unwrap(),
        };
        let _store_lock = acquire_code_generation_store_lock(&fixture.scope_root).unwrap();
        let _pool_lock = acquire_code_generation_store_lock(&replay_root).unwrap();

        let pooled = hydrate(&provider, &owner, &source, &two_second_check()).unwrap();
        assert_eq!(pooled.row_counts(), (10, 9));
        let canonical = generations_root.join(fixture.pool_manifest.file_name().unwrap());
        std::fs::rename(&fixture.pool_manifest, &canonical).unwrap();
        let canonical_only = hydrate(&provider, &owner, &source, &two_second_check()).unwrap();
        assert_eq!(canonical_only.row_counts(), (10, 9));
        std::fs::remove_file(&canonical).unwrap();
        assert!(matches!(
            hydrate(&provider, &owner, &source, &two_second_check()),
            Err(GraphDbError::Unavailable { .. })
        ));
    }

    /// The graph reconciler stages a pool seal for unlink by renaming it out
    /// of the content-addressed namespace. A verification that was reading it
    /// then reports the seal unavailable, not corrupt.
    #[test]
    fn a_pool_seal_staged_for_unlink_mid_read_is_superseded() {
        let temp = TempDir::new().unwrap();
        let generations_root = temp.path().join("store/code-generations-v1");
        let replay_root = temp.path().join("replay");
        std::fs::create_dir_all(&generations_root).unwrap();
        std::fs::create_dir_all(&replay_root).unwrap();
        let bytes = vec![b'a'; SEAL_READ_CHECK_BYTES * 4];
        let digest = hex::encode(Sha256::digest(&bytes));
        let sealed_state_digest =
            SealedGraphStateDigest::try_from(format!("sha256:{digest}")).unwrap();
        let pool_seal = replay_root.join(format!("generation-{digest}.json"));
        let staged = replay_root.join(format!(".generation-{digest}.unlink-test"));
        std::fs::write(&pool_seal, &bytes).unwrap();
        let checks = AtomicUsize::new(0);
        let verify = |retire_mid_read: bool| {
            checks.store(0, Ordering::SeqCst);
            verify_sealed_generation_source_from_roots(
                &generations_root,
                &replay_root,
                &test_resident_memory(),
                &sealed_state_digest,
                &VerifiedSealedSegmentsV1::default(),
                &|| {
                    if retire_mid_read && checks.fetch_add(1, Ordering::SeqCst) == 5 {
                        std::fs::rename(&pool_seal, &staged).unwrap();
                    }
                    Ok(())
                },
            )
        };

        assert_eq!(verify(false), Ok(()));
        assert_eq!(verify(true), Err(GraphDbError::SealSuperseded));
    }

    /// The segment sweep collects a seal's segments only once no root names
    /// the seal. Missing segments of a live seal are corruption; missing
    /// segments of a seal that retired during verification are its
    /// retirement.
    #[test]
    fn swept_segments_are_corruption_for_a_live_seal_and_retirement_for_a_retired_one() {
        let fixture = partitioned_seal_fixture("swept-segments");
        let manifest_checks = AtomicUsize::new(0);
        verify_checked_seal(
            &fixture.pool_manifest,
            &fixture.digest,
            &|| {
                manifest_checks.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            &mut test_manifest_memory(),
        )
        .unwrap();
        let manifest_checks = manifest_checks.load(Ordering::SeqCst);
        let staged = fixture
            .pool_manifest
            .with_file_name(format!(".generation-{}.unlink-test", fixture.digest));
        let sweep = || {
            for entry in std::fs::read_dir(&fixture.segments_root).unwrap() {
                let path = entry.unwrap().path();
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("segment-"))
                {
                    std::fs::remove_file(path).unwrap();
                }
            }
        };
        let verify_sweeping_segments = |retire_seal: bool| {
            let checks = AtomicUsize::new(0);
            verify_checked_seal_bundle_with_evidence_barrier(
                &fixture.pool_manifest,
                &fixture.segments_root,
                &fixture.digest,
                &VerifiedSealedSegmentsV1::default(),
                &|| {
                    if checks.fetch_add(1, Ordering::SeqCst) == manifest_checks {
                        if retire_seal {
                            std::fs::rename(&fixture.pool_manifest, &staged).unwrap();
                        }
                        sweep();
                    }
                    Ok(())
                },
                &test_resident_memory(),
                || {},
            )
            .unwrap_err()
        };

        let live = verify_sweeping_segments(false);
        assert!(matches!(live, GraphDbError::Corrupt { .. }), "{live:?}");
        assert_eq!(verify_sweeping_segments(true), GraphDbError::SealSuperseded);
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture command failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    struct PartitionedSealFixture {
        _temporary: TempDir,
        pool_manifest: std::path::PathBuf,
        scope_root: std::path::PathBuf,
        segments_root: std::path::PathBuf,
        digest: String,
        project: ProjectId,
        repository: RepositoryId,
        generation: CodeGenerationId,
        resident_memory: Arc<ProcessResidentMemoryV1>,
    }

    const PARTITIONED_FIXTURE_FILES: [&str; 3] = ["lib", "alpha", "beta"];

    /// Windows opens the first existing pathname for metadata, payload and a
    /// stable file-ID witness. Preceding missing routes are counted separately.
    fn payload_open_count(files: usize) -> usize {
        files * if cfg!(windows) { 3 } else { 1 }
    }

    fn pathname_probe_open_count(probes: usize) -> usize {
        if cfg!(windows) { probes } else { 0 }
    }

    fn packed_extent(
        manifest: &[u8],
        segments_root: &Path,
        digest: &ManifestDigest,
        size_bytes: u64,
    ) -> (std::path::PathBuf, usize, usize) {
        let layout = SealedGenerationPackLayoutV1::open(manifest).unwrap();
        let request = layout
            .resolve(SealedGenerationSegmentReadV1::Whole { digest, size_bytes })
            .unwrap();
        let (digest, offset, length) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => (digest, 0, size_bytes),
            SealedGenerationSegmentReadV1::Range {
                digest,
                offset,
                length,
                ..
            } => (digest, offset, length),
        };
        (
            super::segment_path(segments_root, digest).unwrap(),
            usize::try_from(offset).unwrap(),
            usize::try_from(length).unwrap(),
        )
    }

    #[test]
    fn sealed_graph_build_preserves_rows_with_fixed_pack_opens_across_cold_passes() {
        let fixture = partitioned_seal_fixture("graph-page-opens");
        let seal = open_seal_from_roots(
            &fixture.scope_root.join("code-generations-v1"),
            fixture.pool_manifest.parent().unwrap(),
            &test_resident_memory(),
            &SealedGraphStateDigest::try_from(format!("sha256:{}", fixture.digest)).unwrap(),
            &fixture.generation,
            &|| Ok(()),
            "test.graph-pages",
        )
        .unwrap();
        let projection = tracedecay_code_index::graph_projection::code_graph_projection_identity(
            GraphNamespace::new("namespace.page-opens").unwrap(),
        )
        .unwrap();
        let revision = GraphProjectorRevision::try_from(
            tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
        )
        .unwrap();
        let manifest_bytes = std::fs::read(&fixture.pool_manifest).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        let pages = manifest["generation"]["code_graph_pages"]
            .as_array()
            .unwrap();
        let distinct = pages
            .iter()
            .map(|page| page["page_digest"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert_eq!(pages.len(), PARTITIONED_FIXTURE_FILES.len());
        assert!(
            distinct > 2,
            "logical graph pages must exceed the physical roles"
        );
        let build =
            |read_pack: &mut tracedecay_code_index::production::SealedGenerationSegmentReaderV1<
                '_,
            >,
             name| {
                let mut read = |request: SealedGenerationSegmentReadV1<'_>,
                                buffer: &mut Vec<u8>| {
                    seal.layout.read(request, buffer, &mut *read_pack)
                };
                build_sealed_code_graph_rows(
                    projection.clone(),
                    &seal.admitted.source,
                    &mut read,
                    &revision,
                    GraphGenerationRowSpill::create(
                        fixture._temporary.path().join(name),
                        projection.clone(),
                    )
                    .unwrap(),
                    &mut |_| Ok(()),
                    &|| Ok(()),
                )
            };
        // This independent file reader supplies the same authenticated logical
        // extents without using the operation-local handle implementation.
        let reference = build(
            &mut |request, buffer| {
                let (digest, _, offset, length) = super::partitioned_segment_request(request)?;
                let digest = ManifestDigest::new(digest).unwrap();
                let bytes =
                    std::fs::read(super::segment_path(&fixture.segments_root, &digest).unwrap())
                        .unwrap();
                *buffer = bytes[offset as usize..(offset + length) as usize].to_vec();
                Ok(())
            },
            "reference",
        )
        .unwrap();
        let expected_rows =
            serde_json::to_vec(&reference.materialize(&|| Ok(())).unwrap()).unwrap();
        for name in ["first-cold-pass", "second-cold-pass"] {
            SEGMENT_OPENS.with(|opens| opens.set(0));
            let rows = with_verified_test_segments(
                &seal.admitted.sealed_state_digest,
                &seal.routes,
                &|| Ok(()),
                |read| build(read, name),
            )
            .unwrap();
            assert_eq!(rows.row_counts(), (10, 9));
            assert_eq!(
                rows.expected_recovered_digest(),
                reference.expected_recovered_digest()
            );
            assert_eq!(
                serde_json::to_vec(&rows.materialize(&|| Ok(())).unwrap()).unwrap(),
                expected_rows,
            );
            assert_eq!(
                SEGMENT_OPENS.with(|opens| opens.get()),
                payload_open_count(1),
                "every logical graph page must use the one content-pack file, including identity opens"
            );
        }
        let page_digest = ManifestDigest::new(pages[0]["page_digest"].as_str().unwrap()).unwrap();
        let (pack_path, offset, length) = packed_extent(
            &manifest_bytes,
            &fixture.segments_root,
            &page_digest,
            pages[0]["size_bytes"].as_u64().unwrap(),
        );
        assert!(length > 0);
        let mut corrupt = std::fs::read(&pack_path).unwrap();
        corrupt[offset] ^= 1;
        let mut permissions = std::fs::metadata(&pack_path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&pack_path, permissions).unwrap();
        std::fs::write(&pack_path, corrupt).unwrap();
        assert!(matches!(
            with_verified_test_segments(
                &seal.admitted.sealed_state_digest,
                &seal.routes,
                &|| Ok(()),
                |read| build(read, "tampered"),
            ),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

    #[test]
    fn graph_segment_pass_pins_bytes_and_preserves_interruption_precedence() {
        let temporary = TempDir::new().unwrap();
        let bytes = b"authenticated segment bytes";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let size_bytes = bytes.len() as u64;
        let path = temporary.path().join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        ));
        std::fs::write(&path, bytes).unwrap();
        let seal_path = temporary.path().join("seal");
        std::fs::write(&seal_path, b"live").unwrap();
        let routes = SealRoutes {
            seal_paths: vec![seal_path.clone()],
            segment_roots: vec![temporary.path().to_path_buf()],
        };
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes,
        };
        let read_after_unlink = |check: &dyn Fn() -> Result<(), GraphDbError>| {
            with_verified_test_segments(&digest, &routes, check, |read| {
                let mut first = Vec::new();
                read(request, &mut first)?;
                std::fs::remove_file(&path).unwrap();
                std::fs::write(&path, vec![b'x'; bytes.len()]).unwrap();
                let mut second = Vec::new();
                read(request, &mut second)?;
                assert_eq!(first, bytes);
                assert_eq!(second, bytes);
                Ok(())
            })
        };
        read_after_unlink(&|| Ok(())).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            with_verified_test_segments(&digest, &routes, &|| Ok(()), |read| {
                read(request, &mut Vec::new())?;
                Ok(())
            }),
            Err(GraphDbError::Corrupt { .. })
        ));
        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            std::fs::write(&path, bytes).unwrap();
            let checks = AtomicUsize::new(0);
            assert_eq!(
                read_after_unlink(&|| {
                    if checks.fetch_add(1, Ordering::SeqCst) < 2 {
                        Ok(())
                    } else {
                        Err(interruption.clone())
                    }
                })
                .unwrap_err(),
                interruption
            );
        }
        assert!(matches!(
            with_verified_test_segments(&digest, &routes, &|| Ok(()), |read| {
                read(request, &mut Vec::new())?;
                read(
                    SealedGenerationSegmentReadV1::Whole {
                        digest: &digest,
                        size_bytes: size_bytes + 1,
                    },
                    &mut Vec::new(),
                )?;
                Ok(())
            }),
            Err(GraphDbError::Corrupt { .. })
        ));
        std::fs::write(&path, bytes).unwrap();
        let changed = with_verified_test_segments(&digest, &routes, &|| Ok(()), |read| {
            read(request, &mut Vec::new())?;
            std::fs::write(&path, vec![b'x'; bytes.len()]).unwrap();
            let mut changed = Vec::new();
            read(request, &mut changed)?;
            Ok(changed)
        })
        .unwrap();
        let observed = ManifestDigest::from_sha256_bytes(&Sha256::digest(&changed)).unwrap();
        assert_ne!(
            observed, digest,
            "a pinned handle must not cache verified content"
        );
        assert_eq!(changed, vec![b'x'; bytes.len()]);
        for length in [size_bytes - 1, size_bytes + 1] {
            std::fs::write(&path, bytes).unwrap();
            assert!(matches!(
                with_verified_test_segments(&digest, &routes, &|| Ok(()), |read| {
                    read(request, &mut Vec::new())?;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .unwrap()
                        .set_len(length)
                        .unwrap();
                    read(request, &mut Vec::new())?;
                    Ok(())
                }),
                Err(GraphDbError::Corrupt { .. })
            ));
        }
        std::fs::remove_file(&seal_path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            with_verified_test_segments(&digest, &routes, &|| Ok(()), |read| {
                read(request, &mut Vec::new())?;
                Ok(())
            })
            .unwrap_err(),
            GraphDbError::SealSuperseded
        );
    }

    #[test]
    fn graph_segment_pass_preserves_first_existing_route_identity() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        let bytes = b"sealed graph page from a retained route";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let filename = format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        );
        std::fs::write(second.path().join(&filename), bytes).unwrap();
        let roots = vec![first.path().to_path_buf(), second.path().to_path_buf()];
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        };
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let mut pass = super::GraphSegmentReadPassV1::new(&roots);
        let mut decoded = Vec::new();
        pass.read(request, &mut decoded).unwrap();
        assert_eq!(decoded, bytes);
        let fallback_opens = payload_open_count(1) + pathname_probe_open_count(1);
        assert_eq!(pass.open_attempts, fallback_opens);
        assert_eq!(SEGMENT_OPENS.with(|opens| opens.get()), fallback_opens);
        assert!(pass.open_wall_time > std::time::Duration::ZERO);

        // A non-file at the first existing route is corrupt, not a reason
        // to silently fall through to a later apparently valid segment.
        std::fs::create_dir(first.path().join(&filename)).unwrap();
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let mut fresh = super::GraphSegmentReadPassV1::new(&roots);
        assert!(matches!(
            fresh.read(request, &mut Vec::new()),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            pathname_probe_open_count(1)
        );
        assert_eq!(
            fresh.open_attempts,
            pathname_probe_open_count(1),
            "invalid pathname is refused after its metadata probe"
        );

        // A valid pathname with a mismatched manifest extent must also stop
        // before the payload open, while retaining its metadata attempt.
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let roots = vec![second.path().to_path_buf()];
        let mut wrong_extent = super::GraphSegmentReadPassV1::new(&roots);
        let mismatch = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64 + 1,
        };
        assert!(matches!(
            wrong_extent.read(mismatch, &mut Vec::new()),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert_eq!(wrong_extent.open_attempts, pathname_probe_open_count(1));
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            pathname_probe_open_count(1)
        );
    }

    #[test]
    fn graph_pack_missing_routes_preserve_a_healthy_role_and_count_recovery() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        let roots = [first.path().to_path_buf(), second.path().to_path_buf()];
        let healthy_bytes = b"retained healthy graph pack";
        let missing_bytes = b"graph pack restored to a later route";
        let healthy_digest =
            ManifestDigest::from_sha256_bytes(&Sha256::digest(healthy_bytes)).unwrap();
        let missing_digest =
            ManifestDigest::from_sha256_bytes(&Sha256::digest(missing_bytes)).unwrap();
        std::fs::write(
            super::segment_path(first.path(), &healthy_digest).unwrap(),
            healthy_bytes,
        )
        .unwrap();
        let healthy = SealedGenerationSegmentReadV1::Whole {
            digest: &healthy_digest,
            size_bytes: healthy_bytes.len() as u64,
        };
        let missing = SealedGenerationSegmentReadV1::Whole {
            digest: &missing_digest,
            size_bytes: missing_bytes.len() as u64,
        };
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let mut pass = super::GraphSegmentReadPassV1::new(&roots);
        let mut buffer = Vec::new();
        pass.read(healthy, &mut buffer).unwrap();
        assert_eq!(buffer, healthy_bytes);
        assert_eq!(pass.open_attempts, payload_open_count(1));

        assert!(matches!(
            pass.read(missing, &mut buffer),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(buffer.is_empty());
        assert_eq!(pass.pinned.len(), 1);
        assert_eq!(pass.pinned[0].digest, healthy_digest.as_str());
        let after_missing = payload_open_count(1) + pathname_probe_open_count(2);
        assert_eq!(pass.open_attempts, after_missing);
        assert_eq!(SEGMENT_OPENS.with(|opens| opens.get()), after_missing);
        pass.read(healthy, &mut buffer).unwrap();
        assert_eq!(buffer, healthy_bytes);
        assert_eq!(pass.open_attempts, after_missing);

        std::fs::write(
            super::segment_path(second.path(), &missing_digest).unwrap(),
            missing_bytes,
        )
        .unwrap();
        pass.read(missing, &mut buffer).unwrap();
        assert_eq!(buffer, missing_bytes);
        let after_recovery = after_missing + payload_open_count(1) + pathname_probe_open_count(1);
        assert_eq!(pass.open_attempts, after_recovery);
        assert_eq!(pass.pinned.len(), 2);
        pass.read(healthy, &mut buffer).unwrap();
        assert_eq!(buffer, healthy_bytes);
        pass.read(missing, &mut buffer).unwrap();
        assert_eq!(buffer, missing_bytes);
        assert_eq!(pass.open_attempts, after_recovery);
        assert_eq!(SEGMENT_OPENS.with(|opens| opens.get()), after_recovery);
    }

    #[test]
    fn sealed_proof_manifest_reads_preserve_late_interruption_after_real_io() {
        let fixture = partitioned_seal_fixture("proof-manifest-late-deadline");
        let expected = std::fs::read(&fixture.pool_manifest).unwrap();
        assert!(!expected.is_empty());
        let mut file = std::fs::File::open(&fixture.pool_manifest).unwrap();
        assert_eq!(
            super::read_checked_seal_manifest_for_proof(
                &mut file,
                expected.len() as u64,
                &|| Ok(()),
                &mut test_manifest_memory()
            )
            .unwrap()
            .unwrap(),
            expected
        );

        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            for late_probe in [0, 1] {
                let mut file = std::fs::File::open(&fixture.pool_manifest).unwrap();
                let probes = AtomicUsize::new(0);
                let result = super::read_checked_seal_manifest_for_proof(
                    &mut file,
                    expected.len() as u64,
                    &|| {
                        if probes.fetch_add(1, Ordering::SeqCst) == late_probe {
                            Err(interruption.clone())
                        } else {
                            Ok(())
                        }
                    },
                    &mut test_manifest_memory(),
                );
                assert_eq!(result.unwrap_err(), interruption);
                assert_eq!(probes.load(Ordering::SeqCst), late_probe + 1);
                let actual_bytes = file.stream_position().unwrap();
                assert!(
                    actual_bytes > 0,
                    "interruption must follow actual manifest I/O"
                );
                if late_probe == 0 {
                    assert!(actual_bytes <= SEAL_READ_CHECK_BYTES as u64);
                } else {
                    assert_eq!(actual_bytes, expected.len() as u64);
                }
            }
        }
    }

    /// Bound each individual OS read while still obtaining bytes from a real file.
    /// The test proves that short successful reads cannot skip seal proof.
    struct ThreeByteFileReader {
        file: std::fs::File,
        nonempty_reads: usize,
        bytes_read: usize,
    }

    impl Read for ThreeByteFileReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let limit = buffer.len().min(3);
            let read = self.file.read(&mut buffer[..limit])?;
            if read != 0 {
                self.nonempty_reads += 1;
            }
            self.bytes_read += read;
            Ok(read)
        }
    }

    impl Seek for ThreeByteFileReader {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.file.seek(position)
        }
    }

    #[test]
    fn sealed_proof_manifest_reads_all_short_chunks_from_real_file() {
        let fixture = partitioned_seal_fixture("proof-manifest-short-reads");
        let expected = std::fs::read(&fixture.pool_manifest).unwrap();
        assert!(expected.len() > 3, "the manifest must need multiple reads");
        let mut file = ThreeByteFileReader {
            file: std::fs::File::open(&fixture.pool_manifest).unwrap(),
            nonempty_reads: 0,
            bytes_read: 0,
        };
        let mut memory = test_manifest_memory();
        let proven = super::read_checked_seal_manifest_for_proof(
            &mut file,
            expected.len() as u64,
            &|| Ok(()),
            &mut memory,
        )
        .unwrap()
        .expect("a short read must not skip the sealed component proof");
        assert_eq!(
            proven, expected,
            "the entire manifest must remain byte-exact"
        );
        assert!(
            file.nonempty_reads > 1,
            "the fixture must use multiple actual file reads"
        );
        assert_eq!(file.file.stream_position().unwrap(), expected.len() as u64);
        assert_eq!(
            file.bytes_read,
            expected.len(),
            "proof must consume each authenticated byte once without rereading the prefix"
        );
    }

    #[test]
    fn sealed_proof_authenticates_materialized_bytes_and_unretained_tail() {
        let temporary = TempDir::new().unwrap();
        for partitioned in [true, false] {
            let mut bytes = if partitioned {
                format!(
                    "{{\"format_revision\":{},\"payload\":\"",
                    tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1
                )
                .into_bytes()
            } else {
                b"non-partitioned sealed payload\n".to_vec()
            };
            bytes.resize(SEAL_READ_CHECK_BYTES * 2 + 17, b'x');
            if partitioned {
                bytes.extend_from_slice(b"\"}");
            }
            let path = temporary.path().join(format!("seal-{partitioned}.json"));
            std::fs::write(&path, &bytes).unwrap();
            let digest = hex::encode(Sha256::digest(&bytes));
            let wrong_digest = hex::encode(Sha256::digest(b"different sealed bytes"));
            let mut memory = test_manifest_memory();
            let (mut file, manifest) =
                verify_checked_seal(&path, &digest, &|| Ok(()), &mut memory).unwrap();
            assert_eq!(manifest.as_deref(), partitioned.then_some(bytes.as_slice()));
            assert_eq!(file.stream_position().unwrap(), bytes.len() as u64);
            drop(file);
            assert!(matches!(
                verify_checked_seal(
                    &path,
                    &wrong_digest,
                    &|| Ok(()),
                    &mut test_manifest_memory()
                ),
                Err(GraphDbError::Corrupt { .. })
            ));

            // The tail beyond the bounded revision probe is authenticated even
            // when the caller does not retain the manifest for component proof.
            let mut changed = bytes.clone();
            changed[SEAL_READ_CHECK_BYTES + 1] ^= 1;
            std::fs::write(&path, &changed).unwrap();
            assert!(matches!(
                verify_checked_seal(&path, &digest, &|| Ok(()), &mut test_manifest_memory()),
                Err(GraphDbError::Corrupt { .. })
            ));
            std::fs::write(&path, &bytes).unwrap();
            let mut recovered_memory = test_manifest_memory();
            let (_, recovered) =
                verify_checked_seal(&path, &digest, &|| Ok(()), &mut recovered_memory).unwrap();
            assert_eq!(
                recovered.as_deref(),
                partitioned.then_some(bytes.as_slice())
            );
        }
    }

    #[test]
    fn graph_segment_interruption_clears_last_verified_page_without_reopen() {
        let temporary = TempDir::new().unwrap();
        let bytes = b"real authenticated segment data";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let path = temporary.path().join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        ));
        std::fs::write(&path, bytes).unwrap();
        let routes = SealRoutes {
            seal_paths: vec![path],
            segment_roots: vec![temporary.path().to_path_buf()],
        };
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        };
        let checks = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let result = with_verified_test_segments(
            &digest,
            &routes,
            &|| {
                checks.fetch_add(1, Ordering::SeqCst);
                if cancelled.load(Ordering::SeqCst) {
                    Err(GraphDbError::Cancelled)
                } else {
                    Ok(())
                }
            },
            |read| {
                let mut buffer = Vec::new();
                read(request, &mut buffer)?;
                assert_eq!(buffer, bytes);
                cancelled.store(true, Ordering::SeqCst);
                assert!(matches!(
                    read(request, &mut buffer),
                    Err(CodeIndexProductionErrorV1::Contract(_))
                ));
                assert!(
                    buffer.is_empty(),
                    "interrupted graph reads cannot expose prior page bytes"
                );
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err(), GraphDbError::Cancelled);
        assert_eq!(checks.load(Ordering::SeqCst), 3);
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            payload_open_count(1)
        );
    }

    #[test]
    fn graph_segment_pass_rechecks_budget_after_completed_projection() {
        let temporary = TempDir::new().unwrap();
        let bytes = b"real sealed graph page before final projection work";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let path = temporary.path().join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        ));
        std::fs::write(&path, bytes).unwrap();
        let routes = SealRoutes {
            seal_paths: vec![path],
            segment_roots: vec![temporary.path().to_path_buf()],
        };
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        };

        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            let projection_finished = AtomicBool::new(false);
            let checks = AtomicUsize::new(0);
            SEGMENT_OPENS.with(|opens| opens.set(0));
            let result = with_verified_test_segments(
                &digest,
                &routes,
                &|| {
                    checks.fetch_add(1, Ordering::SeqCst);
                    if projection_finished.load(Ordering::SeqCst) {
                        Err(interruption.clone())
                    } else {
                        Ok(())
                    }
                },
                |read| {
                    let mut buffer = Vec::new();
                    read(request, &mut buffer)?;
                    assert_eq!(buffer, bytes, "the builder must read real nonempty bytes");
                    // Model work after the last segment I/O, including a final
                    // graph-row spill that can outlive the publication budget.
                    projection_finished.store(true, Ordering::SeqCst);
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err(), interruption);
            assert_eq!(
                checks.load(Ordering::SeqCst),
                3,
                "pre-read, post-read and post-projection probes must execute"
            );
            assert_eq!(
                SEGMENT_OPENS.with(|opens| opens.get()),
                payload_open_count(1),
                "a completed page read must not be repeated for the last probe"
            );
        }
    }

    #[test]
    fn graph_segment_retirement_probe_preserves_late_interruption() {
        let temporary = TempDir::new().unwrap();
        let bytes = b"authenticated graph page before retirement classification";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let segment_path = temporary.path().join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        ));
        std::fs::write(segment_path, bytes).unwrap();
        let seal_path = temporary.path().join("seal");
        let routes = SealRoutes {
            seal_paths: vec![seal_path.clone()],
            segment_roots: vec![temporary.path().to_path_buf()],
        };
        let run = |check: &dyn Fn() -> Result<(), GraphDbError>| {
            with_verified_test_segments::<()>(&digest, &routes, check, |read| {
                let mut page = Vec::new();
                read(
                    SealedGenerationSegmentReadV1::Whole {
                        digest: &digest,
                        size_bytes: bytes.len() as u64,
                    },
                    &mut page,
                )?;
                assert_eq!(page, bytes, "a real nonempty segment must be read");
                // The builder has authenticated source bytes but rejects a
                // graph row, forcing the final seal-name probe.
                Err(super::SealedCodeGraphRowsError::Source(
                    CodeIndexProductionErrorV1::Contract(
                        "invalid graph row after sealed segment read".to_owned(),
                    ),
                ))
            })
        };

        for retired in [false, true] {
            std::fs::write(&seal_path, b"live seal").unwrap();
            if retired {
                std::fs::remove_file(&seal_path).unwrap();
            }
            SEGMENT_OPENS.with(|opens| opens.set(0));
            let uninterrupted = run(&|| Ok(())).unwrap_err();
            assert!(
                matches!(
                    (&uninterrupted, retired),
                    (GraphDbError::Corrupt { .. }, false) | (GraphDbError::SealSuperseded, true)
                ),
                "unexpected retirement classification: {uninterrupted:?}"
            );
            assert_eq!(
                SEGMENT_OPENS.with(|opens| opens.get()),
                payload_open_count(1)
            );

            for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
                let checks = AtomicUsize::new(0);
                SEGMENT_OPENS.with(|opens| opens.set(0));
                let result = run(&|| {
                    let count = checks.fetch_add(1, Ordering::SeqCst) + 1;
                    if count == 4 {
                        Err(interruption.clone())
                    } else {
                        Ok(())
                    }
                });
                assert_eq!(result.unwrap_err(), interruption);
                assert_eq!(
                    checks.load(Ordering::SeqCst),
                    4,
                    "the final check must follow a real seal-path metadata probe"
                );
                assert_eq!(
                    SEGMENT_OPENS.with(|opens| opens.get()),
                    payload_open_count(1),
                    "the deadline probe must not reopen the authenticated page"
                );
            }
        }
    }

    #[test]
    fn graph_segment_pass_preserves_interruption_after_io() {
        let temporary = TempDir::new().unwrap();
        let bytes = b"authenticated bytes from a completed segment read";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let path = temporary.path().join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        ));
        std::fs::write(&path, bytes).unwrap();
        let routes = SealRoutes {
            seal_paths: vec![path],
            segment_roots: vec![temporary.path().to_path_buf()],
        };
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        };
        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            let checks = AtomicUsize::new(0);
            SEGMENT_OPENS.with(|opens| opens.set(0));
            let result = with_verified_test_segments(
                &digest,
                &routes,
                &|| {
                    checks.fetch_add(1, Ordering::SeqCst);
                    if SEGMENT_OPENS.with(|opens| opens.get()) == 0 {
                        Ok(())
                    } else {
                        Err(interruption.clone())
                    }
                },
                |read| {
                    let mut buffer = b"previous page".to_vec();
                    assert!(matches!(
                        read(request, &mut buffer),
                        Err(CodeIndexProductionErrorV1::Contract(_))
                    ));
                    assert!(buffer.is_empty(), "late segment bytes must not escape");
                    // Even a builder that handled the source error cannot
                    // turn an observed interruption into a successful pass.
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err(), interruption);
            assert_eq!(checks.load(Ordering::SeqCst), 2);
            assert_eq!(
                SEGMENT_OPENS.with(|opens| opens.get()),
                payload_open_count(1)
            );
        }
        SEGMENT_OPENS.with(|opens| opens.set(0));
        with_verified_test_segments(&digest, &routes, &|| Ok(()), |read| {
            let mut buffer = Vec::new();
            read(request, &mut buffer)?;
            assert_eq!(buffer, bytes);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            payload_open_count(1)
        );
    }

    #[test]
    fn graph_segment_failed_read_preserves_post_io_deadline() {
        let temporary = TempDir::new().unwrap();
        let bytes = b"authenticated segment before truncation";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let path = temporary.path().join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        ));
        std::fs::write(&path, bytes).unwrap();
        let routes = SealRoutes {
            seal_paths: vec![path.clone()],
            segment_roots: vec![temporary.path().to_path_buf()],
        };
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        };
        let truncated = AtomicBool::new(false);
        let checks_after_truncation = AtomicUsize::new(0);
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let result = with_verified_test_segments(
            &digest,
            &routes,
            &|| {
                if truncated.load(Ordering::SeqCst)
                    && checks_after_truncation.fetch_add(1, Ordering::SeqCst) > 0
                {
                    Err(GraphDbError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            },
            |read| {
                let mut buffer = Vec::new();
                read(request, &mut buffer)?;
                assert_eq!(buffer, bytes);
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(0)
                    .unwrap();
                truncated.store(true, Ordering::SeqCst);
                read(request, &mut buffer).unwrap_err();
                assert!(buffer.is_empty());
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err(), GraphDbError::DeadlineExceeded);
        assert_eq!(checks_after_truncation.load(Ordering::SeqCst), 2);
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            payload_open_count(1)
        );
    }

    #[test]
    fn graph_segment_short_read_discards_partially_filled_output() {
        let mut reader = std::io::Cursor::new(b"short".to_vec());
        let mut buffer = vec![b'x'; 3];
        assert!(super::read_segment_range(&mut reader, 0, 8, &mut buffer).is_err());
        assert!(
            buffer.is_empty(),
            "failed reads cannot expose partial segment bytes"
        );
        super::read_segment_range(&mut reader, 0, 5, &mut buffer).unwrap();
        assert_eq!(buffer, b"short");
    }

    #[test]
    fn graph_pack_read_clears_refused_and_stale_page_output() {
        let temporary = TempDir::new().unwrap();
        let bytes = b"verified graph evidence pack";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let path = super::segment_path(temporary.path(), &digest).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let roots = [temporary.path().to_path_buf()];
        let mut pass = super::GraphSegmentReadPassV1::new(&roots);
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        };
        let mut buffer = b"old page".to_vec();
        pass.read(request, &mut buffer).unwrap();
        assert_eq!(buffer, bytes);
        assert!(matches!(
            pass.read(
                SealedGenerationSegmentReadV1::Whole {
                    digest: &digest,
                    size_bytes: bytes.len() as u64 + 1,
                },
                &mut buffer,
            ),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(
            buffer.is_empty(),
            "a refused pack must not return prior bytes"
        );
        pass.read(request, &mut buffer).unwrap();
        assert_eq!(buffer, bytes);
        drop(pass);
        std::fs::remove_file(&path).unwrap();
        let mut next = super::GraphSegmentReadPassV1::new(&roots);
        assert!(matches!(
            next.read(request, &mut buffer),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(
            buffer.is_empty(),
            "a new operation cannot use retired pack bytes"
        );
    }

    #[test]
    fn graph_segment_read_failure_clears_the_previous_verified_page() {
        let temporary = TempDir::new().unwrap();
        let bytes = b"authenticated page";
        let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
        let filename = format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        );
        std::fs::write(temporary.path().join(filename), bytes).unwrap();
        let roots = vec![temporary.path().to_path_buf()];
        let mut pass = super::GraphSegmentReadPassV1::new(&roots);
        let request = SealedGenerationSegmentReadV1::Whole {
            digest: &digest,
            size_bytes: bytes.len() as u64,
        };
        let mut buffer = Vec::new();
        pass.read(request, &mut buffer).unwrap();
        assert_eq!(buffer, bytes);
        assert_eq!(pass.open_attempts, payload_open_count(1));

        // Reject an invalid range *before* reaching the retained file handle.
        let invalid = SealedGenerationSegmentReadV1::Range {
            digest: &digest,
            size_bytes: bytes.len() as u64,
            offset: bytes.len() as u64,
            length: 1,
        };
        assert!(matches!(
            pass.read(invalid, &mut buffer),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(
            buffer.is_empty(),
            "a refused page cannot expose prior bytes"
        );
        assert_eq!(pass.open_attempts, payload_open_count(1));

        pass.read(request, &mut buffer).unwrap();
        assert_eq!(buffer, bytes);
        assert!(matches!(
            pass.read(
                SealedGenerationSegmentReadV1::Whole {
                    digest: &digest,
                    size_bytes: bytes.len() as u64 + 1,
                },
                &mut buffer
            ),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(
            buffer.is_empty(),
            "a mismatched manifest cannot expose prior bytes"
        );
        assert_eq!(pass.open_attempts, payload_open_count(1));
    }

    #[test]
    fn graph_segment_pass_reuses_interleaved_sealed_handles() {
        let temporary = TempDir::new().unwrap();
        let first: &[u8] = b"first authenticated graph segment";
        let second: &[u8] = b"second authenticated graph segment";
        let digests = [first, second].map(|bytes| {
            let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap();
            let path = temporary.path().join(format!(
                "segment-{}.json",
                sha256_hex_suffix(digest.as_str()).unwrap()
            ));
            std::fs::write(path, bytes).unwrap();
            digest
        });
        let seal_path = temporary.path().join("seal");
        std::fs::write(&seal_path, b"live").unwrap();
        let routes = SealRoutes {
            seal_paths: vec![seal_path],
            segment_roots: vec![temporary.path().to_path_buf()],
        };
        SEGMENT_OPENS.with(|opens| opens.set(0));
        with_verified_test_segments(&digests[0], &routes, &|| Ok(()), |read| {
            let mut buffer = Vec::new();
            for index in [0, 1, 0, 1, 0, 1] {
                let expected = [first, second][index];
                read(
                    SealedGenerationSegmentReadV1::Whole {
                        digest: &digests[index],
                        size_bytes: expected.len() as u64,
                    },
                    &mut buffer,
                )?;
                assert_eq!(buffer.as_slice(), expected);
                assert_eq!(
                    ManifestDigest::from_sha256_bytes(&Sha256::digest(&buffer)).unwrap(),
                    digests[index]
                );
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            payload_open_count(2),
            "interleaved pages must not re-open previously pinned segments"
        );
    }

    #[test]
    fn graph_pack_pass_refuses_a_third_role_without_displacing_pinned_files() {
        let temporary = TempDir::new().unwrap();
        let roots = [temporary.path().to_path_buf()];
        let packs = (0..3)
            .map(|index| {
                let bytes = format!("sealed graph pack {index}").into_bytes();
                let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes)).unwrap();
                std::fs::write(
                    super::segment_path(temporary.path(), &digest).unwrap(),
                    &bytes,
                )
                .unwrap();
                (digest, bytes)
            })
            .collect::<Vec<_>>();
        let mut pass = super::GraphSegmentReadPassV1::new(&roots);
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let mut buffer = Vec::new();
        for (digest, bytes) in packs.iter().take(2) {
            pass.read(
                SealedGenerationSegmentReadV1::Whole {
                    digest,
                    size_bytes: bytes.len() as u64,
                },
                &mut buffer,
            )
            .unwrap();
            assert_eq!(&buffer, bytes);
        }
        assert_eq!(pass.pinned.len(), 2);
        assert_eq!(pass.open_attempts, payload_open_count(2));
        let original = pass
            .pinned
            .iter()
            .map(|pack| pack.digest.clone())
            .collect::<Vec<_>>();
        let (third_digest, third) = &packs[2];
        assert!(matches!(
            pass.read(
                SealedGenerationSegmentReadV1::Whole {
                    digest: third_digest,
                    size_bytes: third.len() as u64,
                },
                &mut buffer,
            ),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(buffer.is_empty());
        for (digest, bytes) in packs.iter().take(2) {
            for invalid in [
                SealedGenerationSegmentReadV1::Whole {
                    digest,
                    size_bytes: bytes.len() as u64 + 1,
                },
                SealedGenerationSegmentReadV1::Range {
                    digest,
                    size_bytes: bytes.len() as u64,
                    offset: 1,
                    length: bytes.len() as u64,
                },
            ] {
                buffer.extend_from_slice(b"prior page");
                assert!(pass.read(invalid, &mut buffer).is_err());
                assert!(buffer.is_empty());
            }
            pass.read(
                SealedGenerationSegmentReadV1::Whole {
                    digest,
                    size_bytes: bytes.len() as u64,
                },
                &mut buffer,
            )
            .unwrap();
            assert_eq!(&buffer, bytes);
        }
        assert_eq!(
            pass.pinned
                .iter()
                .map(|pack| pack.digest.clone())
                .collect::<Vec<_>>(),
            original
        );
        assert_eq!(pass.open_attempts, payload_open_count(2));
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            payload_open_count(2)
        );
    }

    #[test]
    fn graph_pack_failed_cold_read_preserves_healthy_role_and_recovers() {
        let temporary = TempDir::new().unwrap();
        let roots = [temporary.path().to_path_buf()];
        let packs = (0..2)
            .map(|index| {
                let bytes = format!("sealed cold pack {index}").into_bytes();
                let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes)).unwrap();
                let path = super::segment_path(temporary.path(), &digest).unwrap();
                std::fs::write(&path, &bytes).unwrap();
                (digest, bytes, path)
            })
            .collect::<Vec<_>>();
        let mut pass = super::GraphSegmentReadPassV1::new(&roots);
        let mut buffer = Vec::new();
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let (healthy_digest, healthy_bytes, _) = &packs[0];
        let healthy = SealedGenerationSegmentReadV1::Whole {
            digest: healthy_digest,
            size_bytes: healthy_bytes.len() as u64,
        };
        pass.read(healthy, &mut buffer).unwrap();
        assert_eq!(&buffer, healthy_bytes);
        assert_eq!(pass.pinned.len(), 1);
        assert_eq!(pass.open_attempts, payload_open_count(1));

        // Open the second real role through the production admission path,
        // then truncate it before its first read. The failed cold role must
        // not displace the first role or leave its partial bytes available.
        let (digest, bytes, path) = &packs[1];
        let request = SealedGenerationSegmentReadV1::Whole {
            digest,
            size_bytes: bytes.len() as u64,
        };
        pass.pin(request).unwrap();
        assert_eq!(pass.open_attempts, payload_open_count(2));
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(0)
            .unwrap();
        buffer.extend_from_slice(b"stale");
        assert!(matches!(
            pass.read(request, &mut buffer),
            Err(CodeIndexProductionErrorV1::Contract(_))
        ));
        assert!(buffer.is_empty());
        assert_eq!(pass.pinned.len(), 1);
        assert_eq!(pass.pinned[0].digest, healthy_digest.as_str());
        pass.read(healthy, &mut buffer).unwrap();
        assert_eq!(&buffer, healthy_bytes);
        assert_eq!(pass.open_attempts, payload_open_count(2));

        std::fs::write(path, bytes).unwrap();
        pass.read(request, &mut buffer).unwrap();
        assert_eq!(&buffer, bytes);
        assert_eq!(pass.open_attempts, payload_open_count(3));
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            payload_open_count(3)
        );
        assert_eq!(pass.pinned.len(), 2);
    }

    #[test]
    fn graph_build_segment_ranges_open_once_per_pass() {
        let fixture = partitioned_seal_fixture("range-opens");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.pool_manifest).unwrap()).unwrap();
        let evidence = &manifest["generation"]["generation_evidence"];
        let digest = ManifestDigest::new(evidence["segment_digest"].as_str().unwrap()).unwrap();
        let size_bytes = evidence["segment_size_bytes"].as_u64().unwrap();
        let path = fixture.segments_root.join(format!(
            "segment-{}.json",
            sha256_hex_suffix(digest.as_str()).unwrap()
        ));
        let expected = std::fs::read(&path).unwrap();
        let routes = SealRoutes {
            seal_paths: vec![fixture.pool_manifest.clone()],
            segment_roots: vec![fixture.segments_root.clone()],
        };
        let sealed_digest = ManifestDigest::new(format!("sha256:{}", fixture.digest)).unwrap();
        let page_bytes = size_bytes.div_ceil(16);
        let read_pass = || {
            SEGMENT_OPENS.with(|opens| opens.set(0));
            let started = std::time::Instant::now();
            let mut reads = 0;
            let restored =
                with_verified_test_segments(&sealed_digest, &routes, &|| Ok(()), |read_segment| {
                    let mut restored = Vec::new();
                    let mut buffer = Vec::new();
                    let mut offset = 0;
                    while offset < size_bytes {
                        let length = page_bytes.min(size_bytes - offset);
                        read_segment(
                            SealedGenerationSegmentReadV1::Range {
                                digest: &digest,
                                size_bytes,
                                offset,
                                length,
                            },
                            &mut buffer,
                        )?;
                        restored.extend_from_slice(&buffer);
                        offset += length;
                        reads += 1;
                    }
                    Ok(restored)
                })
                .unwrap();
            assert_eq!(restored, expected);
            assert_eq!(
                ManifestDigest::from_sha256_bytes(&Sha256::digest(&restored)).unwrap(),
                digest
            );
            let opens = SEGMENT_OPENS.with(|opens| opens.get());
            println!(
                "segment range pass: reads={reads}, segments=1, opens={opens}, bytes={}, elapsed={:?}",
                restored.len(),
                started.elapsed()
            );
            assert!(reads > 1);
            assert_eq!(
                opens,
                payload_open_count(1),
                "range reads must reuse the physical pack handle"
            );
        };
        read_pass();
        read_pass();
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            with_verified_test_segments(&sealed_digest, &routes, &|| Ok(()), |read_segment| {
                read_segment(
                    SealedGenerationSegmentReadV1::Range {
                        digest: &digest,
                        size_bytes,
                        offset: 0,
                        length: page_bytes,
                    },
                    &mut Vec::new(),
                )?;
                Ok(())
            }),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

    fn partitioned_seal_fixture(label: &str) -> PartitionedSealFixture {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let project_root = root.join("project");
        std::fs::create_dir_all(project_root.join("src")).unwrap();
        git(&project_root, &["init", "-q", "-b", "main"]);
        git(&project_root, &["config", "user.name", "TraceDecay Test"]);
        git(
            &project_root,
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        for module in PARTITIONED_FIXTURE_FILES {
            std::fs::write(
                project_root.join(format!("src/{module}.rs")),
                format!(
                    "pub fn {module}_first(value: usize) -> usize {{ value + 1 }}\n\
                     pub fn {module}_second(value: usize) -> usize {{ {module}_first(value) * 2 }}\n"
                ),
            )
            .unwrap();
        }
        git(&project_root, &["add", "."]);
        git(&project_root, &["commit", "-qm", "partitioned fixture"]);
        let project_id = ProjectId::new(format!("project.manifest-{label}")).unwrap();
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &project_root,
            project_id.as_str(),
        )
        .unwrap();
        let canonical_project = project_root.canonicalize().unwrap();
        let store_root = root.join("code-index-store");
        let scoped_store = scoped_code_index_store_root(&store_root, &canonical_project);
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .unwrap();
        scheduler.reconcile_now().unwrap();
        let latest = scheduler.latest_complete().unwrap();
        let repository = latest.generation().snapshot().repository.clone();
        let generation = latest.generation().manifest().generation_id.clone();
        let resident_memory = scheduler
            .code_graph_replay_binding(&generation)
            .unwrap()
            .resident_memory;
        drop(scheduler);
        let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
            &std::fs::read(scoped_store.join("active-code-generation-v1.json")).unwrap(),
        )
        .unwrap();
        let digest = sha256_hex_suffix(&pointer.state_digest).unwrap().to_owned();
        let canonical_manifest = scoped_store
            .join("code-generations-v1")
            .join(pointer.generation_file);
        let segments_root = code_generation_segments_root(&scoped_store);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&canonical_manifest).unwrap()).unwrap();
        // Graph pages are the only segments a build reads, and verification
        // reads them after the evidence Range read that fires its barrier.
        assert!(
            manifest["generation"]["code_graph_pages"]
                .as_array()
                .unwrap()
                .len()
                > 1,
            "fixture must read several graph pages after the evidence Range read"
        );
        let replay_root = root.join("replay-pool");
        tracedecay_private_fs::create_private_directory(&replay_root).unwrap();
        let pool_manifest = replay_root.join(canonical_manifest.file_name().unwrap());
        {
            let _store_lock = acquire_code_generation_store_lock(&scoped_store).unwrap();
            let _pool_lock = acquire_code_generation_store_lock(&replay_root).unwrap();
            std::fs::rename(canonical_manifest, &pool_manifest).unwrap();
        }
        PartitionedSealFixture {
            _temporary: temporary,
            pool_manifest,
            scope_root: scoped_store,
            segments_root,
            digest,
            project: project_id,
            repository,
            generation,
            resident_memory,
        }
    }

    #[test]
    fn successor_pack_proof_rejects_inherited_witnesses_and_wrong_manifest_extent() {
        let fixture = partitioned_seal_fixture("inherited-extent");
        let verified = VerifiedSealedSegmentsV1::default();
        let generations_root = fixture.scope_root.join("code-generations-v1");
        let replay_root = fixture.pool_manifest.parent().unwrap();
        let first_manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.pool_manifest).unwrap()).unwrap();
        verify_sealed_generation_source_from_roots(
            &generations_root,
            replay_root,
            &test_resident_memory(),
            &SealedGraphStateDigest::try_from(format!("sha256:{}", fixture.digest)).unwrap(),
            &verified,
            &|| Ok(()),
        )
        .unwrap();

        std::fs::rename(
            &fixture.pool_manifest,
            generations_root.join(fixture.pool_manifest.file_name().unwrap()),
        )
        .unwrap();
        let project_root = fixture
            ._temporary
            .path()
            .join("project")
            .canonicalize()
            .unwrap();
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            fixture.project.clone(),
            &project_root,
            fixture.scope_root.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .unwrap();
        std::fs::write(
            project_root.join("src/alpha.rs"),
            "pub fn changed_alpha(value: usize) -> usize { value + 7 }\n",
        )
        .unwrap();
        scheduler.reconcile_now().unwrap();
        assert_ne!(
            scheduler
                .latest_complete()
                .unwrap()
                .generation()
                .manifest()
                .generation_id,
            fixture.generation
        );
        drop(scheduler);
        let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
            &std::fs::read(fixture.scope_root.join("active-code-generation-v1.json")).unwrap(),
        )
        .unwrap();
        let successor_digest =
            SealedGraphStateDigest::try_from(pointer.state_digest.clone()).unwrap();
        let successor_path = generations_root.join(pointer.generation_file);
        let successor_bytes = std::fs::read(&successor_path).unwrap();
        let mut successor: serde_json::Value = serde_json::from_slice(&successor_bytes).unwrap();

        // Logical source identity survives a one-file edit, but its extent is
        // now bound to a different immutable physical pack. A proof of the
        // parent file cannot authorize this successor's physical bytes.
        let parent_file_digests = first_manifest["generation"]["file_segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|segment| segment["segment_digest"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        let inherited_files = successor["generation"]["file_segments"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|segment| {
                parent_file_digests.contains(segment["segment_digest"].as_str().unwrap())
            })
            .count();
        assert!(
            inherited_files > 0,
            "a one-file edit must retain sealed files"
        );
        let count_proof_reads = |record: &VerifiedSealedSegmentsV1| {
            let reads = AtomicUsize::new(0);
            assert_eq!(
                verify_sealed_generation_source_from_roots(
                    &generations_root,
                    replay_root,
                    &test_resident_memory(),
                    &successor_digest,
                    record,
                    &|| {
                        reads.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                ),
                Ok(())
            );
            reads.load(Ordering::SeqCst)
        };
        assert_ne!(
            first_manifest["generation"]["content_pack"]["digest"],
            successor["generation"]["content_pack"]["digest"],
            "the changed logical roster requires a new content pack"
        );
        assert_ne!(
            first_manifest["generation"]["generation_evidence"]["segment_digest"],
            successor["generation"]["generation_evidence"]["segment_digest"],
            "the new generation must own its evidence pack"
        );
        let cold_reads = count_proof_reads(&VerifiedSealedSegmentsV1::default());
        let trace = tracedecay_runtime_core::logging::FormattedTracingCapture::start();
        let parent_witness_reads = count_proof_reads(&verified);
        let observed = trace.text();
        drop(trace);
        let observed_field = |name: &str| -> u64 {
            observed
                .split(&format!("{name}="))
                .nth(1)
                .and_then(|field| field.split_whitespace().next())
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| {
                    panic!("missing numeric {name} in sealed proof trace: {observed}")
                })
        };
        assert!(observed.contains("event=sealed_source_proof"));
        assert!(observed_field("hashed_segments") > 0);
        assert_eq!(
            observed_field("reused_segments"),
            0,
            "even unchanged logical digests must be proved under the successor's pack binding"
        );
        assert_eq!(
            parent_witness_reads, cold_reads,
            "a parent proof must not bypass physical or logical successor authentication"
        );
        let successor_warm_reads = count_proof_reads(&verified);
        if cfg!(unix) {
            assert!(
                successor_warm_reads < cold_reads,
                "an unchanged successor pack must retain its completed proof"
            );
        } else {
            assert_eq!(successor_warm_reads, cold_reads);
        }

        // A warm successor must still reject corruption of the changed file's
        // exact extent inside its physical content pack.
        let changed_segment = successor["generation"]["file_segments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|segment| {
                !parent_file_digests.contains(segment["segment_digest"].as_str().unwrap())
            })
            .expect("a one-file successor must seal a changed file");
        let changed_digest = tracedecay_domain::ManifestDigest::new(
            changed_segment["segment_digest"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
        .unwrap();
        let (changed_path, changed_offset, changed_length) = packed_extent(
            &successor_bytes,
            &fixture.segments_root,
            &changed_digest,
            changed_segment["segment_size_bytes"].as_u64().unwrap(),
        );
        let original_bytes = std::fs::read(&changed_path).unwrap();
        assert!(changed_length > 0);
        assert_eq!(
            ManifestDigest::from_sha256_bytes(&Sha256::digest(
                &original_bytes[changed_offset..changed_offset + changed_length],
            ))
            .unwrap(),
            changed_digest,
            "the injected corruption must target the edited logical file"
        );
        let successor_record = VerifiedSealedSegmentsV1::default();
        assert_eq!(
            verify_sealed_generation_source_from_roots(
                &generations_root,
                replay_root,
                &test_resident_memory(),
                &successor_digest,
                &successor_record,
                &|| Ok(()),
            ),
            Ok(())
        );
        // The check callback also runs for manifest and lifecycle checks,
        // so a positive count alone does not prove a segment was rehashed.
        let warmed_reads = count_proof_reads(&successor_record);
        let original_permissions = std::fs::metadata(&changed_path).unwrap().permissions();
        let mut writable_permissions = original_permissions.clone();
        #[allow(clippy::permissions_set_readonly_false)]
        writable_permissions.set_readonly(false);
        std::fs::set_permissions(&changed_path, writable_permissions).unwrap();
        let mut tampered_bytes = original_bytes.clone();
        tampered_bytes[changed_offset] ^= 1;
        std::fs::write(&changed_path, tampered_bytes).unwrap();
        let corrupt_changed = verify_sealed_generation_source_from_roots(
            &generations_root,
            replay_root,
            &test_resident_memory(),
            &successor_digest,
            &successor_record,
            &|| Ok(()),
        );
        assert!(
            matches!(corrupt_changed, Err(GraphDbError::Corrupt { .. })),
            "a warmed successor accepted a corrupt changed segment: {corrupt_changed:?}"
        );
        std::fs::write(&changed_path, original_bytes).unwrap();
        std::fs::set_permissions(&changed_path, original_permissions).unwrap();

        // A failed proof must not evict the last complete evidence-pack proof.
        // Every extent in the changed content pack must be authenticated again;
        // the untouched evidence pack retains its exact physical witness.
        let recovered_reads = count_proof_reads(&successor_record);
        if cfg!(unix) {
            assert!(
                recovered_reads > warmed_reads,
                "recovery did not re-authenticate repaired source: warmed={warmed_reads}, recovered={recovered_reads}"
            );
        } else {
            // Without fingerprint reuse, a warm proof still rehashes every
            // segment. The corruption refusal above checks edited source.
            assert_eq!(
                recovered_reads, warmed_reads,
                "a platform without fingerprints must rehash every segment"
            );
        }
        let rewarmed_reads = count_proof_reads(&successor_record);
        assert_eq!(
            rewarmed_reads, warmed_reads,
            "successful recovery must restore the stable warm proof cost"
        );
        let recovery_cold_reads = count_proof_reads(&VerifiedSealedSegmentsV1::default());
        if cfg!(unix) {
            assert!(
                recovery_cold_reads > recovered_reads,
                "a failed content proof evicted the unchanged evidence-pack witness: \
                 cold={recovery_cold_reads}, recovered={recovered_reads}"
            );
        } else {
            assert_eq!(
                recovery_cold_reads, recovered_reads,
                "platforms without reliable fingerprints must rehash every segment"
            );
        }

        let inherited = successor["generation"]["file_segments"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|segment| {
                first_manifest["generation"]["file_segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|prior| prior["segment_digest"] == segment["segment_digest"])
            })
            .expect("a one-file successor inherits an unchanged segment");
        inherited["segment_size_bytes"] =
            (inherited["segment_size_bytes"].as_u64().unwrap() + 1).into();
        // Authenticate the changed descriptor at both seal layers so only
        // the segment extent can distinguish the warm and cold proofs.
        let state_digest = tracedecay_domain::ManifestDigest::from_sha256_bytes(&Sha256::digest(
            serde_json::to_vec(&successor["generation"]).unwrap(),
        ))
        .unwrap();
        successor["state_digest"] = state_digest.as_str().into();
        let bytes = serde_json::to_vec(&successor).unwrap();
        let digest =
            tracedecay_domain::ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes)).unwrap();
        let mut permissions = std::fs::metadata(&successor_path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&successor_path, permissions).unwrap();
        std::fs::write(&successor_path, bytes).unwrap();
        std::fs::rename(
            successor_path,
            generations_root.join(format!(
                "generation-{}.json",
                sha256_hex_suffix(digest.as_str()).unwrap()
            )),
        )
        .unwrap();
        let digest = SealedGraphStateDigest::try_from(digest.as_str().to_owned()).unwrap();
        let prove = |record: &VerifiedSealedSegmentsV1| {
            verify_sealed_generation_source_from_roots(
                &generations_root,
                replay_root,
                &test_resident_memory(),
                &digest,
                record,
                &|| Ok(()),
            )
        };
        let cold = prove(&VerifiedSealedSegmentsV1::default());
        assert!(
            matches!(cold, Err(GraphDbError::Corrupt { .. })),
            "{cold:?}"
        );
        let warm = prove(&verified);
        assert!(
            matches!(warm, Err(GraphDbError::Corrupt { .. })),
            "{warm:?}"
        );
    }

    /// Repeated proofs of the same immutable packs can reuse complete physical
    /// and extent witnesses. Both cached and uncached paths refuse a rewrite.
    #[test]
    fn a_repeated_source_proof_reuses_only_unchanged_pack_extents() {
        let fixture = partitioned_seal_fixture("repeated-proof");
        let manifest_bytes = std::fs::read(&fixture.pool_manifest).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        let generation = &manifest["generation"];
        let unchanged_segments = generation["file_segments"].as_array().unwrap().len()
            + generation["code_graph_pages"].as_array().unwrap().len();
        let verified = VerifiedSealedSegmentsV1::default();
        let checks = AtomicUsize::new(0);
        let prove = || {
            checks.store(0, Ordering::SeqCst);
            let proven = verify_checked_seal_bundle_with_evidence_barrier(
                &fixture.pool_manifest,
                &fixture.segments_root,
                &fixture.digest,
                &verified,
                &|| {
                    checks.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                &test_resident_memory(),
                || {},
            );
            (proven, checks.load(Ordering::SeqCst))
        };

        SEGMENT_OPENS.with(|opens| opens.set(0));
        let (first, first_reads) = prove();
        assert_eq!(first, Ok(()));
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            payload_open_count(2)
        );
        SEGMENT_OPENS.with(|opens| opens.set(0));
        let (second, second_reads) = prove();
        assert_eq!(second, Ok(()));
        assert_eq!(
            SEGMENT_OPENS.with(|opens| opens.get()),
            payload_open_count(2)
        );
        if cfg!(unix) {
            assert!(
                first_reads >= second_reads + unchanged_segments,
                "the second proof read {second_reads} times after the first read {first_reads}; \
                 none of the {unchanged_segments} unchanged segments may be read again"
            );
        } else {
            assert_eq!(
                second_reads, first_reads,
                "without trustworthy file fingerprints every proof must reread every segment"
            );
        }

        let descriptor = &generation["code_graph_pages"][0];
        let page_digest = ManifestDigest::new(descriptor["page_digest"].as_str().unwrap()).unwrap();
        let (page, offset, length) = packed_extent(
            &manifest_bytes,
            &fixture.segments_root,
            &page_digest,
            descriptor["size_bytes"].as_u64().unwrap(),
        );
        assert!(length > 0);
        let mut bytes = std::fs::read(&page).unwrap();
        bytes[offset] ^= 1;
        let mut permissions = std::fs::metadata(&page).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&page, permissions).unwrap();
        std::fs::write(&page, &bytes).unwrap();
        assert!(matches!(prove().0, Err(GraphDbError::Corrupt { .. })));
    }

    #[test]
    fn active_routes_skip_absent_locked_store_for_canonical_and_pool_hydration() {
        let fixture = partitioned_seal_fixture("active-routes");
        let provider = Arc::new(DaemonCodeGraphManifestProviderV1::default());
        let shard = StoreShardIdV1::project(
            BrainId::new("brain.active-routes").unwrap(),
            UserProfileId::new("profile.active-routes").unwrap(),
            fixture.project.clone(),
        );
        let absent_store = fixture
            .pool_manifest
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("aaa-absent");
        std::fs::create_dir_all(absent_store.join("code-generations-v1")).unwrap();
        let replay_root = fixture.pool_manifest.parent().unwrap().to_path_buf();
        let absent_route = provider
            .bind(
                shard.clone(),
                fixture.project.clone(),
                fixture.repository.clone(),
                absent_store.join("code-generations-v1"),
                replay_root.clone(),
                Arc::clone(&fixture.resident_memory),
            )
            .unwrap();
        let store = fixture.scope_root.as_path();
        assert!(absent_store.as_path() < store);
        let route = provider
            .bind(
                shard.clone(),
                fixture.project.clone(),
                fixture.repository.clone(),
                store.join("code-generations-v1"),
                replay_root,
                Arc::clone(&fixture.resident_memory),
            )
            .unwrap();
        let equal_route = provider
            .bind(
                shard.clone(),
                fixture.project.clone(),
                fixture.repository.clone(),
                store.join("code-generations-v1"),
                fixture.pool_manifest.parent().unwrap().to_path_buf(),
                Arc::clone(&fixture.resident_memory),
            )
            .unwrap();
        drop(route);
        let owner = GraphProjectionIdentityV1 {
            shard_id: shard,
            namespace: GraphNamespaceV1::new("namespace.active-routes").unwrap(),
            projection: GraphProjectionIdV1::new("code-generation").unwrap(),
        };
        let source = SealedCodeGenerationReplay {
            repository: fixture.repository.clone(),
            generation: fixture.generation.clone(),
            sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                "sha256:{}",
                fixture.digest
            ))
            .unwrap(),
            projector_revision: GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .unwrap(),
        };

        let _absent_lock = acquire_code_generation_store_lock(&absent_store).unwrap();
        let canonical = store
            .join("code-generations-v1")
            .join(fixture.pool_manifest.file_name().unwrap());
        std::fs::copy(&fixture.pool_manifest, &canonical).unwrap();
        let hydrate_route = |route_kind: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            hydrate(&provider, &owner, &source, &|| {
                if std::time::Instant::now() >= deadline {
                    Err(GraphDbError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            })
            .unwrap_or_else(|error| panic!("{route_kind} hydration failed: {error:?}"));
        };
        hydrate_route("canonical");
        std::fs::remove_file(&canonical).unwrap();
        hydrate_route("pool");
        // A verified pool copy also recovers a damaged canonical payload.
        std::fs::write(&canonical, b"corrupt").unwrap();
        hydrate_route("canonical recovery");
        drop(equal_route);
        drop(absent_route);
        assert!(provider.sources.read().unwrap().is_empty());
        assert!(matches!(
            hydrate(&provider, &owner, &source, &|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));
    }

    #[test]
    fn equal_route_references_retire_exactly_and_reject_divergent_replay_authority() {
        let temporary = TempDir::new().unwrap();
        let generations = temporary.path().join("generations");
        let replay = temporary.path().join("replay");
        let (provider, initial, owner, source) = fixture(generations.clone(), replay.clone());
        let resident_memory = Arc::clone(
            &provider
                .sources
                .read()
                .unwrap()
                .get(&owner.shard_id)
                .unwrap()
                .resident_memory,
        );
        drop(initial);
        for _ in 0..16 {
            let first = provider
                .bind(
                    owner.shard_id.clone(),
                    ProjectId::new("project.provider").unwrap(),
                    source.repository.clone(),
                    generations.clone(),
                    replay.clone(),
                    Arc::clone(&resident_memory),
                )
                .unwrap();
            let second = provider
                .bind(
                    owner.shard_id.clone(),
                    ProjectId::new("project.provider").unwrap(),
                    source.repository.clone(),
                    generations.clone(),
                    replay.clone(),
                    Arc::clone(&resident_memory),
                )
                .unwrap();
            assert!(matches!(
                provider.bind(
                    owner.shard_id.clone(),
                    ProjectId::new("project.provider").unwrap(),
                    source.repository.clone(),
                    generations.clone(),
                    temporary.path().join("other-replay"),
                    Arc::clone(&resident_memory)
                ),
                Err(GraphDbError::Conflict { .. })
            ));
            assert!(matches!(
                provider.bind(
                    owner.shard_id.clone(),
                    ProjectId::new("project.provider").unwrap(),
                    source.repository.clone(),
                    generations.clone(),
                    replay.clone(),
                    test_resident_memory(),
                ),
                Err(GraphDbError::Conflict { .. })
            ));
            drop(first);
            let sources = provider.sources.read().unwrap();
            let routes = &sources.get(&owner.shard_id).unwrap().routes;
            assert_eq!(routes.len(), 1);
            assert_eq!(routes.values().copied().collect::<Vec<_>>(), vec![1]);
            drop(sources);
            drop(second);
            assert!(provider.sources.read().unwrap().is_empty());
        }
    }

    #[test]
    fn manifest_admission_precedes_parse_and_payload_reads_and_releases_for_retry() {
        let fixture = partitioned_seal_fixture("manifest-admission");
        let manifest_len = std::fs::metadata(&fixture.pool_manifest).unwrap().len();
        let owner_bytes = std::mem::size_of::<super::CodeGraphMemoryV1>() as u64;
        let raw_only = test_resident_memory_with_limit(manifest_len + owner_bytes);
        let mut memory =
            super::CodeGraphMemoryV1::new(&raw_only, "code_graph.manifest.v1").unwrap();
        let manifest = read_verified_seal_manifest(
            &fixture.pool_manifest,
            &fixture.digest,
            &|| Ok(()),
            &mut memory,
        )
        .expect("the raw allocation fits before decoded manifest admission");
        assert_eq!(raw_only.snapshot().used_bytes, manifest_len + owner_bytes);
        assert!(matches!(
            super::open_verified_seal(manifest, &fixture.digest, memory),
            Err(GraphDbError::BudgetExhausted {
                kind: tracedecay_graph_db::GraphBudgetKind::ResidentMemory,
                ..
            })
        ));
        assert_eq!(raw_only.snapshot().used_bytes, 0);

        let provider = Arc::new(DaemonCodeGraphManifestProviderV1::default());
        let shard = StoreShardIdV1::project(
            BrainId::new("brain.manifest-admission").unwrap(),
            UserProfileId::new("profile.manifest-admission").unwrap(),
            fixture.project.clone(),
        );
        let generations = fixture.scope_root.join("code-generations-v1");
        let replay = fixture.pool_manifest.parent().unwrap().to_path_buf();
        let bind = |memory| {
            provider
                .bind(
                    shard.clone(),
                    fixture.project.clone(),
                    fixture.repository.clone(),
                    generations.clone(),
                    replay.clone(),
                    memory,
                )
                .unwrap()
        };
        let route = bind(Arc::clone(&raw_only));
        let owner = GraphProjectionIdentityV1 {
            shard_id: shard.clone(),
            namespace: GraphNamespaceV1::new("namespace.manifest-admission").unwrap(),
            projection: GraphProjectionIdV1::new("code-generation").unwrap(),
        };
        let source = SealedCodeGenerationReplay {
            repository: fixture.repository.clone(),
            generation: fixture.generation.clone(),
            sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                "sha256:{}",
                fixture.digest
            ))
            .unwrap(),
            projector_revision: GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .unwrap(),
        };
        SEGMENT_OPENS.with(|opens| opens.set(0));
        assert!(matches!(
            hydrate(&provider, &owner, &source, &|| Ok(())),
            Err(GraphDbError::BudgetExhausted {
                kind: tracedecay_graph_db::GraphBudgetKind::ResidentMemory,
                ..
            })
        ));
        assert_eq!(SEGMENT_OPENS.with(|opens| opens.get()), 0);
        assert_eq!(raw_only.snapshot().used_bytes, 0);
        drop(route);

        let baseline = fixture.resident_memory.snapshot().used_bytes;
        let _route = bind(Arc::clone(&fixture.resident_memory));
        let admitted = open_seal_from_roots(
            &generations,
            &replay,
            &fixture.resident_memory,
            &source.sealed_state_digest,
            &fixture.generation,
            &|| Ok(()),
            "manifest_admission_retry",
        )
        .unwrap();
        assert!(
            fixture.resident_memory.snapshot().used_bytes
                >= baseline + admitted.admitted.source.retained_bytes() as u64
        );
        drop(admitted);
        assert_eq!(fixture.resident_memory.snapshot().used_bytes, baseline);
        let rows = hydrate(&provider, &owner, &source, &|| Ok(())).unwrap();
        assert_eq!(rows.row_counts(), (10, 9));
        assert_eq!(fixture.resident_memory.snapshot().used_bytes, baseline);
    }

    /// Builds the fixture's graph from its pool seal and interrupts the build
    /// once `reads` segment reads have passed their check.
    /// Builds the fixture's graph from its pool seal, probing `check` before
    /// every read.
    fn spill_partitioned(
        fixture: &PartitionedSealFixture,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<SpilledGraphGeneration, GraphDbError> {
        let generations_root = fixture.scope_root.join("code-generations-v1");
        let replay_root = fixture.pool_manifest.parent().unwrap();
        let owner = GraphProjectionIdentityV1 {
            shard_id: StoreShardIdV1::project(
                BrainId::new("brain.spill-interruption").unwrap(),
                UserProfileId::new("profile.spill-interruption").unwrap(),
                fixture.project.clone(),
            ),
            namespace: GraphNamespaceV1::new("namespace.spill-interruption").unwrap(),
            projection: GraphProjectionIdV1::new("code-generation").unwrap(),
        };
        spill_sealed_generation_graph_from_roots(
            &generations_root,
            replay_root,
            &fixture.resident_memory,
            &SealedGraphStateDigest::try_from(format!("sha256:{}", fixture.digest)).unwrap(),
            &fixture.generation,
            GraphProjectionIdentity::new(
                GraphNamespace::new(owner.namespace.as_str()).unwrap(),
                GraphProjectionId::new(owner.projection.as_str()).unwrap(),
            ),
            &GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .unwrap(),
            spill_for(&owner),
            check,
        )
    }

    fn spill_partitioned_with_interruption(
        label: &str,
        reads: usize,
        interruption: GraphDbError,
    ) -> GraphDbError {
        let fixture = partitioned_seal_fixture(label);
        let checks = AtomicUsize::new(0);
        let error = spill_partitioned(&fixture, &|| {
            if checks.fetch_add(1, Ordering::SeqCst) >= reads {
                Err(interruption.clone())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(checks.load(Ordering::SeqCst), reads + 1);
        error
    }

    /// Retention retires a seal without waiting for a graph build reading it,
    /// and the segment sweep then collects its segments. The build read a
    /// superseded generation, not a corrupt one; the same swept segments
    /// under a seal a root still names are corruption.
    #[test]
    fn a_seal_retired_during_a_build_segment_read_is_superseded_not_corrupt() {
        let build_sweeping_segments = |label: &str, retire_seal: bool| {
            let fixture = partitioned_seal_fixture(label);
            let manifest_checks = AtomicUsize::new(0);
            read_verified_seal_manifest(
                &fixture.pool_manifest,
                &fixture.digest,
                &|| {
                    manifest_checks.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                &mut test_manifest_memory(),
            )
            .unwrap();
            let manifest_checks = manifest_checks.load(Ordering::SeqCst);
            let staged = fixture
                .pool_manifest
                .with_file_name(format!(".generation-{}.unlink-test", fixture.digest));
            let checks = AtomicUsize::new(0);
            spill_partitioned(&fixture, &|| {
                if checks.fetch_add(1, Ordering::SeqCst) == manifest_checks {
                    if retire_seal {
                        std::fs::rename(&fixture.pool_manifest, &staged).unwrap();
                    }
                    for entry in std::fs::read_dir(&fixture.segments_root).unwrap() {
                        let path = entry.unwrap().path();
                        if path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("segment-"))
                        {
                            std::fs::remove_file(path).unwrap();
                        }
                    }
                }
                Ok(())
            })
            .unwrap_err()
        };

        let live = build_sweeping_segments("build-swept-live", false);
        assert!(matches!(live, GraphDbError::Corrupt { .. }), "{live:?}");
        let retired = build_sweeping_segments("build-swept-retired", true);
        assert_eq!(
            retired.to_string(),
            "sealed code generation was superseded while it was read; retry against the current seal"
        );
    }

    #[test]
    fn failed_seal_evidence_read_preserves_post_io_interruption_and_recovers() {
        for (label, interruption) in [
            ("cancel", GraphDbError::Cancelled),
            ("deadline", GraphDbError::DeadlineExceeded),
        ] {
            let fixture = partitioned_seal_fixture(&format!("verify-late-{label}"));
            let verified = VerifiedSealedSegmentsV1::default();
            // Prove this is a valid, nonempty production generation before
            // injecting a real range-read failure on the next fresh proof.
            verify_checked_seal_bundle(
                &fixture.pool_manifest,
                &fixture.segments_root,
                &fixture.digest,
                &verified,
                &|| Ok(()),
                &test_resident_memory(),
            )
            .unwrap();
            let interrupted = AtomicBool::new(false);
            let late_checks = AtomicUsize::new(0);
            let barriers = AtomicUsize::new(0);
            let originals = std::cell::RefCell::new(Vec::<(std::path::PathBuf, Vec<u8>)>::new());
            SEGMENT_OPENS.with(|opens| opens.set(0));
            let failure = verify_checked_seal_bundle_with_evidence_barrier(
                &fixture.pool_manifest,
                &fixture.segments_root,
                &fixture.digest,
                &VerifiedSealedSegmentsV1::default(),
                &|| {
                    if interrupted.load(Ordering::SeqCst) {
                        late_checks.fetch_add(1, Ordering::SeqCst);
                        Err(interruption.clone())
                    } else {
                        Ok(())
                    }
                },
                &test_resident_memory(),
                || {
                    barriers.fetch_add(1, Ordering::SeqCst);
                    // The range handle is already open. Truncate the disposable
                    // fixture's segment files so that the read itself rejects.
                    for entry in std::fs::read_dir(&fixture.segments_root).unwrap() {
                        let path = entry.unwrap().path();
                        if !path
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with("segment-"))
                        {
                            continue;
                        }
                        let bytes = std::fs::read(&path).unwrap();
                        originals.borrow_mut().push((path.clone(), bytes));
                        // Published segments can be read-only. This disposable
                        // fixture must permit the deliberate in-place fault.
                        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        permissions.set_readonly(false);
                        std::fs::set_permissions(&path, permissions).unwrap();
                        std::fs::OpenOptions::new()
                            .write(true)
                            .open(&path)
                            .unwrap()
                            .set_len(0)
                            .unwrap();
                    }
                    interrupted.store(true, Ordering::SeqCst);
                },
            )
            .unwrap_err();
            assert_eq!(failure, interruption);
            assert_eq!(barriers.load(Ordering::SeqCst), 1);
            assert_eq!(late_checks.load(Ordering::SeqCst), 1);
            assert!(SEGMENT_OPENS.with(|opens| opens.get()) > 0);
            assert!(!originals.borrow().is_empty());
            for (path, bytes) in originals.into_inner() {
                std::fs::write(path, bytes).unwrap();
            }
            // An interrupted proof must not poison the original generation.
            verify_checked_seal_bundle(
                &fixture.pool_manifest,
                &fixture.segments_root,
                &fixture.digest,
                &VerifiedSealedSegmentsV1::default(),
                &|| Ok(()),
                &test_resident_memory(),
            )
            .unwrap();
        }
    }

    fn verify_partitioned_with_interruption(
        label: &str,
        interruption: GraphDbError,
    ) -> GraphDbError {
        let fixture = partitioned_seal_fixture(label);
        let evidence_ranges_started = AtomicBool::new(false);
        let interrupted_range_checks = AtomicUsize::new(0);
        let error = verify_checked_seal_bundle_with_evidence_barrier(
            &fixture.pool_manifest,
            &fixture.segments_root,
            &fixture.digest,
            &VerifiedSealedSegmentsV1::default(),
            &|| {
                if evidence_ranges_started.load(Ordering::SeqCst) {
                    interrupted_range_checks.fetch_add(1, Ordering::SeqCst);
                    Err(interruption.clone())
                } else {
                    Ok(())
                }
            },
            &test_resident_memory(),
            || evidence_ranges_started.store(true, Ordering::SeqCst),
        )
        .unwrap_err();
        assert_eq!(interrupted_range_checks.load(Ordering::SeqCst), 1);
        error
    }

    #[test]
    fn sealed_graph_build_preserves_cancellation_between_segment_reads() {
        assert_eq!(
            spill_partitioned_with_interruption("spill-cancelled", 4, GraphDbError::Cancelled),
            GraphDbError::Cancelled
        );
    }

    #[test]
    fn sealed_graph_build_preserves_deadline_between_segment_reads() {
        assert_eq!(
            spill_partitioned_with_interruption(
                "spill-deadline",
                4,
                GraphDbError::DeadlineExceeded
            ),
            GraphDbError::DeadlineExceeded
        );
    }

    #[test]
    fn partitioned_verify_callback_preserves_cancellation() {
        assert_eq!(
            verify_partitioned_with_interruption("verify-cancelled", GraphDbError::Cancelled),
            GraphDbError::Cancelled
        );
    }

    #[test]
    fn partitioned_verify_callback_preserves_deadline() {
        assert_eq!(
            verify_partitioned_with_interruption("verify-deadline", GraphDbError::DeadlineExceeded,),
            GraphDbError::DeadlineExceeded
        );
    }

    /// Hydration rebuilds the sealed generation's graph rows from its segments
    /// on disk every time, so a replay whose seal is gone fails typed rather
    /// than being answered from memory, and a foreign sealed digest is never
    /// served.
    #[test]
    fn hydration_builds_the_sealed_graph_from_disk_and_fails_closed_once_it_is_gone() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let project_root = root.join("project");
        std::fs::create_dir_all(project_root.join("src")).unwrap();
        git(&project_root, &["init", "-q", "-b", "main"]);
        git(&project_root, &["config", "user.name", "TraceDecay Test"]);
        git(
            &project_root,
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::write(
            project_root.join("src/lib.rs"),
            "pub fn single_pass_value() -> usize { 11 }\n",
        )
        .unwrap();
        git(&project_root, &["add", "."]);
        git(&project_root, &["commit", "-qm", "single-pass fixture"]);
        let project_id = ProjectId::new("project.manifest-single-pass").unwrap();
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &project_root,
            project_id.as_str(),
        )
        .unwrap();
        let canonical_project = project_root.canonicalize().unwrap();

        // Seal one real generation through the production worktree scheduler.
        let store_root = root.join("code-index-store");
        let scoped_store = scoped_code_index_store_root(&store_root, &canonical_project);
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .unwrap();
        scheduler.reconcile_now().unwrap();
        let latest = scheduler.latest_complete().unwrap();
        let generation_id = latest.generation().manifest().generation_id.clone();
        let repository_id = latest.generation().snapshot().repository.clone();
        let resident_memory = scheduler
            .code_graph_replay_binding(&generation_id)
            .unwrap()
            .resident_memory;
        drop(latest);
        drop(scheduler);
        let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
            &std::fs::read(scoped_store.join("active-code-generation-v1.json")).unwrap(),
        )
        .unwrap();
        let sealed_state_digest =
            SealedGraphStateDigest::try_from(pointer.state_digest.clone()).unwrap();
        let generations_root = scoped_store.join("code-generations-v1");
        let replay_root = root.join("replay-pool");
        std::fs::create_dir_all(&replay_root).unwrap();

        let shard = StoreShardIdV1::project(
            BrainId::new("brain.single-pass").unwrap(),
            UserProfileId::new("profile.single-pass").unwrap(),
            project_id.clone(),
        );
        let provider = Arc::new(DaemonCodeGraphManifestProviderV1::default());
        let _route = provider
            .bind(
                shard.clone(),
                project_id,
                repository_id.clone(),
                generations_root.clone(),
                replay_root.clone(),
                Arc::clone(&resident_memory),
            )
            .unwrap();
        let namespace = GraphNamespace::new("namespace.single-pass").unwrap();
        let projection = tracedecay_code_index::graph_projection::code_graph_projection_identity(
            namespace.clone(),
        )
        .unwrap();
        let owner = GraphProjectionIdentityV1 {
            shard_id: shard,
            namespace: GraphNamespaceV1::new(namespace.as_str()).unwrap(),
            projection: GraphProjectionIdV1::new(projection.projection.as_str()).unwrap(),
        };
        let source = SealedCodeGenerationReplay {
            repository: repository_id,
            generation: generation_id,
            sealed_state_digest,
            projector_revision: GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .unwrap(),
        };

        let first = hydrate(&provider, &owner, &source, &|| Ok(()))
            .expect("hydration builds the sealed graph from disk");
        let second = hydrate(&provider, &owner, &source, &|| Ok(()))
            .expect("a second hydration builds it again");
        assert_eq!(first.row_counts(), second.row_counts());
        assert_eq!(
            first.expected_recovered_digest(),
            second.expected_recovered_digest()
        );

        let foreign = SealedCodeGenerationReplay {
            sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                "sha256:{}",
                "b".repeat(64)
            ))
            .unwrap(),
            ..source.clone()
        };
        assert!(matches!(
            hydrate(&provider, &owner, &foreign, &|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));

        let digest = sha256_hex_suffix(&pointer.state_digest).unwrap();
        std::fs::remove_file(generations_root.join(format!("generation-{digest}.json"))).unwrap();
        assert!(matches!(
            hydrate(&provider, &owner, &source, &|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));
    }
}

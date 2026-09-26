use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionError, SealedCodeGraphRowsError, build_sealed_code_graph_rows,
};
use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, SealedGenerationFileWindowsV1, SealedGenerationSegmentReadV1,
};
use tracedecay_code_index_retention::code_index_generations::{
    CodeGenerationStoreLockV1, GRAPH_REPLAY_POOL_ACQUIRE_POLL, code_generation_segments_root,
    try_acquire_code_generation_store_lock,
};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_domain::{ManifestDigest, ProjectId, RepositoryId, sha256_hex_suffix};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphDbError, GraphGenerationManifestProvider, GraphGenerationRowSpill,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
    SealedCodeGenerationReplay, SealedGraphStateDigest, SpilledGraphGeneration,
};
use tracedecay_runtime_core::resident_memory::ResidentMemoryPressureV1;
use tracedecay_store::{GraphProjectionIdentityV1, StoreShardIdV1};

const SEAL_READ_CHECK_BYTES: usize = 64 * 1024;

fn classify_sealed_generation_decode_error(
    error: CodeIndexProductionErrorV1,
    sealed_state_digest: &ManifestDigest,
) -> GraphDbError {
    match error {
        CodeIndexProductionErrorV1::SourceCommitmentsUnavailable => {
            GraphDbError::SourceCommitmentsUnavailable {
                sealed_state_digest: sealed_state_digest.as_str().to_owned(),
            }
        }
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

fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        left.dev() == right.dev()
            && left.ino() == right.ino()
            && left.len() == right.len()
            && left.mtime() == right.mtime()
            && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime()
            && left.ctime_nsec() == right.ctime_nsec()
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
fn same_windows_handle_identity(file: &File, path: &std::path::Path) -> Result<bool, GraphDbError> {
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

    fn finish(
        self,
        path: &std::path::Path,
        opened_metadata: &std::fs::Metadata,
        admitted_len: u64,
        expected_digest: &str,
    ) -> Result<(), GraphDbError> {
        (self.check)()?;
        let final_file_metadata =
            self.reader
                .get_ref()
                .metadata()
                .map_err(|error| GraphDbError::Corrupt {
                    message: format!(
                        "sealed code generation metadata cannot be revalidated: {error}"
                    ),
                })?;
        let final_path_metadata =
            path.symlink_metadata()
                .map_err(|error| GraphDbError::Corrupt {
                    message: format!("sealed code generation path cannot be revalidated: {error}"),
                })?;
        if !same_file_identity(opened_metadata, &final_file_metadata)
            || !same_file_identity(opened_metadata, &final_path_metadata)
            || self.bytes_read != admitted_len
        {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation identity or length changed while it was read"
                    .to_owned(),
            });
        }
        #[cfg(windows)]
        if !same_windows_handle_identity(self.reader.get_ref(), path)? {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation identity or length changed while it was read"
                    .to_owned(),
            });
        }
        if encode_lowercase_hex(&self.digest.finalize()) != expected_digest {
            return Err(GraphDbError::Corrupt {
                message: "sealed code generation filename digest does not match its bytes"
                    .to_owned(),
            });
        }
        Ok(())
    }
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
        let read = match self.reader.read(&mut buffer[..read_len]) {
            Ok(read) => read,
            Err(error) => {
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
) -> Result<(CheckedSealReader<'a>, std::fs::Metadata, u64), GraphDbError> {
    let path_metadata = path.symlink_metadata().map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation is unavailable for replay: {error}"
        ))
    })?;
    let admitted_len = validate_sealed_generation_metadata(&path_metadata)?;
    let file = File::open(path).map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation cannot be opened for replay: {error}"
        ))
    })?;
    let opened_metadata = file.metadata().map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed code generation metadata cannot be read: {error}"),
    })?;
    if !same_file_identity(&path_metadata, &opened_metadata) {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity changed while it was opened".to_owned(),
        });
    }
    #[cfg(windows)]
    if !same_windows_handle_identity(&file, path)? {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity changed while it was opened".to_owned(),
        });
    }
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
/// which makes recovery from either root equally trustworthy. Typed
/// interruptions from the caller's probe are transport states and must
/// surface immediately instead of triggering a second full read.
fn with_verified_seal_from_roots<T>(
    canonical: &std::path::Path,
    pool: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    read: impl Fn(
        &std::path::Path,
        &str,
        &dyn Fn() -> Result<(), GraphDbError>,
        CodeGenerationStoreLockV1,
    ) -> Result<T, GraphDbError>,
) -> Result<T, GraphDbError> {
    let canonical_store_root = canonical
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| GraphDbError::invalid("canonical generation root has no store parent"))?;
    let canonical_lock = acquire_generation_bundle_lock(canonical_store_root, check)?;
    let canonical_absent = matches!(
        std::fs::symlink_metadata(canonical),
        Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
    );
    if !canonical_absent {
        match read(canonical, expected_digest, check, canonical_lock) {
            Ok(value) => return Ok(value),
            Err(error @ (GraphDbError::Cancelled | GraphDbError::DeadlineExceeded)) => {
                return Err(error);
            }
            Err(canonical_error) => {
                // A concurrent retirement rename can move the seal mid-read;
                // the pool copy is digest-verified, so recovering there is
                // sound. A pool failure reports the canonical error, which
                // names the authoritative copy.
                let pool_root = pool.parent().ok_or_else(|| {
                    GraphDbError::invalid("graph replay generation has no pool parent")
                })?;
                let pool_lock = acquire_generation_bundle_lock(pool_root, check)?;
                return match read(pool, expected_digest, check, pool_lock) {
                    Ok(value) => Ok(value),
                    Err(_) => Err(canonical_error),
                };
            }
        }
    }
    drop(canonical_lock);
    let pool_root = pool
        .parent()
        .ok_or_else(|| GraphDbError::invalid("graph replay generation has no pool parent"))?;
    let pool_lock = acquire_generation_bundle_lock(pool_root, check)?;
    read(pool, expected_digest, check, pool_lock)
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

#[hotpath::measure(label = "daemon.session_registry.seal.acquire_bundle_lock")]
fn acquire_generation_bundle_lock(
    root: &std::path::Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<CodeGenerationStoreLockV1, GraphDbError> {
    loop {
        check()?;
        match try_acquire_code_generation_store_lock(root)
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?
        {
            Some(lock) => return Ok(lock),
            None => std::thread::sleep(GRAPH_REPLAY_POOL_ACQUIRE_POLL),
        }
    }
}

/// Reads the partitioned manifest at `path` and proves it is the seal named
/// by `expected_digest`. The lock proves the pathname is live while the
/// bytes are read; it drops with the returned manifest in hand, because the
/// segments it names are content-addressed and every segment read verifies
/// its own address.
#[hotpath::measure(label = "daemon.session_registry.seal.read_manifest")]
fn read_verified_seal_manifest(
    path: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    lifetime_lock: CodeGenerationStoreLockV1,
) -> Result<Vec<u8>, GraphDbError> {
    (check)()?;
    let path_metadata = path.symlink_metadata().map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation is unavailable for replay: {error}"
        ))
    })?;
    let admitted_len = validate_sealed_generation_metadata(&path_metadata)?;
    let mut file = File::open(path).map_err(|error| {
        GraphDbError::unavailable(format!(
            "sealed code generation cannot be opened for replay: {error}"
        ))
    })?;
    let opened_metadata = file.metadata().map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed code generation metadata cannot be read: {error}"),
    })?;
    if !same_file_identity(&path_metadata, &opened_metadata) {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity changed while it was opened".to_owned(),
        });
    }
    #[cfg(windows)]
    if !same_windows_handle_identity(&file, path)? {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity changed while it was opened".to_owned(),
        });
    }
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_registry.seal.decode.bytes_total").inc(admitted_len);
    let mut manifest = Vec::new();
    file.by_ref()
        .take(admitted_len)
        .read_to_end(&mut manifest)
        .map_err(|error| GraphDbError::Corrupt {
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
    drop(lifetime_lock);
    (check)()?;
    Ok(manifest)
}

/// Builds the code graph of the authenticated seal `manifest` into `spill`,
/// streaming its file segments from `segment_roots` one window at a time.
#[hotpath::measure(label = "daemon.session_registry.seal.spill_graph")]
fn spill_verified_seal_graph(
    source: &SealedGenerationFileWindowsV1,
    sealed_state_digest: &ManifestDigest,
    segment_roots: &[PathBuf],
    projection: GraphProjectionIdentity,
    projector_revision: &GraphProjectorRevision,
    spill: GraphGenerationRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SpilledGraphGeneration, GraphDbError> {
    let mut interruption = None;
    let mut read_segment = |request: SealedGenerationSegmentReadV1<'_>,
                            buffer: &mut Vec<u8>|
     -> Result<(), CodeIndexProductionErrorV1> {
        if let Err(error) = (check)() {
            if matches!(
                error,
                GraphDbError::Cancelled | GraphDbError::DeadlineExceeded
            ) {
                interruption = Some(error.clone());
            }
            return Err(CodeIndexProductionErrorV1::Contract(error.to_string()));
        }
        read_partitioned_segment(
            select_partitioned_segment_root(segment_roots, request)?,
            request,
            buffer,
        )
    };
    let built = build_sealed_code_graph_rows(
        projection,
        source,
        &mut read_segment,
        projector_revision,
        spill,
        check,
    );
    if let Some(interruption) = interruption {
        return Err(interruption);
    }
    built.map_err(|error| match error {
        SealedCodeGraphRowsError::Source(error) => {
            classify_sealed_generation_decode_error(error, sealed_state_digest)
        }
        SealedCodeGraphRowsError::Projection(error) => {
            classify_sealed_projection_build_error(error)
        }
    })
}

/// Authenticates a seal's partitioned manifest for a streaming graph build.
fn open_verified_seal(
    manifest: &[u8],
    expected_digest: &str,
) -> Result<(SealedGenerationFileWindowsV1, ManifestDigest), GraphDbError> {
    let sealed_state_digest =
        ManifestDigest::new(format!("sha256:{expected_digest}")).map_err(|error| {
            GraphDbError::Corrupt {
                message: format!(
                    "sealed code generation filename digest is not canonical: {error}"
                ),
            }
        })?;
    let source = SealedGenerationFileWindowsV1::open(manifest)
        .map_err(|error| classify_sealed_generation_decode_error(error, &sealed_state_digest))?;
    Ok((source, sealed_state_digest))
}

/// Builds the code graph of the seal `sealed_state_digest` names, read from
/// the canonical generations root or, once retention moved it, the replay
/// pool, into `spill`. The seal must hold `generation`.
#[allow(clippy::too_many_arguments)]
pub(super) fn spill_sealed_generation_graph_from_roots(
    generations_root: &std::path::Path,
    replay_root: &std::path::Path,
    sealed_state_digest: &SealedGraphStateDigest,
    generation: &tracedecay_domain::CodeGenerationId,
    projection: GraphProjectionIdentity,
    projector_revision: &GraphProjectorRevision,
    spill: GraphGenerationRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SpilledGraphGeneration, GraphDbError> {
    let digest = sha256_hex_suffix(sealed_state_digest.as_str())
        .ok_or_else(|| GraphDbError::invalid("sealed state digest is not sha256"))?;
    let seal_file = format!("generation-{digest}.json");
    let segments_root = code_generation_segments_root(
        generations_root
            .parent()
            .ok_or_else(|| GraphDbError::invalid("generation root has no store parent"))?,
    );
    let manifest = with_verified_seal_from_roots(
        &generations_root.join(&seal_file),
        &replay_root.join(&seal_file),
        digest,
        check,
        read_verified_seal_manifest,
    )?;
    let (source, sealed_state_digest) = open_verified_seal(&manifest, digest)?;
    drop(manifest);
    if source.generation_id() != generation {
        return Err(GraphDbError::conflict(
            "code_graph_manifest.spill_sealed_generation_graph",
        ));
    }
    spill_verified_seal_graph(
        &source,
        &sealed_state_digest,
        &[segments_root],
        projection,
        projector_revision,
        spill,
        check,
    )
}

struct PinnedPartitionedSegmentV1 {
    digest: String,
    size_bytes: u64,
    file: File,
}

fn partitioned_segment_request(
    request: tracedecay_code_index::production::SealedGenerationSegmentReadV1<'_>,
) -> Result<(&str, u64, u64, u64), tracedecay_code_index::production::CodeIndexProductionErrorV1> {
    use tracedecay_code_index::production::{
        CodeIndexProductionErrorV1, SealedGenerationSegmentReadV1,
    };
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
) -> Result<&'a std::path::Path, CodeIndexProductionErrorV1> {
    let (digest, _, _, _) = partitioned_segment_request(request)?;
    let digest = sha256_hex_suffix(digest).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed segment digest is not sha256".to_owned())
    })?;
    for root in roots {
        match root
            .join(format!("segment-{digest}.json"))
            .symlink_metadata()
        {
            Ok(_) => return Ok(root),
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

fn open_partitioned_segment(
    segments_root: &std::path::Path,
    request: tracedecay_code_index::production::SealedGenerationSegmentReadV1<'_>,
) -> Result<PinnedPartitionedSegmentV1, tracedecay_code_index::production::CodeIndexProductionErrorV1>
{
    use tracedecay_code_index::production::CodeIndexProductionErrorV1;

    let (digest, expected_size, _, _) = partitioned_segment_request(request)?;
    let digest_hex = sha256_hex_suffix(digest).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed segment digest is not sha256".to_owned())
    })?;
    let path = segments_root.join(format!("segment-{digest_hex}.json"));
    let path_metadata = path.symlink_metadata().map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation segment is unavailable: {error}"
        ))
    })?;
    if !path_metadata.file_type().is_file() || path_metadata.len() != expected_size {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment identity does not match its manifest".to_owned(),
        ));
    }
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
    if !same_file_identity(&path_metadata, &file_metadata) {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment identity changed while it was opened".to_owned(),
        ));
    }
    #[cfg(windows)]
    if !same_windows_handle_identity(&file, &path)
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
            "sealed generation evidence pages do not share one segment identity".to_owned(),
        ));
    }
    let length = usize::try_from(length).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation segment range exceeds addressable memory".to_owned(),
        )
    })?;
    buffer.clear();
    buffer.resize(length, 0);
    pinned
        .file
        .seek(SeekFrom::Start(offset))
        .and_then(|_| pinned.file.read_exact(buffer))
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment read failed: {error}"
            ))
        })
}

fn read_partitioned_segment(
    segments_root: &std::path::Path,
    request: tracedecay_code_index::production::SealedGenerationSegmentReadV1<'_>,
    buffer: &mut Vec<u8>,
) -> Result<(), tracedecay_code_index::production::CodeIndexProductionErrorV1> {
    use tracedecay_code_index::production::CodeIndexProductionErrorV1;
    let (digest, expected_size, offset, length) = partitioned_segment_request(request)?;
    let digest_hex = sha256_hex_suffix(digest).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed segment digest is not sha256".to_owned())
    })?;
    let segment_path = segments_root.join(format!("segment-{digest_hex}.json"));
    let metadata = segment_path.symlink_metadata().map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation segment is unavailable: {error}"
        ))
    })?;
    if !metadata.file_type().is_file() || metadata.len() != expected_size {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment identity does not match its manifest".to_owned(),
        ));
    }
    let length = usize::try_from(length).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation segment range exceeds addressable memory".to_owned(),
        )
    })?;
    buffer.clear();
    buffer.resize(length, 0);
    File::open(segment_path)
        .and_then(|mut file| {
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(buffer)
        })
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation segment read failed: {error}"
            ))
        })
}

#[hotpath::measure(label = "daemon.session_registry.seal.verify")]
fn verify_checked_seal(
    path: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let (mut reader, opened_metadata, admitted_len) = open_checked_seal_reader(path, check)?;
    let copied = std::io::copy(&mut reader, &mut std::io::sink());
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_registry.seal.verify.bytes_total").inc(reader.bytes_read);
    if let Some(error) = reader.failure.take() {
        return Err(error);
    }
    copied.map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed code generation checked read failed: {error}"),
    })?;
    reader.finish(path, &opened_metadata, admitted_len, expected_digest)
}

#[hotpath::measure(label = "daemon.session_registry.seal.verify_bundle")]
fn verify_checked_seal_bundle(
    path: &std::path::Path,
    segments_root: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    lifetime_lock: CodeGenerationStoreLockV1,
) -> Result<(), GraphDbError> {
    verify_checked_seal_bundle_with_evidence_barrier(
        path,
        segments_root,
        expected_digest,
        check,
        lifetime_lock,
        || {},
    )
}

fn verify_checked_seal_bundle_with_evidence_barrier(
    path: &std::path::Path,
    segments_root: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    lifetime_lock: CodeGenerationStoreLockV1,
    evidence_barrier: impl FnOnce(),
) -> Result<(), GraphDbError> {
    verify_checked_seal(path, expected_digest, check)?;
    let mut prefix = vec![0_u8; SEAL_READ_CHECK_BYTES];
    let mut file = File::open(path).map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed generation manifest cannot be reopened: {error}"),
    })?;
    let read = file
        .read(&mut prefix)
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("sealed generation manifest prefix read failed: {error}"),
        })?;
    prefix.truncate(read);
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
    if revision != Some(tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1) {
        return (check)();
    }
    let manifest = std::fs::read(path).map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed generation manifest read failed: {error}"),
    })?;
    let mut lifetime_lock = Some(lifetime_lock);
    let mut pinned_evidence = None;
    let mut evidence_barrier = Some(evidence_barrier);
    let mut interruption = None;
    let verified =
        tracedecay_code_index::production::CodeIndexPublishedGenerationV1::verify_partitioned_sealed(
        &manifest,
        |request, buffer| {
            if let Err(error) = (check)() {
                if matches!(error, GraphDbError::Cancelled | GraphDbError::DeadlineExceeded) {
                    interruption = Some(error.clone());
                }
                return Err(
                    tracedecay_code_index::production::CodeIndexProductionErrorV1::Contract(
                        error.to_string(),
                    ),
                );
            }
            match request {
                tracedecay_code_index::production::SealedGenerationSegmentReadV1::Whole {
                    ..
                } => read_partitioned_segment(segments_root, request, buffer),
                tracedecay_code_index::production::SealedGenerationSegmentReadV1::Range {
                    ..
                } => {
                    if pinned_evidence.is_none() {
                        pinned_evidence = Some(open_partitioned_segment(segments_root, request)?);
                        // Verification uses the same lifetime handoff as decode:
                        // pathname authority under the lock, then one pinned pack.
                        drop(lifetime_lock.take());
                        if let Some(barrier) = evidence_barrier.take() {
                            barrier();
                        }
                    }
                    read_pinned_partitioned_segment(
                        pinned_evidence.as_mut().ok_or_else(|| {
                            tracedecay_code_index::production::CodeIndexProductionErrorV1::Contract(
                                "sealed generation evidence handle was not pinned".to_owned(),
                            )
                        })?,
                        request,
                        buffer,
                    )
                }
            }
        },
    );
    if let Some(interruption) = interruption {
        return Err(interruption);
    }
    verified.map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed generation component verification failed: {error}"),
    })?;
    (check)()
}

/// Proves that the durable source backing an already-decoded generation still
/// exists under its canonical-or-retained authority with the exact digest.
/// This reads and hashes the bounded source without decoding or projecting it.
pub(super) fn verify_sealed_generation_source_from_roots(
    generations_root: &std::path::Path,
    replay_root: &std::path::Path,
    sealed_state_digest: &SealedGraphStateDigest,
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
        |path, expected_digest, check, lifetime_lock| {
            verify_checked_seal_bundle(path, &segments_root, expected_digest, check, lifetime_lock)
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
    ) -> Result<CodeGraphManifestRouteV1, GraphDbError> {
        let mut sources = self.sources.write().map_err(|_| {
            GraphDbError::unavailable("code generation manifest provider lock is poisoned")
        })?;
        let route = CodeGenerationRouteV1 {
            repository,
            generations_root,
        };
        if let Some(existing) = sources.get_mut(&project_shard) {
            if existing.project_id != project_id || existing.replay_root != replay_root {
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
        let mut manifest = None;
        let mut canonical_error = None;
        for route in &routes {
            check()?;
            let store_root = route.generations_root.parent().ok_or_else(|| {
                GraphDbError::invalid("canonical generation root has no store parent")
            })?;
            let canonical = route.generations_root.join(&seal_file);
            // Absence abstains before lock acquisition. Presence is only a
            // prefilter: the reader revalidates identity under the lock.
            if !seal_is_present(&canonical)? {
                continue;
            }
            let lock = acquire_generation_bundle_lock(store_root, check)?;
            if !seal_is_present(&canonical)? {
                // Retention may move the seal while this reader waits.
                // The single replay-pool probe below resolves that move.
                drop(lock);
                continue;
            }
            match read_verified_seal_manifest(&canonical, digest, check, lock) {
                Ok(bytes) => manifest = Some(bytes),
                Err(error @ (GraphDbError::Cancelled | GraphDbError::DeadlineExceeded)) => {
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
                let lock = acquire_generation_bundle_lock(&binding.replay_root, check)?;
                read_verified_seal_manifest(&pool, digest, check, lock).map_err(|error| {
                    if matches!(
                        error,
                        GraphDbError::Cancelled | GraphDbError::DeadlineExceeded
                    ) {
                        error
                    } else {
                        canonical_error.unwrap_or(error)
                    }
                })?
            }
        };
        let (sealed, sealed_state_digest) = open_verified_seal(&manifest, digest)?;
        drop(manifest);
        if sealed.manifest().project_id != binding.project_id
            || sealed.snapshot().repository != source.repository
            || sealed.generation_id() != &source.generation
        {
            return Err(GraphDbError::conflict(
                "code_graph_manifest.hydrate_sealed_code_generation",
            ));
        }
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new(owner.namespace.as_str())?,
            GraphProjectionId::new(owner.projection.as_str())?,
        );
        // The replay, not the current reader, owns the projector revision at
        // this boundary. An interrupted historical publication must be able
        // to reconstruct its exact rows so the ordered journal can advance;
        // the registry compares the rebuilt digests with the durable replay
        // before any row is served.
        spill_verified_seal_graph(
            &sealed,
            &sealed_state_digest,
            &segment_roots,
            projection,
            &source.projector_revision,
            spill,
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
    use std::fmt::Write as _;
    use std::io::{Seek, SeekFrom, Write};
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_code_index_retention::code_index_generations::{
        DurablePublicationPointerV1, acquire_code_generation_store_lock,
        code_generation_segments_root,
    };
    use tracedecay_domain::{CodeGenerationId, ProjectId, RepositoryId, sha256_hex_suffix};
    use tracedecay_graph_db::{
        GraphDbError, GraphGenerationManifestProvider, GraphGenerationRowSpill, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
        SealedCodeGenerationReplay, SealedGraphStateDigest, SpilledGraphGeneration,
    };
    use tracedecay_store::{
        BrainId, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1, StoreShardIdV1,
        UserProfileId,
    };

    use super::{
        DaemonCodeGraphManifestProviderV1, SEAL_READ_CHECK_BYTES,
        spill_sealed_generation_graph_from_roots, validate_sealed_generation_metadata,
        verify_checked_seal, verify_checked_seal_bundle_with_evidence_barrier,
        verify_sealed_generation_source_from_roots,
    };
    use tracedecay_code_index_runtime::code_index_scheduler::{
        CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1, scoped_code_index_store_root,
    };

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

        let _branch_route = provider
            .bind(
                owner.shard_id.clone(),
                ProjectId::new("project.provider").unwrap(),
                source.repository.clone(),
                branch_generations.clone(),
                branch_replay.clone(),
            )
            .expect("a worktree route under the same project shard is not a conflict");

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
    fn sealed_generation_read_rejects_same_length_mutation() {
        let temp = TempDir::new().unwrap();
        let bytes = vec![b'a'; SEAL_READ_CHECK_BYTES * 2];
        let digest = hex::encode(Sha256::digest(&bytes));
        let path = temp.path().join(format!("generation-{digest}.json"));
        std::fs::write(&path, bytes).unwrap();
        let checks = AtomicUsize::new(0);

        let error = verify_checked_seal(&path, &digest, &|| {
            if checks.fetch_add(1, Ordering::SeqCst) == 1 {
                let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.seek(SeekFrom::Start(SEAL_READ_CHECK_BYTES as u64))
                    .unwrap();
                file.write_all(b"z").unwrap();
                file.sync_all().unwrap();
            }
            Ok(())
        })
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
            verify_checked_seal(&path, &digest, &|| {
                if checks.fetch_add(1, Ordering::SeqCst) >= 2 {
                    Err(GraphDbError::DeadlineExceeded)
                } else {
                    Ok(())
                }
            }),
            Err(GraphDbError::DeadlineExceeded)
        );
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
                &sealed_state_digest,
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
    }

    /// 1,600 functions named `{prefix}_{index}` whose bodies apply
    /// `operator`. A clean generation's evidence is implied by its own
    /// symbols and chunks and fits one page; a successor that changes every
    /// body keeps one whole lineage row per function, which spans several.
    fn multi_page_evidence_source(prefix: &str, operator: char) -> String {
        let mut source = String::new();
        for index in 0..1_600 {
            writeln!(
                source,
                "pub fn {prefix}_{index}(value: usize) -> usize {{ value {operator} {index} }}"
            )
            .unwrap();
        }
        source
    }

    /// Publish a successor of the fixture's clean generation that changes
    /// every function body, so the active generation's evidence spans pages.
    fn publish_multi_page_evidence(
        project_root: &Path,
        prefix: &str,
        scheduler: &mut CodeIndexWorktreeSchedulerV1,
    ) {
        scheduler.reconcile_now().unwrap();
        std::fs::write(
            project_root.join("src/lib.rs"),
            multi_page_evidence_source(prefix, '*'),
        )
        .unwrap();
        git(project_root, &["commit", "-qam", "change every body"]);
        scheduler.reconcile_now().unwrap();
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
        std::fs::write(
            project_root.join("src/lib.rs"),
            multi_page_evidence_source("partitioned_fixture", '+'),
        )
        .unwrap();
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
        publish_multi_page_evidence(&project_root, "partitioned_fixture", &mut scheduler);
        let latest = scheduler.latest_complete().unwrap();
        let repository = latest.generation().snapshot().repository.clone();
        let generation = latest.generation().manifest().generation_id.clone();
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
        assert!(
            manifest["generation"]["generation_evidence"]["pages"]
                .as_array()
                .unwrap()
                .len()
                > 1,
            "fixture must reach a later evidence Range callback"
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
        }
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
            )
            .unwrap();
        let equal_route = provider
            .bind(
                shard.clone(),
                fixture.project.clone(),
                fixture.repository.clone(),
                store.join("code-generations-v1"),
                fixture.pool_manifest.parent().unwrap().to_path_buf(),
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
        #[cfg(feature = "hotpath")]
        let _profile = hotpath::HotpathGuardBuilder::new("active-graph-replay-routes").build();
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
        drop(initial);
        for _ in 0..16 {
            let first = provider
                .bind(
                    owner.shard_id.clone(),
                    ProjectId::new("project.provider").unwrap(),
                    source.repository.clone(),
                    generations.clone(),
                    replay.clone(),
                )
                .unwrap();
            let second = provider
                .bind(
                    owner.shard_id.clone(),
                    ProjectId::new("project.provider").unwrap(),
                    source.repository.clone(),
                    generations.clone(),
                    replay.clone(),
                )
                .unwrap();
            assert!(matches!(
                provider.bind(
                    owner.shard_id.clone(),
                    ProjectId::new("project.provider").unwrap(),
                    source.repository.clone(),
                    generations.clone(),
                    temporary.path().join("other-replay")
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

    /// Builds the fixture's graph from its pool seal and interrupts the build
    /// once `reads` segment reads have passed their check.
    fn spill_partitioned_with_interruption(
        label: &str,
        reads: usize,
        interruption: GraphDbError,
    ) -> GraphDbError {
        let fixture = partitioned_seal_fixture(label);
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
        let checks = AtomicUsize::new(0);
        let error = spill_sealed_generation_graph_from_roots(
            &generations_root,
            replay_root,
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
            &|| {
                if checks.fetch_add(1, Ordering::SeqCst) >= reads {
                    Err(interruption.clone())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(checks.load(Ordering::SeqCst), reads + 1);
        error
    }

    fn verify_partitioned_with_interruption(
        label: &str,
        interruption: GraphDbError,
    ) -> GraphDbError {
        let fixture = partitioned_seal_fixture(label);
        let evidence_ranges_started = AtomicBool::new(false);
        let interrupted_range_checks = AtomicUsize::new(0);
        let replay_root = fixture.pool_manifest.parent().unwrap();
        let error = verify_checked_seal_bundle_with_evidence_barrier(
            &fixture.pool_manifest,
            &fixture.segments_root,
            &fixture.digest,
            &|| {
                if evidence_ranges_started.load(Ordering::SeqCst) {
                    interrupted_range_checks.fetch_add(1, Ordering::SeqCst);
                    Err(interruption.clone())
                } else {
                    Ok(())
                }
            },
            acquire_code_generation_store_lock(replay_root).unwrap(),
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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use tracedecay_code_index::graph_projection::CodeGraphProjectionError;
use tracedecay_code_index::production::UninterruptibleCodeIndexControlV1;
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_domain::{ManifestDigest, ProjectId, RepositoryId};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphDbError, GraphGenerationManifest, GraphGenerationManifestProvider,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
    SealedCodeGenerationReplay, SealedGraphStateDigest,
};
use tracedecay_runtime_core::resident_memory::{
    ResidentMemoryPressureRegistrationV1, ResidentMemoryPressureV1,
};
use tracedecay_store::{GraphProjectionIdentityV1, StoreShardIdV1};

const SEAL_READ_CHECK_BYTES: usize = 64 * 1024;

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
    ) -> Result<T, GraphDbError>,
) -> Result<T, GraphDbError> {
    let canonical_absent = matches!(
        std::fs::symlink_metadata(canonical),
        Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
    );
    if !canonical_absent {
        match read(canonical, expected_digest, check) {
            Ok(value) => return Ok(value),
            Err(error @ (GraphDbError::Cancelled | GraphDbError::DeadlineExceeded)) => {
                return Err(error);
            }
            Err(canonical_error) => {
                // A concurrent retirement rename can move the seal mid-read;
                // the pool copy is digest-verified, so recovering there is
                // sound. A pool failure reports the canonical error, which
                // names the authoritative copy.
                return match read(pool, expected_digest, check) {
                    Ok(value) => Ok(value),
                    Err(_) => Err(canonical_error),
                };
            }
        }
    }
    read(pool, expected_digest, check)
}

fn decode_verified_seal_from_roots(
    canonical: &std::path::Path,
    pool: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<tracedecay_code_index::production::CodeIndexPublishedGenerationV1, GraphDbError> {
    with_verified_seal_from_roots(
        canonical,
        pool,
        expected_digest,
        check,
        decode_verified_seal,
    )
}

#[hotpath::measure(label = "daemon.session_registry.seal.decode")]
fn decode_verified_seal(
    path: &std::path::Path,
    expected_digest: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<tracedecay_code_index::production::CodeIndexPublishedGenerationV1, GraphDbError> {
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
    let expected_digest =
        ManifestDigest::new(format!("sha256:{expected_digest}")).map_err(|error| {
            GraphDbError::Corrupt {
                message: format!(
                    "sealed code generation filename digest is not canonical: {error}"
                ),
            }
        })?;
    (check)()?;
    let decoded = tracedecay_code_index::production::CodeIndexPublishedGenerationV1::decode_sealed_seek_reader(
        &mut file,
        admitted_len,
        Some(&expected_digest),
        &UninterruptibleCodeIndexControlV1,
    );
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_registry.seal.decode.bytes_total").inc(admitted_len);
    let generation = decoded
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("sealed code generation replay is invalid: {error}"),
        })?
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "sealed code generation format revision is incompatible".to_owned(),
        })?;
    (check)()?;
    let final_file_metadata = file.metadata().map_err(|error| GraphDbError::Corrupt {
        message: format!("sealed code generation metadata cannot be revalidated: {error}"),
    })?;
    let final_path_metadata = path
        .symlink_metadata()
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("sealed code generation path cannot be revalidated: {error}"),
        })?;
    if !same_file_identity(&opened_metadata, &final_file_metadata)
        || !same_file_identity(&opened_metadata, &final_path_metadata)
    {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity or length changed while it was read"
                .to_owned(),
        });
    }
    #[cfg(windows)]
    if !same_windows_handle_identity(&file, path)? {
        return Err(GraphDbError::Corrupt {
            message: "sealed code generation identity or length changed while it was read"
                .to_owned(),
        });
    }
    Ok(generation)
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

/// Proves that the durable source backing an already-decoded generation still
/// exists under its canonical-or-retained authority with the exact digest.
/// This reads and hashes the bounded source without decoding or projecting it.
pub(super) fn verify_sealed_generation_source_from_roots(
    generations_root: &std::path::Path,
    replay_root: &std::path::Path,
    sealed_state_digest: &SealedGraphStateDigest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let digest = sealed_state_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| GraphDbError::invalid("sealed state digest is not sha256"))?;
    let seal_file = format!("generation-{digest}.json");
    with_verified_seal_from_roots(
        &generations_root.join(&seal_file),
        &replay_root.join(&seal_file),
        digest,
        check,
        verify_checked_seal,
    )
}

#[derive(Clone)]
struct BoundCodeGenerationSourceV1 {
    project_shard: StoreShardIdV1,
    project_id: ProjectId,
    repositories: BTreeSet<RepositoryId>,
    generations_root: PathBuf,
    replay_root: PathBuf,
}

/// One already-decoded sealed generation — offered by the code-index
/// activation path or retained from this provider's own verified disk decode —
/// addressed by the exact identity that authorizes it.
///
/// The producing side decoded these bytes only after verifying that their
/// SHA-256 equals `sealed_state_digest`, so an entry that matches a replay's
/// `generation` *and* `sealed_state_digest` denotes the same immutable payload
/// the canonical seal file holds. Matching on the digest — never on the
/// generation id alone — is what keeps a superseded or foreign decode from
/// being served in place of the requested seal.
#[derive(Clone)]
struct DecodedSealedCodeGenerationV1 {
    generation: tracedecay_domain::CodeGenerationId,
    sealed_state_digest: SealedGraphStateDigest,
    decoded: Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
    /// Sealed source-byte census of the decode this offer retains. A checked
    /// fact from the generation itself, used only to report retained offer
    /// bytes; a census that cannot be computed reports zero rather than
    /// refusing the offer, because the offer is an accelerator and the census
    /// is telemetry.
    source_total_bytes: u64,
}

impl DecodedSealedCodeGenerationV1 {
    /// Census the decode as it is retained, so the byte accounting a release
    /// reports is fixed at retention time rather than recomputed from a
    /// payload that may already be gone.
    fn retained(
        generation: tracedecay_domain::CodeGenerationId,
        sealed_state_digest: SealedGraphStateDigest,
        decoded: Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
    ) -> Self {
        let source_total_bytes = decoded
            .generation_statistics()
            .map_or(0, |statistics| statistics.source_total_bytes);
        Self {
            generation,
            sealed_state_digest,
            decoded,
            source_total_bytes,
        }
    }
}

/// The decodes one shard may reuse instead of re-reading its sealed payload:
/// the decode offered by the activating code index (plan 40, stage 1) and the
/// provider's own most recent digest-verified disk decode. Both are pure
/// accelerators matched on the exact generation AND sealed-state digest; a
/// miss always falls through to the canonical-then-pool disk read, and the
/// durable-source verification in
/// [`verify_sealed_generation_source_from_roots`] never consults them.
///
/// Both slots are bounded the same two ways. Supersession bounds them inside a
/// shard: a fresh activation offer drops the hydration it replaces. Release
/// bounds them across the daemon: the retirement of the commissioning runtime
/// and the resident-memory pressure backstop each drop the whole shard entry.
#[derive(Default)]
struct ShardDecodedSealsV1 {
    offered: Option<DecodedSealedCodeGenerationV1>,
    hydrated: Option<DecodedSealedCodeGenerationV1>,
}

impl ShardDecodedSealsV1 {
    /// The decode for this exact replay identity held in either slot.
    fn matching(
        &self,
        generation: &tracedecay_domain::CodeGenerationId,
        sealed_state_digest: &SealedGraphStateDigest,
    ) -> Option<Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>> {
        self.retained()
            .find(|candidate| {
                candidate.generation == *generation
                    && candidate.sealed_state_digest == *sealed_state_digest
            })
            .map(|candidate| Arc::clone(&candidate.decoded))
    }

    fn retained(&self) -> impl Iterator<Item = &DecodedSealedCodeGenerationV1> {
        [self.offered.as_ref(), self.hydrated.as_ref()]
            .into_iter()
            .flatten()
    }

    fn retained_decodes(&self) -> usize {
        self.retained().count()
    }

    fn retained_bytes(&self) -> u64 {
        self.retained().fold(0_u64, |total, retained| {
            total.saturating_add(retained.source_total_bytes)
        })
    }
}

/// The retained decoded seals, owned separately from the provider so a
/// resident-memory pressure reclaimer can hold a `Weak` to exactly this state
/// and nothing else.
///
/// Every retained slot holds a whole decoded generation. Until release landed,
/// nothing ever removed one: a decode stayed live for the lifetime of the
/// daemon's session registry, invisible to the resident-memory admission
/// authority, which is one of the unaccounted holders behind a 16GiB limit
/// sitting inside a 42GiB process.
#[derive(Default)]
pub(super) struct DecodedCodeGenerationOffersV1 {
    seals: RwLock<BTreeMap<StoreShardIdV1, ShardDecodedSealsV1>>,
}

impl DecodedCodeGenerationOffersV1 {
    /// Record the decode the activating code index offered for this shard.
    ///
    /// A fresh activation offer supersedes whatever this provider retained
    /// from an older hydration; dropping that hydration bounds decode
    /// retention to the seals still in play for the shard.
    fn offer(
        &self,
        project_shard: StoreShardIdV1,
        offered: DecodedSealedCodeGenerationV1,
    ) -> Result<(), GraphDbError> {
        let mut seals = self.write()?;
        let slot = seals.entry(project_shard).or_default();
        slot.offered = Some(offered);
        slot.hydrated = None;
        Self::publish_retained_gauge(&seals);
        Ok(())
    }

    /// Record the digest-verified decode this provider just paid a full disk
    /// pass for, so a repeated hydration of the same replay reuses it instead
    /// of reading and parsing the sealed payload a second time.
    fn retain_hydrated(
        &self,
        project_shard: StoreShardIdV1,
        hydrated: DecodedSealedCodeGenerationV1,
    ) -> Result<(), GraphDbError> {
        let mut seals = self.write()?;
        seals.entry(project_shard).or_default().hydrated = Some(hydrated);
        Self::publish_retained_gauge(&seals);
        Ok(())
    }

    /// The retained decode for this exact replay identity, if one is held.
    ///
    /// Deliberately not take-on-read. One activation has two legitimate
    /// consumers of the same decode — the current-revision publication and the
    /// interrupted-predecessor recovery that rebuilds a historical manifest at
    /// its own projector revision — so consuming on first read would force the
    /// second to re-read and re-parse exactly the bytes this decode exists to
    /// spare. The lifetime bound is supersession and release, not first read.
    fn matching(
        &self,
        project_shard: &StoreShardIdV1,
        generation: &tracedecay_domain::CodeGenerationId,
        sealed_state_digest: &SealedGraphStateDigest,
    ) -> Result<
        Option<Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>>,
        GraphDbError,
    > {
        let seals = self.seals.read().map_err(|_| {
            GraphDbError::unavailable("code generation manifest provider lock is poisoned")
        })?;
        Ok(seals
            .get(project_shard)
            .and_then(|slot| slot.matching(generation, sealed_state_digest)))
    }

    /// Drop one shard's retained decodes at retirement and report the census
    /// bytes released.
    ///
    /// This is the primary retention fix. A retained decode is an
    /// activation-scoped accelerator over bytes that stay durable on disk;
    /// once the runtime that commissioned it retires, nothing can consume it
    /// again, so holding whole decoded generations past that point is pure
    /// resident cost. Before this, nothing removed them at all. Both slots go
    /// together: the hydration was retained to serve the same activation
    /// window as the offer.
    fn release_shard(&self, project_shard: &StoreShardIdV1) -> u64 {
        let Ok(mut seals) = self.write() else {
            return 0;
        };
        let released_bytes = seals
            .remove(project_shard)
            .map_or(0, |slot| slot.retained_bytes());
        Self::publish_retained_gauge(&seals);
        released_bytes
    }

    /// Drop every retained decode and report the census bytes released.
    ///
    /// The pressure backstop. Dropping a retained decode never loses truth:
    /// the sealed payload stays on disk and the canonical read reconstructs
    /// it, so this costs one re-decode and never revokes work that is already
    /// admitted.
    fn release_all(&self) -> u64 {
        let Ok(mut seals) = self.write() else {
            return 0;
        };
        let released_bytes = Self::retained_bytes_of(&seals);
        seals.clear();
        Self::publish_retained_gauge(&seals);
        released_bytes
    }

    #[cfg(test)]
    fn retained_offer_count(&self) -> usize {
        self.write()
            .map_or(0, |seals| Self::retained_decodes_of(&seals))
    }

    #[cfg(test)]
    fn retained_bytes(&self) -> u64 {
        self.write()
            .map_or(0, |seals| Self::retained_bytes_of(&seals))
    }

    fn write(
        &self,
    ) -> Result<
        std::sync::RwLockWriteGuard<'_, BTreeMap<StoreShardIdV1, ShardDecodedSealsV1>>,
        GraphDbError,
    > {
        self.seals.write().map_err(|_| {
            GraphDbError::unavailable("code generation manifest provider lock is poisoned")
        })
    }

    fn retained_decodes_of(seals: &BTreeMap<StoreShardIdV1, ShardDecodedSealsV1>) -> usize {
        seals
            .values()
            .map(ShardDecodedSealsV1::retained_decodes)
            .sum()
    }

    fn retained_bytes_of(seals: &BTreeMap<StoreShardIdV1, ShardDecodedSealsV1>) -> u64 {
        seals.values().fold(0_u64, |total, slot| {
            total.saturating_add(slot.retained_bytes())
        })
    }

    fn publish_retained_gauge(seals: &BTreeMap<StoreShardIdV1, ShardDecodedSealsV1>) {
        hotpath::gauge!("daemon.memory.decoded_offers_bytes")
            .set(Self::retained_bytes_of(seals) as f64);
        hotpath::gauge!("daemon.memory.decoded_offers")
            .set(Self::retained_decodes_of(seals) as f64);
    }
}

pub(super) struct DaemonCodeGraphManifestProviderV1 {
    sources: RwLock<BTreeMap<StoreShardIdV1, BoundCodeGenerationSourceV1>>,
    /// Per-shard decoded seals — the activation offer (plan 40, stage 1) and
    /// this provider's own last verified disk decode — so graph publication
    /// and the recovery branches reuse an already-verified decode instead of
    /// re-reading and re-parsing the same sealed payload. Held behind an
    /// `Arc` so the pressure reclaimer can reach exactly this state through a
    /// `Weak` without keeping the provider alive.
    decoded: Arc<DecodedCodeGenerationOffersV1>,
    /// Keeps the pressure reclaimer registered for this provider's lifetime.
    _pressure_registration: Option<ResidentMemoryPressureRegistrationV1>,
}

impl Default for DaemonCodeGraphManifestProviderV1 {
    fn default() -> Self {
        Self::with_pressure(
            tracedecay_runtime_core::resident_memory::process_resident_memory_pressure_v1(),
        )
    }
}

impl DaemonCodeGraphManifestProviderV1 {
    /// Bind the offer store to a measured-RSS pressure cell.
    ///
    /// Production passes the process cell fed by the daemon's `VmRSS` sampler.
    /// Tests pass an isolated cell so a fake RSS series drives the backstop
    /// without touching `/proc` or other cases.
    pub(super) fn with_pressure(pressure: &Arc<ResidentMemoryPressureV1>) -> Self {
        let decoded = Arc::new(DecodedCodeGenerationOffersV1::default());
        let reclaim_target = Arc::downgrade(&decoded);
        let registration = pressure
            .register_pressure_reclaimer(
                DECODED_OFFER_PRESSURE_PRIORITY_V1,
                Arc::new(move |_request| {
                    reclaim_target
                        .upgrade()
                        .map_or(0, |offers| offers.release_all())
                }),
            )
            .ok();
        Self {
            sources: RwLock::new(BTreeMap::new()),
            decoded,
            _pressure_registration: registration,
        }
    }
}

/// Decoded offers release before anything a query is actively serving from:
/// they are pure accelerators over bytes that remain on disk.
const DECODED_OFFER_PRESSURE_PRIORITY_V1: u32 = 10;

impl DaemonCodeGraphManifestProviderV1 {
    pub(super) fn bind(
        &self,
        project_shard: StoreShardIdV1,
        project_id: ProjectId,
        repository: RepositoryId,
        generations_root: PathBuf,
        replay_root: PathBuf,
    ) -> Result<(), GraphDbError> {
        let mut sources = self.sources.write().map_err(|_| {
            GraphDbError::unavailable("code generation manifest provider lock is poisoned")
        })?;
        if let Some(existing) = sources.get_mut(&project_shard) {
            if existing.project_shard != project_shard
                || existing.project_id != project_id
                || existing.generations_root != generations_root
                || existing.replay_root != replay_root
            {
                return Err(GraphDbError::conflict("code_graph_manifest.bind"));
            }
            existing.repositories.insert(repository);
            return Ok(());
        }
        sources.insert(
            project_shard.clone(),
            BoundCodeGenerationSourceV1 {
                project_shard,
                project_id,
                repositories: BTreeSet::from([repository]),
                generations_root,
                replay_root,
            },
        );
        Ok(())
    }

    /// Offer the sealed generation this shard just decoded for query serving.
    ///
    /// Cold activation decodes the sealed payload once to serve queries; without
    /// this offer the graph publication and recovery branches decode the very
    /// same bytes a second time through [`decode_verified_seal_from_roots`].
    /// The offer is a pure accelerator: it is consulted only on an exact
    /// generation-and-digest match, and every miss falls through to the
    /// canonical-then-pool read that remains the authority.
    ///
    /// The offer is released when the runtime that commissioned it retires,
    /// and dropped early under measured memory pressure, so a shard that is
    /// offered a decode nobody ever claims does not retain a whole generation
    /// for the daemon's lifetime.
    pub(super) fn offer_decoded_code_generation(
        &self,
        project_shard: StoreShardIdV1,
        generation: tracedecay_domain::CodeGenerationId,
        sealed_state_digest: SealedGraphStateDigest,
        decoded: Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
    ) -> Result<(), GraphDbError> {
        // The offer supersedes any hydration this provider retained for the
        // shard, and is censused as it lands so release can report the bytes
        // it frees.
        self.decoded.offer(
            project_shard,
            DecodedSealedCodeGenerationV1::retained(generation, sealed_state_digest, decoded),
        )
    }

    /// An already-verified decode for this exact replay — the activation
    /// offer or the provider's own last disk decode — or `None` to read from
    /// disk.
    ///
    /// `None` is an abstention, never a verdict: it means "not already decoded
    /// here", and the caller must still resolve the seal from the canonical
    /// root or the replay pool.
    fn reusable_decode(
        &self,
        owner: &GraphProjectionIdentityV1,
        source: &SealedCodeGenerationReplay,
    ) -> Result<
        Option<Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>>,
        GraphDbError,
    > {
        self.decoded.matching(
            &owner.shard_id,
            &source.generation,
            &source.sealed_state_digest,
        )
    }

    /// Retain the digest-verified decode this provider just paid a full disk
    /// pass for, so a repeated hydration of the same replay (verified-snapshot
    /// recovery, pending-predecessor completion retries) reuses it instead of
    /// reading and parsing the sealed payload a second time.
    fn retain_hydrated_decode(
        &self,
        project_shard: StoreShardIdV1,
        source: &SealedCodeGenerationReplay,
        decoded: Arc<tracedecay_code_index::production::CodeIndexPublishedGenerationV1>,
    ) -> Result<(), GraphDbError> {
        self.decoded.retain_hydrated(
            project_shard,
            DecodedSealedCodeGenerationV1::retained(
                source.generation.clone(),
                source.sealed_state_digest.clone(),
                decoded,
            ),
        )
    }

    /// Release the decoded seals this shard's retiring runtime commissioned —
    /// the activation offer and any hydration retained alongside it —
    /// reporting the census bytes released.
    pub(super) fn release_decoded_offer(&self, project_shard: &StoreShardIdV1) -> u64 {
        self.decoded.release_shard(project_shard)
    }

    #[cfg(test)]
    pub(super) fn retained_decoded_offer_count(&self) -> usize {
        self.decoded.retained_offer_count()
    }

    #[cfg(test)]
    pub(super) fn retained_decoded_offer_bytes(&self) -> u64 {
        self.decoded.retained_bytes()
    }
}

impl GraphGenerationManifestProvider for DaemonCodeGraphManifestProviderV1 {
    fn hydrate_sealed_code_generation(
        &self,
        owner: &GraphProjectionIdentityV1,
        source: &SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphGenerationManifest, GraphDbError> {
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
            || !binding.repositories.contains(&source.repository)
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

        // Reuse a decode whose SHA-256 was already proven equal to this
        // replay's sealed-state digest — the one the activating code index
        // offered (plan 40, stage 1) or the provider's own last verified disk
        // decode. The reuse is matched on the exact generation AND sealed
        // state digest, and the identity guards below still run against it, so
        // the only difference from the disk path is that the identical bytes
        // are not read and parsed a second time.
        let reused = self.reusable_decode(owner, source)?;
        let decoded_from_disk = reused.is_none();
        let generation = match reused {
            Some(already_decoded) => {
                #[cfg(feature = "hotpath")]
                hotpath::gauge!("session_registry.seal.decode.reused_total").inc(1_u64);
                already_decoded
            }
            None => {
                let digest = source
                    .sealed_state_digest
                    .as_str()
                    .strip_prefix("sha256:")
                    .ok_or_else(|| GraphDbError::invalid("sealed state digest is not sha256"))?;
                let seal_file = format!("generation-{digest}.json");
                Arc::new(decode_verified_seal_from_roots(
                    &binding.generations_root.join(&seal_file),
                    &binding.replay_root.join(&seal_file),
                    digest,
                    check,
                )?)
            }
        };
        if generation.manifest().project_id != binding.project_id
            || generation.snapshot().repository != source.repository
            || generation.manifest().generation_id != source.generation
        {
            return Err(GraphDbError::conflict(
                "code_graph_manifest.hydrate_sealed_code_generation",
            ));
        }
        if decoded_from_disk {
            self.retain_hydrated_decode(owner.shard_id.clone(), source, Arc::clone(&generation))?;
        }

        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new(owner.namespace.as_str())?,
            GraphProjectionId::new(owner.projection.as_str())?,
        );
        // The replay, not the current reader, owns the projector revision at
        // this boundary. An interrupted historical publication must be able
        // to reconstruct its exact manifest so the ordered journal can
        // advance. `GraphGenerationManifest::from_replay` compares the
        // rebuilt dependency closure and recovered digest with the durable
        // replay before any rows are served, while current graph readers keep
        // enforcing the current revision independently.
        tracedecay_code_index::graph_projection::build_published_code_graph_manifest_checked(
            projection,
            &generation,
            &GraphProjectorRevision::try_from(source.projector_revision.as_str().to_owned())?,
            check,
        )
        .map(Arc::unwrap_or_clone)
        .map_err(classify_sealed_projection_build_error)
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
    use std::io::{Seek, SeekFrom, Write};
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tracedecay_code_index_retention::code_index_generations::DurablePublicationPointerV1;
    use tracedecay_domain::{CodeGenerationId, ProjectId, RepositoryId};
    use tracedecay_graph_db::{
        GraphBudgetKind, GraphDbError, GraphGenerationManifestProvider, GraphNamespace,
        GraphProjectorRevision, SealedCodeGenerationReplay, SealedGraphStateDigest,
    };
    use tracedecay_store::{
        BrainId, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1, StoreShardIdV1,
        UserProfileId,
    };

    use super::{
        DaemonCodeGraphManifestProviderV1, SEAL_READ_CHECK_BYTES,
        validate_sealed_generation_metadata, verify_checked_seal,
        verify_sealed_generation_source_from_roots,
    };
    use tracedecay_code_index_runtime::code_index_scheduler::{
        CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1, scoped_code_index_store_root,
    };

    fn fixture(
        generations_root: std::path::PathBuf,
        replay_root: std::path::PathBuf,
    ) -> (
        DaemonCodeGraphManifestProviderV1,
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
        let provider = DaemonCodeGraphManifestProviderV1::default();
        provider
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
        let (provider, owner, source) = fixture(generations_root.clone(), replay_root.clone());
        let seal_file = format!(
            "generation-{}.json",
            source
                .sealed_state_digest
                .as_str()
                .strip_prefix("sha256:")
                .unwrap()
        );

        assert!(matches!(
            provider.hydrate_sealed_code_generation(&owner, &source, &|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));

        let mut foreign = source.clone();
        foreign.repository = RepositoryId::new("repository.foreign").unwrap();
        assert!(matches!(
            provider
                .hydrate_sealed_code_generation(&owner, &foreign, &|| Ok(()))
                .unwrap_err(),
            GraphDbError::Conflict { .. }
        ));

        // A retired seal that only survives in the replay pool is still read.
        std::fs::write(replay_root.join(&seal_file), b"corrupt").unwrap();
        assert!(matches!(
            provider.hydrate_sealed_code_generation(&owner, &source, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));

        // A canonical read failure is authoritative over a failing pool probe.
        std::fs::remove_file(replay_root.join(&seal_file)).unwrap();
        std::fs::write(generations_root.join(&seal_file), b"corrupt").unwrap();
        assert!(matches!(
            provider.hydrate_sealed_code_generation(&owner, &source, &|| Ok(())),
            Err(GraphDbError::Corrupt { .. })
        ));
    }

    #[test]
    fn seal_interruptions_surface_without_probing_the_replay_pool() {
        let temp = TempDir::new().unwrap();
        let generations_root = temp.path().join("generations");
        let replay_root = temp.path().join("replay");
        std::fs::create_dir_all(&generations_root).unwrap();
        std::fs::create_dir_all(&replay_root).unwrap();
        let (provider, owner, source) = fixture(generations_root.clone(), replay_root.clone());
        let seal_file = format!(
            "generation-{}.json",
            source
                .sealed_state_digest
                .as_str()
                .strip_prefix("sha256:")
                .unwrap()
        );
        std::fs::write(generations_root.join(&seal_file), b"canonical").unwrap();
        std::fs::write(replay_root.join(&seal_file), b"pool").unwrap();

        // Pass the entry probe, then cancel during the canonical read: the
        // typed interruption must surface without a second read against the
        // pool copy (which would probe the closure again).
        let probes = AtomicUsize::new(0);
        assert_eq!(
            provider
                .hydrate_sealed_code_generation(&owner, &source, &|| {
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
    fn sealed_projection_build_interruptions_stay_typed_and_never_read_as_corruption() {
        use tracedecay_code_index::graph_projection::CodeGraphProjectionError;

        use super::classify_sealed_projection_build_error;

        assert_eq!(
            classify_sealed_projection_build_error(CodeGraphProjectionError::DeadlineExceeded),
            GraphDbError::DeadlineExceeded
        );
        assert_eq!(
            classify_sealed_projection_build_error(CodeGraphProjectionError::Cancelled),
            GraphDbError::Cancelled
        );
        assert!(matches!(
            classify_sealed_projection_build_error(CodeGraphProjectionError::BudgetExhausted {
                budget: "capacity".to_owned(),
                limit: 7,
            }),
            GraphDbError::BudgetExhausted {
                kind: GraphBudgetKind::Capacity,
                limit: 7,
            }
        ));
        assert!(matches!(
            classify_sealed_projection_build_error(CodeGraphProjectionError::Contract(
                "entity payload is malformed".to_owned()
            )),
            GraphDbError::Corrupt { .. }
        ));
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

    /// One disk pass hydrates a replay; the second hydration of the same
    /// replay reuses that digest-verified decode and produces the identical
    /// manifest. Falsifiable by construction: the sealed file is deleted
    /// between the two hydrations, so any second read attempt fails, while
    /// durable-source verification — which must never trust the retained
    /// decode — is required to observe the loss.
    #[test]
    fn disk_hydration_is_single_pass_and_source_verification_stays_fail_closed() {
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
        let decoded_handle = latest.generation_handle();
        let generation_id = latest.generation().manifest().generation_id.clone();
        let repository_id = latest.generation().snapshot().repository.clone();
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
        let provider = DaemonCodeGraphManifestProviderV1::default();
        provider
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
            shard_id: shard.clone(),
            namespace: GraphNamespaceV1::new(namespace.as_str()).unwrap(),
            projection: GraphProjectionIdV1::new(projection.projection.as_str()).unwrap(),
        };
        let source = SealedCodeGenerationReplay {
            repository: repository_id,
            generation: generation_id,
            sealed_state_digest: sealed_state_digest.clone(),
            projector_revision: GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .unwrap(),
        };

        // Nothing was offered, so the first hydration pays the one disk pass.
        let first = provider
            .hydrate_sealed_code_generation(&owner, &source, &|| Ok(()))
            .expect("first hydration decodes the sealed payload from disk");

        // Delete the seal from both roots so any further byte pass must fail.
        let digest = pointer.state_digest.strip_prefix("sha256:").unwrap();
        let seal_file = format!("generation-{digest}.json");
        std::fs::remove_file(generations_root.join(&seal_file)).unwrap();

        // Durable-source verification never trusts the retained decode.
        assert!(matches!(
            verify_sealed_generation_source_from_roots(
                &generations_root,
                &replay_root,
                &sealed_state_digest,
                &|| Ok(()),
            ),
            Err(GraphDbError::Unavailable { .. })
        ));

        // The same replay hydrates again from the retained decode — identical
        // manifest, zero further byte passes.
        let second = provider
            .hydrate_sealed_code_generation(&owner, &source, &|| Ok(()))
            .expect("repeated hydration reuses the verified decode");
        assert_eq!(first, second);

        // The retained decode never answers a foreign sealed digest.
        let foreign = SealedCodeGenerationReplay {
            sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                "sha256:{}",
                "b".repeat(64)
            ))
            .unwrap(),
            ..source.clone()
        };
        provider
            .hydrate_sealed_code_generation(&owner, &foreign, &|| Ok(()))
            .expect_err("a foreign sealed digest must never be served from the retained decode");

        // A fresh activation offer supersedes the retained decode, so the old
        // replay can only be answered from disk again — which is now gone.
        provider
            .offer_decoded_code_generation(
                shard,
                CodeGenerationId::new("generation.superseding").unwrap(),
                foreign.sealed_state_digest.clone(),
                decoded_handle,
            )
            .unwrap();
        assert!(matches!(
            provider.hydrate_sealed_code_generation(&owner, &source, &|| Ok(())),
            Err(GraphDbError::Unavailable { .. })
        ));
    }
}

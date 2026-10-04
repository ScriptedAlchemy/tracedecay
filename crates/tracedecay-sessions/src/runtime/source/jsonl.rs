use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use sha2::{Digest, Sha256};
use tracedecay_domain::{ObservationOrderingDomainV1, ObservationSourceCursorV1};
use tracedecay_private_fs::RewriteWitness;

use super::{
    StoredCursor, TranscriptIngestError, TranscriptIngestResult, file_mtime_secs,
    should_resume_jsonl, stable_jsonl_file_id,
};

pub use crate::runtime::jsonl_io::{JsonlChangeKind, JsonlIoAccounting};

/// Why strict JSONL framing stopped before consuming the next record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFrameDeferral {
    Partial {
        offset: u64,
    },
    Malformed {
        offset: u64,
    },
    Backlog {
        offset: u64,
        unread_bytes: u64,
        max_new_bytes: u64,
    },
}

impl JsonlFrameDeferral {
    pub fn offset(self) -> u64 {
        match self {
            Self::Partial { offset }
            | Self::Malformed { offset }
            | Self::Backlog { offset, .. } => offset,
        }
    }

    pub fn reason_code(self) -> &'static str {
        match self {
            Self::Partial { .. } => "partial_jsonl_frame",
            Self::Malformed { .. } => "malformed_jsonl_frame",
            Self::Backlog { .. } => "jsonl_backlog_limit",
        }
    }
}

pub const MAX_JSONL_RECORD_BYTES: usize = 16 * 1024 * 1024;
/// Default strict-scan budget keeps recovery bounded even without a hook cap.
pub const STRICT_JSONL_BATCH_BYTES: u64 = 2 * 1024 * 1024;
pub(in crate::runtime) const MAX_JSONL_FRAMES_PER_BATCH: usize = 4096;
const JSONL_HASH_CHUNK_BYTES: usize = 64 * 1024;
const UNCHANGED_GENERATION_CACHE_CAP: usize = 4096;

/// Process-local proof that one exact durable checkpoint is still the prefix of
/// an unchanged file: at EOF it settles a repoll, short of it the next batch
/// resumes from its digest instead of rehashing the prefix.
///
/// Entries are minted only after revalidation succeeds. A miss or any native
/// identity, size, high-resolution token, or checkpoint mismatch falls back to
/// byte-exact prefix validation; this cache never becomes a durable authority.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct UnchangedGenerationCacheKey {
    native_identity: JsonlNativeFileIdentity,
    size: u64,
    change: JsonlFileChangeToken,
    position: u64,
    generation: u64,
    stable_file_identity: u64,
    fingerprint: u64,
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::runtime) struct JsonlNativeFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
pub(in crate::runtime) fn jsonl_native_file_identity(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Option<JsonlNativeFileIdentity> {
    Some(JsonlNativeFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::runtime) struct JsonlNativeFileIdentity {
    volume_serial_number: u32,
    file_index: u64,
}

#[cfg(windows)]
pub(in crate::runtime) fn jsonl_native_file_identity(
    file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Option<JsonlNativeFileIdentity> {
    let information = tracedecay_private_fs::windows_file::information(file).ok()?;
    Some(JsonlNativeFileIdentity {
        volume_serial_number: information.volume_serial_number,
        file_index: information.file_index,
    })
}

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::runtime) struct JsonlNativeFileIdentity {
    created_nanos: u128,
}

#[cfg(not(any(unix, windows)))]
pub(in crate::runtime) fn jsonl_native_file_identity(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Option<JsonlNativeFileIdentity> {
    let created_nanos = metadata
        .created()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(JsonlNativeFileIdentity { created_nanos })
}

/// One stat observation of a file's modification and rewrite-witness times.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::runtime) struct JsonlFileChangeToken {
    modified_nanos: Option<u128>,
    /// The platform [`RewriteWitness`]'s change time; `None` where no stat
    /// field witnesses a rewrite, so the token can disprove one but never
    /// prove the bytes unchanged.
    changed_nanos: Option<i128>,
}

pub(in crate::runtime) fn jsonl_file_change_token_under(
    metadata: &std::fs::Metadata,
    witness: RewriteWitness,
) -> JsonlFileChangeToken {
    JsonlFileChangeToken {
        modified_nanos: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos()),
        changed_nanos: witness.change_time_nanos(metadata),
    }
}

impl JsonlFileChangeToken {
    /// The data-modification half of the token.
    ///
    /// The whole token also carries ctime, which moves for metadata-only
    /// operations: a rename bumps it while every byte stays put. Only mtime
    /// answers "were this file's contents written", so the two halves are
    /// asked separately, ctime is enough to suspect a change, mtime is what
    /// proves one.
    fn data_stamp(self) -> Option<u128> {
        self.modified_nanos
    }

    /// Whether an equal later token proves the file's bytes unchanged.
    pub(in crate::runtime) fn witnesses_rewrites(self) -> bool {
        self.changed_nanos.is_some()
    }
}

/// Whether `token` is old enough that a later write must move it.
///
/// Linux inode timestamps come from the coarse realtime clock. A token still
/// inside that quantum can be shared with a same-length rewrite, so it is not
/// proof the bytes are unchanged. Callers that would skip a content check
/// must refuse the skip until the token falls behind the clock. A token
/// without a rewrite witness never is: no later write has to move it.
pub(in crate::runtime) fn jsonl_change_token_settled(token: JsonlFileChangeToken) -> bool {
    token
        .changed_nanos
        .is_some_and(tracedecay_private_fs::change_time_settled)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct CacheAdmission<N> {
    priority: u64,
    native_identity: N,
}

impl<N: Ord> PartialOrd for CacheAdmission<N> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<N: Ord> Ord for CacheAdmission<N> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        #[cfg(test)]
        CACHE_HEAP_COMPARISONS.with(|count| count.set(count.get().saturating_add(1)));
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.native_identity.cmp(&other.native_identity))
    }
}

#[cfg(test)]
std::thread_local! {
    static CACHE_HEAP_COMPARISONS: Cell<usize> = const { Cell::new(0) };
}

struct BoundedLatestProofCache<N, P> {
    capacity: usize,
    entries: HashMap<N, P>,
    admissions: BinaryHeap<CacheAdmission<N>>,
}

impl<N: Copy + Eq + Ord + Hash, P> BoundedLatestProofCache<N, P> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::with_capacity(capacity),
            admissions: BinaryHeap::with_capacity(capacity),
        }
    }

    fn get(&self, native_identity: &N) -> Option<&P> {
        self.entries.get(native_identity)
    }

    #[cfg(test)]
    fn contains(&self, native_identity: &N, proof: &P) -> bool
    where
        P: PartialEq,
    {
        self.get(native_identity) == Some(proof)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn admission_len(&self) -> usize {
        self.admissions.len()
    }

    fn insert(&mut self, native_identity: N, proof: P) {
        if self.capacity == 0 {
            return;
        }
        if let Some(current) = self.entries.get_mut(&native_identity) {
            *current = proof;
            return;
        }
        let admission = CacheAdmission {
            priority: stable_cache_priority(&native_identity),
            native_identity,
        };
        if self.entries.len() < self.capacity {
            self.entries.insert(native_identity, proof);
            self.admissions.push(admission);
            return;
        }
        let Some(highest_admission) = self.admissions.peek() else {
            return;
        };
        if admission >= *highest_admission {
            return;
        }
        let Some(evicted) = self.admissions.pop() else {
            return;
        };
        self.entries.remove(&evicted.native_identity);
        self.entries.insert(native_identity, proof);
        self.admissions.push(admission);
    }
}

fn stable_cache_priority(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// An [`UnchangedGenerationCacheKey`] with what resuming at its position needs
/// and the key cannot carry.
#[derive(Clone)]
struct UnchangedGenerationProof {
    key: UnchangedGenerationCacheKey,
    /// Identity window hash of the file the proving scan read, which differs
    /// from `key.stable_file_identity` once a replaced file resumed its
    /// recorded generation.
    physical_identity: u64,
    /// Digest of `[0, key.position)`.
    digest: ResumeDigest,
    /// More than one cursor resumed this exact checkpoint. One of them can
    /// advance without taking the proof from the cursor that is still here.
    shared: bool,
}

impl UnchangedGenerationProof {
    /// Proofs of the same physical file version. Each scope can resume it
    /// under its own generation, so the generation does not tell versions
    /// apart and must not evict another scope's proof.
    fn same_cached_file(&self, other: &Self) -> bool {
        other.key.size == self.key.size
            && other.key.change == self.key.change
            && other.physical_identity == self.physical_identity
    }
}

/// Cursors that can resume one file without rehashing its prefix. A project
/// scope and the profile scope each catch the same rollout up under their own
/// cursor. A batch moves the proof it alone resumed from; a checkpoint two
/// cursors share stays until the last of them advances.
///
/// ponytail: a file read under more cursors than this rehashes on eviction;
/// key proofs by scope if that shows up.
const UNCHANGED_GENERATION_PROOFS_PER_FILE: usize = 2;

type UnchangedGenerationCache =
    BoundedLatestProofCache<JsonlNativeFileIdentity, Vec<UnchangedGenerationProof>>;

fn unchanged_generation_cache() -> &'static Mutex<UnchangedGenerationCache> {
    static CACHE: std::sync::OnceLock<Mutex<UnchangedGenerationCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BoundedLatestProofCache::new(UNCHANGED_GENERATION_CACHE_CAP)))
}

/// Tests keep the process-global cache off except under the directories a
/// [`HoldUnchangedGenerationCache`] names, so one test's proof never settles
/// another test's scan. Keyed by path rather than by thread: admission scans
/// run on blocking-pool threads, not on the test thread holding the cache.
#[cfg(test)]
fn held_unchanged_generation_roots() -> std::sync::MutexGuard<'static, Vec<std::path::PathBuf>> {
    static HELD: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
fn unchanged_generation_cache_serves(path: &Path) -> bool {
    held_unchanged_generation_roots()
        .iter()
        .any(|root| path.starts_with(root))
}

#[cfg(not(test))]
fn unchanged_generation_cache_serves(_path: &Path) -> bool {
    true
}

#[cfg(test)]
pub(in crate::runtime) struct HoldUnchangedGenerationCache {
    root: std::path::PathBuf,
}

#[cfg(test)]
impl HoldUnchangedGenerationCache {
    pub(in crate::runtime) fn enter(root: &Path) -> Self {
        held_unchanged_generation_roots().push(root.to_path_buf());
        Self {
            root: root.to_path_buf(),
        }
    }
}

/// Block until `path`'s change time is strictly older than the kernel clock
/// that stamps it. Tests that lock the settled zero-read path use this
/// instead of sleeping for a fixed budget: the condition is the clock
/// quantum itself.
#[cfg(test)]
pub(in crate::runtime) fn spin_until_jsonl_change_settled(path: &Path) {
    for _ in 0..10_000_000u32 {
        let Ok(file) = std::fs::File::open(path) else {
            std::thread::yield_now();
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            std::thread::yield_now();
            continue;
        };
        let witness = RewriteWitness::NATIVE;
        if !witness.proves_unchanged_bytes() || witness.vouches_for_unchanged_bytes(&metadata) {
            return;
        }
        std::thread::yield_now();
    }
    panic!("filesystem change clock stayed inside one timestamp quantum");
}

#[cfg(test)]
impl Drop for HoldUnchangedGenerationCache {
    fn drop(&mut self) {
        held_unchanged_generation_roots().retain(|root| root != &self.root);
    }
}

fn cached_unchanged_generation(
    path: &Path,
    key: UnchangedGenerationCacheKey,
) -> Option<UnchangedGenerationProof> {
    if !unchanged_generation_cache_serves(path) {
        return None;
    }
    let cache = unchanged_generation_cache().lock().ok()?;
    cache
        .get(&key.native_identity)?
        .iter()
        .find(|proof| proof.key == key)
        .cloned()
}

fn remember_unchanged_generation_if_settled(
    path: &Path,
    proof: UnchangedGenerationProof,
    resumed_position: u64,
) {
    if !unchanged_generation_cache_serves(path) {
        return;
    }
    // A proof taken while the change time is still inside the coarse quantum
    // can share that token with a later same-length rewrite. Remembering it
    // would let the settled repoll skip the bytes. Record the cache only once
    // the token can no longer be shared.
    if !jsonl_change_token_settled(proof.key.change) {
        return;
    }
    let Ok(mut cache) = unchanged_generation_cache().lock() else {
        return;
    };
    let native_identity = proof.key.native_identity;
    let mut proofs = cache.get(&native_identity).cloned().unwrap_or_default();
    proofs.retain(|kept| proof.same_cached_file(kept));
    // A cursor that was the only one at `resumed_position` takes that slot
    // with it. A shared checkpoint stays for the cursor that has not moved.
    // Another generation's proof at the same offset belongs to a scope that
    // has not moved. On one file version the prefix digest is fixed by the
    // position, so these fields single out the moving cursor's own proof.
    if resumed_position != proof.key.position {
        let is_source = |kept: &UnchangedGenerationProof| {
            kept.key.position == resumed_position
                && kept.key.generation == proof.key.generation
                && kept.key.stable_file_identity == proof.key.stable_file_identity
        };
        if let Some(index) = proofs.iter().position(is_source) {
            if proofs[index].shared {
                proofs[index].shared = false;
            } else {
                proofs.remove(index);
            }
        }
    }
    if let Some(index) = proofs.iter().position(|kept| kept.key == proof.key) {
        let mut existing = proofs.remove(index);
        existing.digest = proof.digest;
        existing.physical_identity = proof.physical_identity;
        // Arriving from a different cursor, not refreshing this same one.
        if resumed_position != proof.key.position {
            existing.shared = true;
        }
        proofs.insert(0, existing);
    } else {
        proofs.insert(
            0,
            UnchangedGenerationProof {
                shared: false,
                ..proof
            },
        );
    }
    proofs.truncate(UNCHANGED_GENERATION_PROOFS_PER_FILE);
    cache.insert(native_identity, proofs);
}

/// A cache entry needs a change token whose equality proves the bytes
/// unchanged; without a rewrite witness callers re-verify content instead.
fn unchanged_generation_cache_key(
    file: &std::fs::File,
    metadata: &std::fs::Metadata,
    change: JsonlFileChangeToken,
    previous: StoredCursor,
    resume: JsonlResumeState,
) -> Option<UnchangedGenerationCacheKey> {
    if previous.position == 0
        || previous.position > metadata.len()
        || previous.file_id != resume.generation
    {
        return None;
    }
    Some(UnchangedGenerationCacheKey {
        native_identity: jsonl_native_file_identity(file, metadata)?,
        size: metadata.len(),
        change: Some(change).filter(|change| change.witnesses_rewrites())?,
        position: previous.position,
        generation: resume.generation,
        stable_file_identity: resume.file_identity,
        fingerprint: resume.fingerprint,
    })
}

struct ScanPayloadMeter(Cell<u64>);

impl ScanPayloadMeter {
    fn new() -> Self {
        Self(Cell::new(0))
    }

    fn get(&self) -> u64 {
        self.0.get()
    }
}

struct MeasuredJsonlFile<'a> {
    inner: std::fs::File,
    meter: &'a ScanPayloadMeter,
}

impl<'a> MeasuredJsonlFile<'a> {
    fn new(inner: std::fs::File, meter: &'a ScanPayloadMeter) -> Self {
        Self { inner, meter }
    }

    fn inner(&self) -> &std::fs::File {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut std::fs::File {
        &mut self.inner
    }
}

impl Read for MeasuredJsonlFile<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;

        self.meter
            .0
            .set(self.meter.0.get().saturating_add(read as u64));
        Ok(read)
    }
}

impl Seek for MeasuredJsonlFile<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(position)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlResumeState {
    pub generation: u64,
    pub file_identity: u64,
    pub fingerprint: u64,
}

/// Record-end prefix checkpoint that one generation of a source committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JsonlPrefixCheckpoint {
    pub generation: u64,
    pub position: u64,
    pub fingerprint: u64,
}

/// What a scan does when the recorded resume prefix no longer matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JsonlPrefixRecovery {
    /// Stop before reading content and report the divergence, so the caller
    /// can load the source's committed checkpoints.
    Report,
    /// Resume the rewritten generation after the longest prefix that still
    /// hashes to one of these checkpoints; none matching rescans from zero.
    Checkpoints(Arc<[JsonlPrefixCheckpoint]>),
}

impl JsonlPrefixRecovery {
    pub fn rescan() -> Self {
        Self::Checkpoints(Arc::from([]))
    }

    /// Checkpoints carried by a source's committed byte-ordered cursors.
    pub fn committed(cursors: &[ObservationSourceCursorV1]) -> Self {
        Self::Checkpoints(
            cursors
                .iter()
                .filter(|cursor| cursor.ordering_domain() == ObservationOrderingDomainV1::FileBytes)
                .filter_map(|cursor| {
                    Some(JsonlPrefixCheckpoint {
                        generation: cursor.generation().generation_id(),
                        position: cursor.position(),
                        fingerprint: cursor.resume_fingerprint()?,
                    })
                })
                .collect(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawJsonlFrame {
    Eof,
    Complete { byte_len: u64 },
    Partial { byte_len: u64 },
    Oversized { byte_len: u64, terminated: bool },
    BudgetExhausted { byte_len: u64, oversized: bool },
}

impl RawJsonlFrame {
    fn byte_len(self) -> u64 {
        match self {
            Self::Eof => 0,
            Self::Complete { byte_len }
            | Self::Partial { byte_len }
            | Self::Oversized { byte_len, .. }
            | Self::BudgetExhausted { byte_len, .. } => byte_len,
        }
    }
}

#[derive(Clone)]
pub(in crate::runtime) struct ResumeDigest {
    hasher: Sha256,
}

impl ResumeDigest {
    pub(in crate::runtime) fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay-jsonl-resume-prefix-v2");
        Self { hasher }
    }

    fn extend(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }

    /// 64-bit check of the hashed prefix plus `position`. This is not SHA-256
    /// mid-state: the domain cursor (`StoredCursor` + optional resume
    /// fingerprint) has no field that can carry hasher bytes without
    /// displacing `file_id` / generation, which would collapse rewrite and
    /// file-identity detection. A first append after a durable resume must
    /// therefore re-walk `[0, cursor)` to rebuild this digest.
    fn fingerprint(&self, position: u64) -> u64 {
        let mut hasher = self.hasher.clone();
        hasher.update(position.to_le_bytes());
        digest_prefix_u64(hasher.finalize())
    }

    pub(in crate::runtime) fn witness(&self, verified_position: u64) -> [u8; 32] {
        let mut hasher = self.hasher.clone();
        hasher.update(verified_position.to_le_bytes());
        hasher.finalize().into()
    }
}

fn digest_prefix_u64(digest: sha2::digest::Output<Sha256>) -> u64 {
    let [
        first,
        second,
        third,
        fourth,
        fifth,
        sixth,
        seventh,
        eighth,
        ..,
    ] = <[u8; 32]>::from(digest);
    u64::from_be_bytes([first, second, third, fourth, fifth, sixth, seventh, eighth])
}

pub(in crate::runtime) fn jsonl_prefix_digest<R: Read + Seek>(
    file: &mut R,
    extent: u64,
) -> std::io::Result<(ResumeDigest, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = extent;
    let mut hashed = 0_u64;
    let mut buffer = vec![0_u8; JSONL_HASH_CHUNK_BYTES];
    let mut digest = ResumeDigest::new();
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "JSONL prefix ended during generation hashing",
            ));
        }
        digest.extend(&buffer[..read]);
        hashed = hashed.saturating_add(read as u64);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok((digest, hashed))
}

/// Longest checkpointed prefix of `file` whose digest still matches, or
/// `recorded` itself as soon as it matches, since resuming the recorded
/// generation needs no replacement.
///
/// Checkpoints of one generation share one byte stream, so its first mismatch
/// rules out every later checkpoint of that generation; hashing stops once
/// every generation has diverged instead of walking the whole extent.
fn longest_retained_jsonl_prefix<R: Read + Seek>(
    file: &mut R,
    checkpoints: &[JsonlPrefixCheckpoint],
    recorded: Option<JsonlPrefixCheckpoint>,
    extent: u64,
) -> std::io::Result<(Option<(JsonlPrefixCheckpoint, ResumeDigest)>, u64)> {
    let mut ordered = checkpoints
        .iter()
        .copied()
        .chain(recorded)
        .filter(|checkpoint| checkpoint.position > 0 && checkpoint.position <= extent)
        .collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|checkpoint| {
        (
            checkpoint.position,
            checkpoint.generation,
            checkpoint.fingerprint,
        )
    });
    ordered.dedup();
    let generations = ordered
        .iter()
        .map(|checkpoint| checkpoint.generation)
        .collect::<HashSet<_>>();
    let mut diverged = HashSet::new();
    let mut digest = ResumeDigest::new();
    let mut hashed = 0_u64;
    let mut retained = None;
    let mut buffer = vec![0_u8; JSONL_HASH_CHUNK_BYTES];
    file.seek(SeekFrom::Start(0))?;
    for checkpoint in ordered {
        if diverged.len() == generations.len() {
            break;
        }
        if diverged.contains(&checkpoint.generation) {
            continue;
        }
        while hashed < checkpoint.position {
            let requested =
                usize::try_from((checkpoint.position - hashed).min(buffer.len() as u64))
                    .unwrap_or(buffer.len());
            let read = file.read(&mut buffer[..requested])?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "JSONL prefix ended during checkpoint hashing",
                ));
            }
            digest.extend(&buffer[..read]);
            hashed = hashed.saturating_add(read as u64);
        }
        if digest.fingerprint(checkpoint.position) == checkpoint.fingerprint {
            if Some(checkpoint) == recorded {
                return Ok((Some((checkpoint, digest)), hashed));
            }
            retained = Some((checkpoint, digest.clone()));
        } else {
            diverged.insert(checkpoint.generation);
        }
    }
    Ok((retained, hashed))
}

enum RecordedPrefix {
    /// The recorded cursor still resumes; the digest seeds the scanner when
    /// this pass computed it.
    Resumes(Option<ResumeDigest>),
    /// The recorded prefix changed; carries the longest committed checkpoint
    /// that still matches when checkpoints were supplied.
    Diverged(Option<(JsonlPrefixCheckpoint, ResumeDigest)>),
}

fn match_recorded_prefix(
    file: &mut MeasuredJsonlFile<'_>,
    path: &Path,
    recorded: Option<JsonlPrefixCheckpoint>,
    prefix_recovery: &JsonlPrefixRecovery,
    extent: u64,
    io: &mut JsonlIoAccounting,
) -> TranscriptIngestResult<RecordedPrefix> {
    match (recorded, prefix_recovery) {
        (None, JsonlPrefixRecovery::Report) => Ok(RecordedPrefix::Diverged(None)),
        (Some(recorded), JsonlPrefixRecovery::Report) => {
            match jsonl_prefix_digest(file, recorded.position) {
                Ok((digest, hashed)) => {
                    io.prefix_validation_bytes = io.prefix_validation_bytes.saturating_add(hashed);
                    if digest.fingerprint(recorded.position) == recorded.fingerprint {
                        Ok(RecordedPrefix::Resumes(Some(digest)))
                    } else {
                        Ok(RecordedPrefix::Diverged(None))
                    }
                }
                Err(_) => Ok(RecordedPrefix::Diverged(None)),
            }
        }
        (_, JsonlPrefixRecovery::Checkpoints(checkpoints)) => {
            let (retained, hashed) =
                longest_retained_jsonl_prefix(file, checkpoints, recorded, extent)
                    .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
            io.prefix_validation_bytes = io.prefix_validation_bytes.saturating_add(hashed);
            Ok(match retained {
                Some((checkpoint, digest)) if Some(checkpoint) == recorded => {
                    RecordedPrefix::Resumes(Some(digest))
                }
                other => RecordedPrefix::Diverged(other),
            })
        }
    }
}

/// Memoized [`bounded_jsonl_snapshot_fingerprint`]: the hash walks the whole
/// extent, so callers compute it at most once per scan and only on paths that
/// actually consume it.
fn memoized_jsonl_snapshot_fingerprint(
    cache: &mut Option<u64>,
    hashed_bytes: &mut u64,
    file: &mut MeasuredJsonlFile<'_>,
    extent: u64,
) -> std::io::Result<u64> {
    if let Some(fingerprint) = *cache {
        return Ok(fingerprint);
    }
    let (fingerprint, hashed) = bounded_jsonl_snapshot_fingerprint(file, extent)?;
    *cache = Some(fingerprint);
    *hashed_bytes = hashed_bytes.saturating_add(hashed);
    Ok(fingerprint)
}

fn bounded_jsonl_snapshot_fingerprint(
    file: &mut MeasuredJsonlFile<'_>,
    extent: u64,
) -> std::io::Result<(u64, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-snapshot-v2");
    hasher.update(extent.to_le_bytes());
    let mut remaining = extent;
    let mut hashed = 0_u64;
    let mut buffer = vec![0_u8; JSONL_HASH_CHUNK_BYTES];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "JSONL snapshot ended during generation hashing",
            ));
        }
        hasher.update(&buffer[..read]);
        hashed = hashed.saturating_add(read as u64);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok((digest_prefix_u64(hasher.finalize()), hashed))
}

/// Chain depth for replacement markers derived from one file identity.
///
/// The marker for a rewritten file must be recoverable from the persisted
/// cursor alone (there is no room for a separate generation column), so it is
/// re-derived by searching this bounded counter space. Successive rewrites of a
/// file whose identity never changes therefore stay distinct until the counter
/// wraps.
const MAX_JSONL_REPLACEMENT_GENERATIONS: u32 = 64;

/// Deterministic replacement marker for `counter`-th rewrite of `file_identity`.
///
/// Never zero: the resume check reads zero as "identity unknown".
fn replacement_jsonl_generation(file_identity: u64, counter: u32) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-replacement-generation-v1");
    hasher.update(file_identity.to_le_bytes());
    hasher.update(counter.to_le_bytes());
    digest_prefix_u64(hasher.finalize()).max(1)
}

fn replacement_generation_counter(file_identity: u64, generation: u64) -> Option<u32> {
    if generation == 0 || generation == file_identity {
        return None;
    }
    (1..=MAX_JSONL_REPLACEMENT_GENERATIONS)
        .find(|counter| replacement_jsonl_generation(file_identity, *counter) == generation)
}

/// Whether `generation` was minted for a rewritten generation of this file.
pub(super) fn is_replacement_jsonl_generation(file_identity: u64, generation: u64) -> bool {
    replacement_generation_counter(file_identity, generation).is_some()
}

/// Marker for the generation that replaces the one `previous_generation` names.
fn next_replacement_jsonl_generation(file_identity: u64, previous_generation: u64) -> u64 {
    let counter = replacement_generation_counter(file_identity, previous_generation)
        .map_or(1, |counter| counter % MAX_JSONL_REPLACEMENT_GENERATIONS + 1);
    replacement_jsonl_generation(file_identity, counter)
}

/// `content_fingerprint` is the whole-extent snapshot for a rescan from zero,
/// or the retained prefix's resume fingerprint for a proportional resume.
fn rewritten_jsonl_generation(
    previous: JsonlResumeState,
    file_identity: u64,
    content_fingerprint: u64,
    file_size: u64,
    mtime: u64,
) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-rewrite-generation-v1");
    hasher.update(previous.generation.to_le_bytes());
    hasher.update(file_identity.to_le_bytes());
    hasher.update(content_fingerprint.to_le_bytes());
    hasher.update(file_size.to_le_bytes());
    hasher.update(mtime.to_le_bytes());
    digest_prefix_u64(hasher.finalize()).max(1)
}

/// Bounded raw JSONL framing shared by skip and defer policies.
///
/// The retained record buffer never exceeds `max_record_bytes`. Oversized
/// frames are drained in-place so legacy callers can skip complete records and
/// continue without allocating or parsing them.
pub struct RawJsonlFrameReader<R> {
    reader: R,
    record: Vec<u8>,
    resume_digest: ResumeDigest,
    max_record_bytes: usize,
}

impl<R: BufRead> RawJsonlFrameReader<R> {
    pub fn new(reader: R, max_record_bytes: usize) -> Self {
        Self {
            reader,
            record: Vec::new(),
            resume_digest: ResumeDigest::new(),
            max_record_bytes,
        }
    }

    pub(in crate::runtime) fn seed_resume_digest(&mut self, digest: ResumeDigest) {
        self.resume_digest = digest;
    }

    pub(in crate::runtime) fn resume_digest(&self) -> ResumeDigest {
        self.resume_digest.clone()
    }

    fn resume_fingerprint(&self, position: u64) -> u64 {
        self.resume_digest.fingerprint(position)
    }

    pub fn record(&self) -> &[u8] {
        &self.record
    }

    pub fn set_max_record_bytes(&mut self, max_record_bytes: usize) {
        self.max_record_bytes = max_record_bytes;
    }

    pub fn into_inner(self) -> R {
        self.reader
    }

    pub fn next_frame(&mut self) -> std::io::Result<RawJsonlFrame> {
        self.next_frame_with_budget(
            u64::try_from(self.max_record_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
    }

    pub fn next_frame_with_budget(&mut self, read_budget: u64) -> std::io::Result<RawJsonlFrame> {
        self.record.clear();
        let mut byte_len = 0_u64;
        let mut oversized = false;

        loop {
            if byte_len >= read_budget {
                return Ok(RawJsonlFrame::BudgetExhausted {
                    byte_len,
                    oversized,
                });
            }
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                return Ok(if byte_len == 0 {
                    RawJsonlFrame::Eof
                } else if oversized {
                    RawJsonlFrame::Oversized {
                        byte_len,
                        terminated: false,
                    }
                } else {
                    RawJsonlFrame::Partial { byte_len }
                });
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let available_record = newline.map_or(available.len(), |index| index + 1);
            let remaining =
                usize::try_from(read_budget.saturating_sub(byte_len)).unwrap_or(usize::MAX);
            let consumed = available_record.min(remaining);
            if !oversized {
                let retained =
                    consumed.min(self.max_record_bytes.saturating_sub(self.record.len()));
                self.record.extend_from_slice(&available[..retained]);
                oversized = retained < consumed;
            }
            self.resume_digest.extend(&available[..consumed]);
            self.reader.consume(consumed);
            byte_len = byte_len.saturating_add(consumed as u64);

            if newline.is_some_and(|index| index < consumed) {
                return Ok(if oversized {
                    RawJsonlFrame::Oversized {
                        byte_len,
                        terminated: true,
                    }
                } else {
                    RawJsonlFrame::Complete { byte_len }
                });
            }
            if consumed < available_record {
                return Ok(RawJsonlFrame::BudgetExhausted {
                    byte_len,
                    oversized,
                });
            }
        }
    }
}

#[derive(Clone)]
struct RawJsonlScanRequest {
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
    max_frames: usize,
    max_record_bytes: usize,
    resume_state: Option<JsonlResumeState>,
    prefix_recovery: JsonlPrefixRecovery,
    witness: RewriteWitness,
}

/// One bounded, complete raw JSONL frame with its exact source byte range.
pub struct RawJsonlRecord {
    pub offset: u64,
    pub end_offset: u64,
    pub resume_fingerprint: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawJsonlSkippedReason {
    Whitespace,
    Oversized,
    /// Leading bytes of a rewritten generation that match a committed
    /// checkpoint; their records stay under the generation that admitted them.
    RetainedPrefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawJsonlSkippedRange {
    pub offset: u64,
    pub end_offset: u64,
    pub resume_fingerprint: u64,
    pub reason: RawJsonlSkippedReason,
}

/// Raw framing result. No JSON parser has inspected these bytes.
pub struct RawNewJsonl {
    pub frames: Vec<RawJsonlRecord>,
    pub skipped: Vec<RawJsonlSkippedRange>,
    pub start_offset: u64,
    /// Furthest absolute source position inspected, including a partial frame
    /// that cannot advance the durable cursor.
    pub read_through: u64,
    pub file_identity: u64,
    pub new_cursor: StoredCursor,
    /// Whether `new_cursor.file_id` names a replacement generation rather than
    /// the append-only identity of the scanned file.
    pub replacement_generation: bool,
    pub deferred: Option<JsonlFrameDeferral>,
    /// Set only under [`JsonlPrefixRecovery::Report`]: the recorded prefix no
    /// longer matches and nothing past validation was read.
    pub prefix_diverged: bool,
    pub io: JsonlIoAccounting,
}

impl RawNewJsonl {
    /// Nothing read past validation: the cursor stays where it was.
    fn prefix_diverged(previous: StoredCursor, file_identity: u64, io: JsonlIoAccounting) -> Self {
        Self {
            frames: Vec::new(),
            skipped: Vec::new(),
            start_offset: previous.position,
            read_through: previous.position,
            file_identity,
            new_cursor: previous,
            replacement_generation: false,
            deferred: None,
            prefix_diverged: true,
            io,
        }
    }
}

/// Strict bounded framing used by Claude's single-parse privacy boundary.
#[cfg(test)]
pub fn stream_new_jsonl_raw_strict(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
) -> Option<RawNewJsonl> {
    match try_stream_new_jsonl_raw_strict(path, prev, max_new_bytes, max_record_bytes) {
        Ok(raw) => Some(raw),
        Err(error) => {
            super::log_source_skip(path, "scan strict jsonl transcript", &error);
            None
        }
    }
}

#[cfg(test)]
pub fn try_stream_new_jsonl_raw_strict(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
) -> TranscriptIngestResult<RawNewJsonl> {
    try_stream_new_jsonl_raw_strict_with_resume(path, prev, max_new_bytes, max_record_bytes, None)
}

#[cfg(test)]
pub fn try_stream_new_jsonl_raw_strict_with_resume(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
    resume_state: Option<JsonlResumeState>,
) -> TranscriptIngestResult<RawNewJsonl> {
    try_stream_new_jsonl_raw_strict_with_resume_and_frame_limit(
        path,
        prev,
        max_new_bytes,
        max_record_bytes,
        resume_state,
        JsonlPrefixRecovery::rescan(),
        MAX_JSONL_FRAMES_PER_BATCH,
    )
}

pub(in crate::runtime) fn try_stream_new_jsonl_raw_strict_with_resume_and_frame_limit(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
    resume_state: Option<JsonlResumeState>,
    prefix_recovery: JsonlPrefixRecovery,
    max_frames: usize,
) -> TranscriptIngestResult<RawNewJsonl> {
    let one_record_bytes = u64::try_from(max_record_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let recovery_batch_bytes = STRICT_JSONL_BATCH_BYTES.max(one_record_bytes);
    try_stream_new_jsonl_raw_with_frame_limit(
        path,
        prev,
        Some(max_new_bytes.unwrap_or(recovery_batch_bytes)),
        max_record_bytes,
        resume_state,
        prefix_recovery,
        max_frames,
    )
}

fn try_stream_new_jsonl_raw_with_frame_limit(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
    resume_state: Option<JsonlResumeState>,
    prefix_recovery: JsonlPrefixRecovery,
    max_frames: usize,
) -> TranscriptIngestResult<RawNewJsonl> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return Err(TranscriptIngestError::scan_io("open", path, error)),
    };
    try_stream_new_jsonl_raw_from_file(
        path,
        file,
        RawJsonlScanRequest {
            previous: prev,
            max_new_bytes,
            max_frames: max_frames.clamp(1, MAX_JSONL_FRAMES_PER_BATCH),
            max_record_bytes,
            resume_state,
            prefix_recovery,
            witness: RewriteWitness::NATIVE,
        },
        || {},
    )
}

#[derive(Clone, Copy)]
struct JsonlScanGeneration {
    file_size: u64,
    mtime: u64,
    /// High-resolution platform change token captured with the opening
    /// `fstat`. Unix includes ctime as well as mtime so restoring an old mtime
    /// cannot hide an in-place rewrite. A metadata-only change may cause one
    /// conservative retry; admitting mixed bytes is the worse outcome.
    change: JsonlFileChangeToken,
    witness: RewriteWitness,
    file_id: u64,
    file_identity: u64,
    /// Identity of the file this scan opened. It differs from `file_identity`
    /// only when a replaced file resumed its recorded generation, and it is
    /// what revalidation compares the file against after the read.
    physical_identity: u64,
    /// `None` only when the scan proved it would read nothing, so no batch,
    /// and therefore no revalidation, consumes it.
    snapshot_fingerprint: Option<u64>,
    seek_to: u64,
    replacement: bool,
}

struct PreparedJsonlScan<'a> {
    file: MeasuredJsonlFile<'a>,
    generation: JsonlScanGeneration,
    cached_unchanged: Option<UnchangedGenerationCacheKey>,
    /// Prefix digest already computed while validating the resume checkpoint,
    /// with the exact extent it covers. `RawJsonlBatchScanner::start` needs the
    /// same digest to seed the reader, so carrying it forward keeps one scan to
    /// one pass over the prefix instead of hashing those bytes a second time.
    validated_prefix: Option<(u64, ResumeDigest)>,
    /// `[0, seek_to)` of a rewritten generation proven equal to a committed
    /// checkpoint. The first batch covers it before any new record.
    retained_prefix: Option<RawJsonlSkippedRange>,
}

enum JsonlCapture<'a> {
    Scan(Box<PreparedJsonlScan<'a>>),
    PrefixDiverged { file_identity: u64 },
}

impl<'a> PreparedJsonlScan<'a> {
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is an independent scan input chosen per call"
    )]
    fn capture(
        path: &Path,
        mut file: MeasuredJsonlFile<'a>,
        previous: StoredCursor,
        resume_state: Option<JsonlResumeState>,
        prefix_recovery: &JsonlPrefixRecovery,
        witness: RewriteWitness,
        after_generation_capture: impl FnOnce(),
        io: &mut JsonlIoAccounting,
    ) -> TranscriptIngestResult<JsonlCapture<'a>> {
        let metadata = file
            .inner()
            .metadata()
            .map_err(|error| TranscriptIngestError::scan_io("fstat", path, error))?;
        let file_size = metadata.len();
        let mtime = file_mtime_secs(&metadata);
        let change = jsonl_file_change_token_under(&metadata, witness);
        let cached_proof = resume_state
            .and_then(|resume| {
                unchanged_generation_cache_key(file.inner(), &metadata, change, previous, resume)
            })
            .filter(|key| jsonl_change_token_settled(key.change))
            .and_then(|key| cached_unchanged_generation(path, key));
        let cached_unchanged = cached_proof.as_ref().map(|proof| proof.key);
        let (physical_identity, identity_window_bytes) = if let Some(proof) = &cached_proof {
            (proof.physical_identity, 0)
        } else {
            let (identity, read) = stable_jsonl_file_id(file.inner_mut(), &metadata)
                .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
            (identity, read)
        };
        let mut file_identity = physical_identity;
        io.identity_window_bytes = identity_window_bytes;
        // The snapshot fingerprint hashes the whole extent, so it is captured
        // lazily: only rewrite-marker minting and scans that will actually
        // read bytes pay for it. A no-change poll whose cursor already sits
        // at end-of-file skips the hash entirely.
        let mut snapshot_fingerprint = None;
        // Retains the digest computed below so the scanner can seed its reader
        // from it instead of walking the same prefix a second time.
        let mut validated_prefix: Option<(u64, ResumeDigest)> = None;
        let mut retained_prefix = None;
        let (seek_to, file_id) = if let Some(resume_state) = resume_state {
            // The consumed prefix digest, not the physical file identity, proves
            // a resume: a transcript replaced by rename keeps its generation and
            // recorded identity when its first `position` bytes are unchanged.
            let recorded = (previous.position > 0
                && previous.file_id == resume_state.generation
                && file_size >= previous.position)
                .then_some(JsonlPrefixCheckpoint {
                    generation: resume_state.generation,
                    position: previous.position,
                    fingerprint: resume_state.fingerprint,
                });
            // Under `Checkpoints` the recorded cursor is one more candidate of
            // a single forward walk that stops past the first changed record,
            // rather than a whole-prefix hash that can only say "changed".
            let (resume_matches, recovered) =
                if let Some(proof) = cached_proof.filter(|_| recorded.is_some()) {
                    validated_prefix = Some((previous.position, proof.digest));
                    (true, None)
                } else {
                    match match_recorded_prefix(
                        &mut file,
                        path,
                        recorded,
                        prefix_recovery,
                        file_size,
                        io,
                    )? {
                        RecordedPrefix::Resumes(digest) => {
                            validated_prefix = digest.map(|digest| (previous.position, digest));
                            (true, None)
                        }
                        RecordedPrefix::Diverged(recovered) => (false, recovered),
                    }
                };
            if resume_matches {
                file_identity = resume_state.file_identity;
                (previous.position, resume_state.generation)
            } else if previous.position > 0 && prefix_recovery == &JsonlPrefixRecovery::Report {
                return Ok(JsonlCapture::PrefixDiverged { file_identity });
            } else if let Some((JsonlPrefixCheckpoint { position, .. }, digest)) = recovered {
                // Checkpoints are this source's own committed prefixes, so a
                // match proves the bytes whether or not the file was replaced.
                let resume_fingerprint = digest.fingerprint(position);
                retained_prefix = Some(RawJsonlSkippedRange {
                    offset: 0,
                    end_offset: position,
                    resume_fingerprint,
                    reason: RawJsonlSkippedReason::RetainedPrefix,
                });
                validated_prefix = Some((position, digest));
                (
                    position,
                    rewritten_jsonl_generation(
                        resume_state,
                        file_identity,
                        resume_fingerprint,
                        file_size,
                        mtime,
                    ),
                )
            } else if file_identity == resume_state.file_identity {
                (
                    0,
                    rewritten_jsonl_generation(
                        resume_state,
                        file_identity,
                        memoized_jsonl_snapshot_fingerprint(
                            &mut snapshot_fingerprint,
                            &mut io.snapshot_hash_bytes,
                            &mut file,
                            file_size,
                        )
                        .map_err(|error| {
                            TranscriptIngestError::scan_io("fingerprint", path, error)
                        })?,
                        file_size,
                        mtime,
                    ),
                )
            } else {
                (0, file_identity)
            }
        } else if should_resume_jsonl(previous, file_size, mtime, file_identity) {
            // Carry the stored generation forward: a replacement marker minted
            // when this file was rewritten must survive every later batch of
            // that generation, otherwise batch two would re-mint ids that
            // collide with the retained pre-rewrite rows.
            (
                previous.position,
                if previous.file_id == 0 {
                    file_identity
                } else {
                    previous.file_id
                },
            )
        } else if previous.position > 0 {
            // The cursor is being rewound to the head of a file it had already
            // read past: this generation replaces the recorded one.
            (
                0,
                next_replacement_jsonl_generation(file_identity, previous.file_id),
            )
        } else if is_replacement_jsonl_generation(file_identity, previous.file_id) {
            // A replacement that was truncated to nothing keeps its marker so
            // the records written into it stay namespaced.
            (0, previous.file_id)
        } else {
            (0, file_identity)
        };
        // A settled change token is proof a same-length rewrite would have
        // moved ctime, so a cold catch-up of historical transcripts does not
        // hash the whole extent. A token still inside the kernel's coarse
        // timestamp quantum is not that proof: the rewrite can land before
        // this scan reads and leave inode, length, and both timestamps alone.
        // Seal the fingerprint before `after_generation_capture` so that
        // rewrite cannot hash as its own witness. A token with no rewrite
        // witness never rules that rewrite out; `revalidate` proves the
        // consumed prefix on every such scan instead.
        if snapshot_fingerprint.is_none()
            && file_size > 0
            && change.witnesses_rewrites()
            && !jsonl_change_token_settled(change)
        {
            memoized_jsonl_snapshot_fingerprint(
                &mut snapshot_fingerprint,
                &mut io.snapshot_hash_bytes,
                &mut file,
                file_size,
            )
            .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        }
        after_generation_capture();
        // A generation that is not the file's own identity was minted for a
        // rewrite, so it stays flagged for every batch it covers. The rewind
        // clause additionally covers the first batch after a file was replaced
        // by a different identity, whose generation is that new identity.
        let replacement = file_id != file_identity || (seek_to == 0 && previous.position > 0);
        io.change = if replacement {
            JsonlChangeKind::Rewritten
        } else if previous.position == 0 {
            JsonlChangeKind::Cold
        } else if seek_to >= file_size {
            JsonlChangeKind::Unchanged
        } else {
            JsonlChangeKind::Appended
        };
        Ok(JsonlCapture::Scan(Box::new(Self {
            file,
            generation: JsonlScanGeneration {
                file_size,
                mtime,
                change,
                witness,
                file_id,
                file_identity,
                physical_identity,
                snapshot_fingerprint,
                seek_to,
                replacement,
            },
            cached_unchanged,
            validated_prefix,
            retained_prefix,
        })))
    }

    fn is_complete(&self) -> bool {
        self.generation.seek_to >= self.generation.file_size
    }

    fn into_empty_outcome(
        mut self,
        path: &Path,
        io: &mut JsonlIoAccounting,
    ) -> TranscriptIngestResult<RawNewJsonl> {
        let metadata = self
            .file
            .inner()
            .metadata()
            .map_err(|error| TranscriptIngestError::scan_io("fstat", path, error))?;
        if let Some(expected) = self.cached_unchanged {
            let observed_native = jsonl_native_file_identity(self.file.inner(), &metadata);
            if observed_native != Some(expected.native_identity)
                || metadata.len() != expected.size
                || jsonl_file_change_token_under(&metadata, self.generation.witness)
                    != expected.change
            {
                return Err(TranscriptIngestError::ScanGenerationChanged {
                    path: path.to_path_buf(),
                });
            }
        } else {
            let (final_file_identity, identity_window_bytes) =
                stable_jsonl_file_id(self.file.inner_mut(), &metadata)
                    .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
            io.identity_window_bytes = io
                .identity_window_bytes
                .saturating_add(identity_window_bytes);
            if final_file_identity != self.generation.physical_identity
                || metadata.len() != self.generation.file_size
                || jsonl_file_change_token_under(&metadata, self.generation.witness)
                    != self.generation.change
            {
                return Err(TranscriptIngestError::ScanGenerationChanged {
                    path: path.to_path_buf(),
                });
            }
            if let Some(expected_snapshot) = self.generation.snapshot_fingerprint {
                let (final_snapshot, snapshot_hashed) = bounded_jsonl_snapshot_fingerprint(
                    &mut self.file,
                    self.generation.file_size,
                )
                .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
                io.snapshot_hash_bytes = io.snapshot_hash_bytes.saturating_add(snapshot_hashed);
                if final_snapshot != expected_snapshot {
                    return Err(TranscriptIngestError::ScanGenerationChanged {
                        path: path.to_path_buf(),
                    });
                }
            }
            if let Some((extent, digest)) = self.validated_prefix
                && extent == self.generation.seek_to
                && extent == metadata.len()
            {
                let cursor = StoredCursor {
                    position: extent,
                    mtime: file_mtime_secs(&metadata),
                    file_id: self.generation.file_id,
                };
                let resume = JsonlResumeState {
                    generation: self.generation.file_id,
                    file_identity: self.generation.file_identity,
                    fingerprint: digest.fingerprint(extent),
                };
                if let Some(key) = unchanged_generation_cache_key(
                    self.file.inner(),
                    &metadata,
                    jsonl_file_change_token_under(&metadata, self.generation.witness),
                    cursor,
                    resume,
                ) {
                    remember_unchanged_generation_if_settled(
                        path,
                        UnchangedGenerationProof {
                            key,
                            physical_identity: self.generation.physical_identity,
                            digest,
                            shared: false,
                        },
                        self.generation.seek_to,
                    );
                }
            }
        }
        let generation = self.generation;
        Ok(RawNewJsonl {
            frames: Vec::new(),
            skipped: self.retained_prefix.into_iter().collect(),
            start_offset: generation.seek_to,
            read_through: generation.seek_to,
            file_identity: generation.file_identity,
            new_cursor: StoredCursor {
                position: generation.seek_to,
                mtime: generation.mtime,
                file_id: generation.file_id,
            },
            replacement_generation: generation.replacement,
            deferred: None,
            prefix_diverged: false,
            io: *io,
        })
    }
}

enum JsonlScanStep {
    Continue,
    Stop(Option<JsonlFrameDeferral>),
}

struct RawJsonlBatchScanner<'a> {
    reader: RawJsonlFrameReader<BufReader<MeasuredJsonlFile<'a>>>,
    generation: JsonlScanGeneration,
    max_new_bytes: Option<u64>,
    max_frames: usize,
    scan_end: Option<u64>,
    one_record_budget: u64,
    max_record_bytes: usize,
    frames: Vec<RawJsonlRecord>,
    skipped: Vec<RawJsonlSkippedRange>,
    offset: u64,
    /// Digest of `[0, offset)` before the reader hashed a frame this batch
    /// then left for the next one.
    offset_digest: ResumeDigest,
    read_through: u64,
    continuing_oversized: bool,
    frame_count: usize,
    deferred: Option<JsonlFrameDeferral>,
}

impl<'a> RawJsonlBatchScanner<'a> {
    fn start(
        path: &Path,
        prepared: PreparedJsonlScan<'a>,
        max_new_bytes: Option<u64>,
        max_frames: usize,
        max_record_bytes: usize,
        io: &mut JsonlIoAccounting,
    ) -> TranscriptIngestResult<Self> {
        let generation = prepared.generation;
        let mut file = prepared.file;
        let resume_digest = match prepared.validated_prefix {
            // `capture` already walked exactly this prefix to check the resume
            // checkpoint, and the digest it produced is the one this reader
            // needs. Re-deriving it would read the same bytes a second time
            // for an identical result.
            Some((extent, digest)) if extent == generation.seek_to => digest,
            _ => {
                let (digest, hashed) = jsonl_prefix_digest(&mut file, generation.seek_to)
                    .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
                io.prefix_validation_bytes = io.prefix_validation_bytes.saturating_add(hashed);
                digest
            }
        };
        let continuing_oversized = Self::starts_inside_record(path, &mut file, generation.seek_to)?;
        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(generation.seek_to))
            .map_err(|error| TranscriptIngestError::scan_io("seek", path, error))?;
        let frame_limit = if continuing_oversized {
            0
        } else {
            max_record_bytes
        };
        let mut reader = RawJsonlFrameReader::new(reader, frame_limit);
        reader.seed_resume_digest(resume_digest.clone());
        Ok(Self {
            reader,
            offset_digest: resume_digest,
            generation,
            max_new_bytes,
            max_frames,
            scan_end: max_new_bytes.map(|cap| {
                generation
                    .seek_to
                    .saturating_add(cap)
                    .min(generation.file_size)
            }),
            one_record_budget: u64::try_from(max_record_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
            max_record_bytes,
            frames: Vec::new(),
            skipped: prepared.retained_prefix.into_iter().collect(),
            offset: generation.seek_to,
            read_through: generation.seek_to,
            continuing_oversized,
            frame_count: 0,
            deferred: None,
        })
    }

    fn starts_inside_record(
        path: &Path,
        file: &mut MeasuredJsonlFile<'_>,
        seek_to: u64,
    ) -> TranscriptIngestResult<bool> {
        if seek_to == 0 {
            return Ok(false);
        }
        let mut previous = [0_u8; 1];
        file.seek(SeekFrom::Start(seek_to - 1))
            .map_err(|error| TranscriptIngestError::scan_io("seek", path, error))?;
        file.read_exact(&mut previous)
            .map_err(|error| TranscriptIngestError::scan_io("read", path, error))?;
        file.seek(SeekFrom::Start(seek_to))
            .map_err(|error| TranscriptIngestError::scan_io("seek", path, error))?;
        Ok(previous[0] != b'\n')
    }

    fn scan(mut self, path: &Path, io: &mut JsonlIoAccounting) -> TranscriptIngestResult<Self> {
        loop {
            if let Some(step) = self.boundary_step() {
                self.apply_step(&step);
                return Ok(self);
            }
            self.offset_digest = self.reader.resume_digest();
            let read_budget = self.read_budget();
            let frame = self
                .reader
                .next_frame_with_budget(read_budget)
                .map_err(|error| TranscriptIngestError::scan_io("read", path, error))?;
            self.read_through = self
                .read_through
                .max(self.offset.saturating_add(frame.byte_len()));
            io.content_bytes = io.content_bytes.saturating_add(frame.byte_len());
            self.frame_count = self.frame_count.saturating_add(1);
            let resume_fingerprint = self
                .reader
                .resume_fingerprint(self.offset.saturating_add(frame.byte_len()));
            let step = match frame {
                RawJsonlFrame::Eof => JsonlScanStep::Stop(None),
                RawJsonlFrame::Partial { .. } => {
                    JsonlScanStep::Stop(Some(JsonlFrameDeferral::Partial {
                        offset: self.offset,
                    }))
                }
                RawJsonlFrame::Oversized {
                    byte_len,
                    terminated,
                } => self.handle_oversized(byte_len, terminated, resume_fingerprint),
                RawJsonlFrame::BudgetExhausted {
                    byte_len,
                    oversized,
                } => self.handle_budget_exhausted(byte_len, oversized, resume_fingerprint),
                RawJsonlFrame::Complete { byte_len } => {
                    self.handle_complete(byte_len, resume_fingerprint)
                }
            };
            if self.apply_step(&step) {
                return Ok(self);
            }
        }
    }

    fn boundary_step(&self) -> Option<JsonlScanStep> {
        if self.offset >= self.generation.file_size {
            return Some(JsonlScanStep::Stop(None));
        }
        let budget_exhausted = self
            .scan_end
            .is_some_and(|end| self.offset >= end && self.offset < self.generation.file_size);
        if budget_exhausted || self.frame_count >= self.max_frames {
            return Some(JsonlScanStep::Stop(self.backlog_at(self.offset)));
        }
        None
    }

    fn read_budget(&self) -> u64 {
        let nominal = self
            .scan_end
            .map_or(u64::MAX, |end| end.saturating_sub(self.offset));
        // Finish at most the current valid record past the nominal cap. Chunks
        // already known to be oversized retain the nominal budget.
        if self.continuing_oversized {
            nominal
        } else {
            nominal.max(self.one_record_budget)
        }
    }

    fn handle_oversized(
        &mut self,
        byte_len: u64,
        terminated: bool,
        resume_fingerprint: u64,
    ) -> JsonlScanStep {
        let next_offset = self.offset.saturating_add(byte_len);
        self.push_skipped(
            next_offset,
            RawJsonlSkippedReason::Oversized,
            resume_fingerprint,
        );
        self.offset = next_offset;
        // Having consumed the tail of the record this scan resumed inside,
        // restore the real record budget. Without this the reader keeps the
        // zero limit it was resumed with and reports every subsequent valid
        // record as oversized.
        if terminated && self.continuing_oversized {
            self.reader.set_max_record_bytes(self.max_record_bytes);
            self.continuing_oversized = false;
        }
        if self.offset < self.generation.file_size {
            JsonlScanStep::Stop(self.backlog_at(self.offset))
        } else {
            JsonlScanStep::Continue
        }
    }

    fn handle_budget_exhausted(
        &mut self,
        byte_len: u64,
        oversized: bool,
        resume_fingerprint: u64,
    ) -> JsonlScanStep {
        if oversized && byte_len > 0 {
            let next_offset = self.offset.saturating_add(byte_len);
            self.push_skipped(
                next_offset,
                RawJsonlSkippedReason::Oversized,
                resume_fingerprint,
            );
            self.offset = next_offset;
        }
        JsonlScanStep::Stop(self.backlog_at(self.offset))
    }

    fn handle_complete(&mut self, byte_len: u64, resume_fingerprint: u64) -> JsonlScanStep {
        let next_offset = self.offset.saturating_add(byte_len);
        if self.offset > self.generation.seek_to
            && self.scan_end.is_some_and(|end| next_offset > end)
        {
            return JsonlScanStep::Stop(self.backlog_at(self.offset));
        }
        if self.reader.record().iter().all(u8::is_ascii_whitespace) {
            self.push_skipped(
                next_offset,
                RawJsonlSkippedReason::Whitespace,
                resume_fingerprint,
            );
        } else {
            self.frames.push(RawJsonlRecord {
                offset: self.offset,
                end_offset: next_offset,
                resume_fingerprint,
                bytes: self.reader.record().to_vec(),
            });
        }
        self.offset = next_offset;
        JsonlScanStep::Continue
    }

    fn push_skipped(
        &mut self,
        end_offset: u64,
        reason: RawJsonlSkippedReason,
        resume_fingerprint: u64,
    ) {
        if let Some(last) = self.skipped.last_mut()
            && last.reason == reason
            && last.end_offset == self.offset
        {
            last.end_offset = end_offset;
            last.resume_fingerprint = resume_fingerprint;
        } else {
            self.skipped.push(RawJsonlSkippedRange {
                offset: self.offset,
                end_offset,
                resume_fingerprint,
                reason,
            });
        }
    }

    fn backlog_at(&self, offset: u64) -> Option<JsonlFrameDeferral> {
        self.max_new_bytes
            .map(|max_new_bytes| JsonlFrameDeferral::Backlog {
                offset,
                unread_bytes: self.generation.file_size.saturating_sub(offset),
                max_new_bytes,
            })
    }

    fn apply_step(&mut self, step: &JsonlScanStep) -> bool {
        match step {
            JsonlScanStep::Continue => false,
            JsonlScanStep::Stop(deferred) => {
                self.deferred = *deferred;
                true
            }
        }
    }

    fn revalidate(
        self,
        path: &Path,
        io: &mut JsonlIoAccounting,
    ) -> TranscriptIngestResult<RawNewJsonl> {
        let scan_fingerprint = self.reader.resume_fingerprint(self.read_through);
        // The reader hashed through `read_through`; a frame read past
        // `offset` and left for the next batch sits beyond the cursor.
        let offset_digest = if self.read_through == self.offset {
            self.reader.resume_digest()
        } else {
            self.offset_digest
        };
        let mut file = self.reader.into_inner().into_inner();
        let final_metadata = file
            .inner()
            .metadata()
            .map_err(|error| TranscriptIngestError::scan_io("fstat", path, error))?;
        let (final_file_id, identity_window_bytes) =
            stable_jsonl_file_id(file.inner_mut(), &final_metadata)
                .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        io.identity_window_bytes = io
            .identity_window_bytes
            .saturating_add(identity_window_bytes);
        let snapshot_changed = if let Some(expected_snapshot) = self.generation.snapshot_fingerprint
        {
            let (final_snapshot, snapshot_hashed) =
                bounded_jsonl_snapshot_fingerprint(&mut file, self.generation.file_size)
                    .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
            io.snapshot_hash_bytes = io.snapshot_hash_bytes.saturating_add(snapshot_hashed);
            final_snapshot != expected_snapshot
        } else {
            false
        };
        // Scans that minted a snapshot (a rewrite was already observed) compare
        // it. Every scan compares identity, size, and the high-resolution
        // change token:
        //
        // * identity covers the inode and the head window;
        // * a length below what was read means the file shrank under the scan;
        // * a moved change token means the inode was touched, but not what
        //   was done to it. Length cannot tell the cases apart: a rename moves
        //   ctime without writing a byte, an append writes only past what this
        //   scan consumed, and a same-size rewrite replaces everything. So
        //   rather than infer from length, prove the bytes this scan actually
        //   consumed still hash to the digest accumulated while parsing them.
        //
        // The common unchanged cold scan performs no extra extent hash. The
        // one-pass proof is paid only when the token moved while the file was
        // open, or when it has no rewrite witness and so cannot show a move,
        // and covers only the consumed prefix, never the whole extent.
        let final_change = jsonl_file_change_token_under(&final_metadata, self.generation.witness);
        let generation_changed =
            final_change != self.generation.change || !final_change.witnesses_rewrites();
        // Data was written and the file did not grow, so the bytes this scan
        // consumed were replaced. Rejected without the proof below, which
        // cannot see a rewrite that landed before the read began.
        let wrote_without_growing = final_change.data_stamp()
            != self.generation.change.data_stamp()
            && final_metadata.len() <= self.generation.file_size;
        let changed_consumed_prefix = if generation_changed
            && !wrote_without_growing
            && self.generation.snapshot_fingerprint.is_none()
        {
            let (digest, hashed) = jsonl_prefix_digest(&mut file, self.read_through)
                .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
            io.prefix_validation_bytes = io.prefix_validation_bytes.saturating_add(hashed);
            digest.fingerprint(self.read_through) != scan_fingerprint
        } else {
            false
        };
        if final_file_id != self.generation.physical_identity
            || snapshot_changed
            || wrote_without_growing
            || changed_consumed_prefix
            || final_metadata.len() < self.read_through
        {
            return Err(TranscriptIngestError::ScanGenerationChanged {
                path: path.to_path_buf(),
            });
        }
        let resume = JsonlResumeState {
            generation: self.generation.file_id,
            file_identity: self.generation.file_identity,
            fingerprint: offset_digest.fingerprint(self.offset),
        };
        let cursor = StoredCursor {
            position: self.offset,
            mtime: file_mtime_secs(&final_metadata),
            file_id: self.generation.file_id,
        };
        if let Some(key) = unchanged_generation_cache_key(
            file.inner(),
            &final_metadata,
            final_change,
            cursor,
            resume,
        ) {
            remember_unchanged_generation_if_settled(
                path,
                UnchangedGenerationProof {
                    key,
                    physical_identity: self.generation.physical_identity,
                    digest: offset_digest,
                    shared: false,
                },
                self.generation.seek_to,
            );
        }
        Ok(RawNewJsonl {
            frames: self.frames,
            skipped: self.skipped,
            start_offset: self.generation.seek_to,
            read_through: self.read_through,
            file_identity: self.generation.file_identity,
            new_cursor: StoredCursor {
                position: self.offset,
                mtime: file_mtime_secs(&final_metadata),
                file_id: self.generation.file_id,
            },
            replacement_generation: self.generation.replacement,
            deferred: self.deferred,
            prefix_diverged: false,
            io: *io,
        })
    }
}

fn try_stream_new_jsonl_raw_from_file(
    path: &Path,
    file: std::fs::File,
    request: RawJsonlScanRequest,
    after_generation_capture: impl FnOnce(),
) -> TranscriptIngestResult<RawNewJsonl> {
    let RawJsonlScanRequest {
        previous,
        max_new_bytes,
        max_frames,
        max_record_bytes,
        resume_state,
        prefix_recovery,
        witness,
    } = request;
    let scan_payload_reads = ScanPayloadMeter::new();
    let file = MeasuredJsonlFile::new(file, &scan_payload_reads);
    let mut io = JsonlIoAccounting::default();
    let result = (|| {
        let prepared = match PreparedJsonlScan::capture(
            path,
            file,
            previous,
            resume_state,
            &prefix_recovery,
            witness,
            after_generation_capture,
            &mut io,
        )? {
            JsonlCapture::Scan(prepared) => *prepared,
            JsonlCapture::PrefixDiverged { file_identity } => {
                return Ok(RawNewJsonl::prefix_diverged(previous, file_identity, io));
            }
        };
        if prepared.is_complete() {
            prepared.into_empty_outcome(path, &mut io)
        } else {
            RawJsonlBatchScanner::start(
                path,
                prepared,
                max_new_bytes,
                max_frames,
                max_record_bytes,
                &mut io,
            )?
            .scan(path, &mut io)?
            .revalidate(path, &mut io)
        }
    })();
    io.scan_payload_read_bytes = scan_payload_reads.get();
    result.map(|mut raw| {
        raw.io = io;
        raw
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn bounded_generation_cache_keeps_one_latest_proof_with_logarithmic_admission_work() {
        let mut cache = BoundedLatestProofCache::new(UNCHANGED_GENERATION_CACHE_CAP);
        let native_identity_count = UNCHANGED_GENERATION_CACHE_CAP + 17;
        let native_identities = 0..native_identity_count as u64;
        CACHE_HEAP_COMPARISONS.with(|count| count.set(0));
        for native_identity in native_identities.clone() {
            cache.insert(native_identity, 0_u64);
        }

        let first_pass_comparisons = CACHE_HEAP_COMPARISONS.with(Cell::get);
        let heap_levels =
            usize::BITS as usize - UNCHANGED_GENERATION_CACHE_CAP.leading_zeros() as usize;
        let logarithmic_bound =
            native_identity_count.saturating_mul(heap_levels.saturating_mul(4).saturating_add(1));
        assert!(
            first_pass_comparisons <= logarithmic_bound,
            "{first_pass_comparisons} heap comparisons exceeded {logarithmic_bound}"
        );
        assert_eq!(cache.len(), UNCHANGED_GENERATION_CACHE_CAP);
        assert_eq!(cache.admission_len(), UNCHANGED_GENERATION_CACHE_CAP);

        for generation in 1..=8_u64 {
            CACHE_HEAP_COMPARISONS.with(|count| count.set(0));
            for native_identity in native_identities.clone() {
                cache.insert(native_identity, generation);
            }
            let churn_comparisons = CACHE_HEAP_COMPARISONS.with(Cell::get);
            assert_eq!(
                churn_comparisons,
                native_identity_count - UNCHANGED_GENERATION_CACHE_CAP,
                "generation {generation} performed more than one admission check per rejected native file"
            );
            assert_eq!(
                native_identities
                    .clone()
                    .filter(|native_identity| cache.contains(native_identity, &generation))
                    .count(),
                UNCHANGED_GENERATION_CACHE_CAP
            );
            assert_eq!(
                native_identities
                    .clone()
                    .filter(|native_identity| {
                        cache.contains(native_identity, &generation.saturating_sub(1))
                    })
                    .count(),
                0,
                "generation {generation} left stale proofs in the cache"
            );
            assert_eq!(cache.len(), UNCHANGED_GENERATION_CACHE_CAP);
            assert_eq!(cache.admission_len(), UNCHANGED_GENERATION_CACHE_CAP);
        }
    }

    #[test]
    fn rename_after_generation_capture_scans_one_handle_then_resets_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("active.jsonl");
        let moved = dir.path().join("moved.jsonl");
        let original = b"{\"id\":\"original\"}\n";
        let replacement = b"{\"id\":\"replaced\"}\n";
        std::fs::write(&path, original).unwrap();
        let handle = std::fs::File::open(&path).unwrap();

        let first = try_stream_new_jsonl_raw_from_file(
            &path,
            handle,
            RawJsonlScanRequest {
                previous: StoredCursor::default(),
                max_new_bytes: None,
                max_frames: MAX_JSONL_FRAMES_PER_BATCH,
                max_record_bytes: MAX_JSONL_RECORD_BYTES,
                resume_state: None,
                prefix_recovery: JsonlPrefixRecovery::rescan(),
                witness: RewriteWitness::NATIVE,
            },
            || {
                std::fs::rename(&path, &moved).unwrap();
                std::fs::write(&path, replacement).unwrap();
            },
        )
        .unwrap();

        assert_eq!(first.frames.len(), 1);
        assert_eq!(first.frames[0].bytes, original);
        assert_eq!(first.new_cursor.position, original.len() as u64);

        let second =
            try_stream_new_jsonl_raw_strict(&path, first.new_cursor, None, MAX_JSONL_RECORD_BYTES)
                .unwrap();
        assert_eq!(second.start_offset, 0);
        assert_eq!(second.frames.len(), 1);
        assert_eq!(second.frames[0].bytes, replacement);
        assert_eq!(second.new_cursor.position, replacement.len() as u64);
    }

    #[test]
    fn same_handle_mutation_after_generation_capture_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mutated.jsonl");
        let original = b"{\"id\":\"original\"}\n";
        let replacement = b"{\"id\":\"replaced\"}\n";
        assert_eq!(original.len(), replacement.len());
        std::fs::write(&path, original).unwrap();
        let handle = std::fs::File::open(&path).unwrap();

        let error = try_stream_new_jsonl_raw_from_file(
            &path,
            handle,
            RawJsonlScanRequest {
                previous: StoredCursor::default(),
                max_new_bytes: None,
                max_frames: MAX_JSONL_FRAMES_PER_BATCH,
                max_record_bytes: MAX_JSONL_RECORD_BYTES,
                resume_state: None,
                prefix_recovery: JsonlPrefixRecovery::rescan(),
                witness: RewriteWitness::NATIVE,
            },
            || std::fs::write(&path, replacement).unwrap(),
        )
        .err()
        .expect("same-handle mutation must invalidate the scan generation");

        assert!(matches!(
            error,
            TranscriptIngestError::ScanGenerationChanged { path: error_path }
                if error_path == path
        ));
    }

    /// The counterpart to the rewrite test: a transcript being appended to
    /// while it is scanned is the normal case for a live session, and the
    /// appended bytes are past what the scan consumed. Growth moves the same
    /// change token an in-place rewrite moves, so this pins that growth alone
    /// is not treated as a changed generation, otherwise every scan of an
    /// active session would fail and retry forever.
    #[test]
    fn concurrent_append_during_a_scan_is_not_a_generation_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appending.jsonl");
        std::fs::write(&path, b"{\"v\":0}\n").unwrap();
        // The append must move the change token. A just-written file is still
        // inside the coarse quantum, where an append can share that timestamp.
        spin_until_jsonl_change_settled(&path);
        let handle = std::fs::File::open(&path).unwrap();

        let outcome = try_stream_new_jsonl_raw_from_file(
            &path,
            handle,
            RawJsonlScanRequest {
                previous: StoredCursor::default(),
                max_new_bytes: None,
                max_frames: MAX_JSONL_FRAMES_PER_BATCH,
                max_record_bytes: MAX_JSONL_RECORD_BYTES,
                resume_state: None,
                prefix_recovery: JsonlPrefixRecovery::rescan(),
                witness: RewriteWitness::NATIVE,
            },
            || {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(b"{\"v\":1}\n")
                    .unwrap();
            },
        )
        .expect("an append past the scanned extent must not invalidate the scan");

        assert_eq!(outcome.frames.len(), 1, "the scan keeps its own extent");
        assert_eq!(
            outcome.io.snapshot_hash_bytes, 0,
            "and still does not hash the file to prove it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_cache_miss_returns_complete_empty_scan_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata-change.jsonl");
        std::fs::write(&path, b"{\"v\":0}\n").unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(permissions.mode() ^ 0o100);
        std::fs::set_permissions(&path, permissions).unwrap();

        let second = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();

        assert_eq!(second.io.change, JsonlChangeKind::Unchanged);
        assert_eq!(second.io.prefix_validation_bytes, first.new_cursor.position);
        assert_eq!(
            second.io.identity_window_bytes,
            first.io.identity_window_bytes
        );
        assert!(second.io.scan_payload_read_bytes >= first.new_cursor.position);
    }

    fn resume_next_batch(
        path: &Path,
        previous: Option<&RawNewJsonl>,
        max_new_bytes: u64,
    ) -> RawNewJsonl {
        try_stream_new_jsonl_raw_strict_with_resume(
            path,
            previous.map_or_else(StoredCursor::default, |scan| scan.new_cursor),
            Some(max_new_bytes),
            MAX_JSONL_RECORD_BYTES,
            previous.map(|scan| JsonlResumeState {
                generation: scan.new_cursor.file_id,
                file_identity: scan.file_identity,
                fingerprint: scan.frames.last().unwrap().resume_fingerprint,
            }),
        )
        .unwrap()
    }

    /// Two scopes catch one file up batch by batch under their own cursors.
    /// However their batches interleave, each resumes from the digest its own
    /// previous batch proved instead of rehashing the prefix.
    #[test]
    fn interleaved_cursors_each_resume_without_rehashing_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let _hold = HoldUnchangedGenerationCache::enter(dir.path());
        let path = dir.path().join("two-scopes.jsonl");
        std::fs::write(&path, b"{\"v\":0}\n".repeat(4_096)).unwrap();
        spin_until_jsonl_change_settled(&path);
        let mut scopes = [
            (resume_next_batch(&path, None, 1_024), 1_024),
            (resume_next_batch(&path, None, 1_536), 1_536),
        ];
        for _ in 0..8 {
            for (scope, max_new_bytes) in &mut scopes {
                let next = resume_next_batch(&path, Some(scope), *max_new_bytes);
                assert_eq!(next.start_offset, scope.new_cursor.position);
                // Without a stat rewrite witness (Windows), resuming honestly
                // re-proves the recorded prefix and then re-proves the whole
                // consumed prefix after the scan, instead of skipping the
                // rehash the witness makes safe on Unix.
                let expected_prefix_validation = if cfg!(unix) {
                    0
                } else {
                    next.start_offset + next.read_through
                };
                assert_eq!(
                    next.io.prefix_validation_bytes, expected_prefix_validation,
                    "the batch at {} rehashed its prefix",
                    next.start_offset
                );
                *scope = next;
            }
        }
    }

    /// One scope can catch up many batches before the other resumes. Advancing
    /// replaces the proof that scope resumed from, so the idle scope still
    /// continues from its own digest.
    #[test]
    fn a_cursor_running_ahead_does_not_evict_the_other_scopes_resume() {
        let dir = tempfile::tempdir().unwrap();
        let _hold = HoldUnchangedGenerationCache::enter(dir.path());
        let path = dir.path().join("ahead.jsonl");
        std::fs::write(&path, b"{\"v\":0}\n".repeat(4_096)).unwrap();
        spin_until_jsonl_change_settled(&path);
        let mut ahead = resume_next_batch(&path, None, 1_024);
        let behind = resume_next_batch(&path, None, 1_536);
        // Hosts without a stat rewrite witness (Windows) can never skip
        // prefix re-validation: every resume re-proves the recorded prefix
        // and the whole consumed prefix after the scan.
        let expected_prefix_validation = |scan: &RawNewJsonl| {
            if cfg!(unix) {
                0
            } else {
                scan.start_offset + scan.read_through
            }
        };
        for _ in 0..6 {
            let next = resume_next_batch(&path, Some(&ahead), 1_024);
            assert_eq!(next.start_offset, ahead.new_cursor.position);
            assert_eq!(
                next.io.prefix_validation_bytes,
                expected_prefix_validation(&next)
            );
            ahead = next;
        }
        let resumed = resume_next_batch(&path, Some(&behind), 1_536);
        assert_eq!(resumed.start_offset, behind.new_cursor.position);
        assert_eq!(
            resumed.io.prefix_validation_bytes,
            expected_prefix_validation(&resumed),
            "the idle cursor rehashed its prefix after the other ran ahead"
        );
    }

    /// After an atomic replacement the project scope resumes a minted rewrite
    /// generation while the profile scope takes the new file's identity. Both
    /// sit at end-of-file on one settled file, so neither repoll may evict the
    /// other's proof and re-read the file (#2889).
    #[test]
    fn cursors_under_different_generations_both_settle_at_end_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let _hold = HoldUnchangedGenerationCache::enter(dir.path());
        let path = dir.path().join("replaced.jsonl");
        std::fs::write(&path, b"{\"v\":0}\n".repeat(64)).unwrap();
        spin_until_jsonl_change_settled(&path);
        let profile = resume_next_batch(&path, None, 1 << 20);
        assert_eq!(
            profile.new_cursor.position,
            std::fs::metadata(&path).unwrap().len()
        );
        let rewrite_generation = profile.new_cursor.file_id ^ 1;
        let project_cursor = StoredCursor {
            file_id: rewrite_generation,
            ..profile.new_cursor
        };
        let project_resume = JsonlResumeState {
            generation: rewrite_generation,
            file_identity: profile.file_identity,
            fingerprint: profile.frames.last().unwrap().resume_fingerprint,
        };
        let profile_resume = JsonlResumeState {
            generation: profile.new_cursor.file_id,
            ..project_resume
        };
        let repoll = |cursor, resume| {
            try_stream_new_jsonl_raw_strict_with_resume(
                &path,
                cursor,
                None,
                MAX_JSONL_RECORD_BYTES,
                Some(resume),
            )
            .unwrap()
        };
        // The first repoll of each scope proves its checkpoint; every later
        // one must be served from that proof.
        repoll(profile.new_cursor, profile_resume);
        repoll(project_cursor, project_resume);
        for round in 0..3 {
            for (scope, cursor, resume) in [
                ("profile", profile.new_cursor, profile_resume),
                ("project", project_cursor, project_resume),
            ] {
                let next = repoll(cursor, resume);
                assert_eq!(next.new_cursor.file_id, cursor.file_id);
                assert!(next.frames.is_empty());
                if cfg!(unix) {
                    assert_eq!(
                        (
                            next.io.prefix_validation_bytes,
                            next.io.identity_window_bytes
                        ),
                        (0, 0),
                        "round {round}: the {scope} scope re-read the settled file"
                    );
                }
            }
        }
    }

    /// The project scope resumes the profile's checkpoint under its own
    /// generation and advances. That retires only the project's own proof, so
    /// the profile still resumes the same offset from its digest.
    #[test]
    fn a_generation_advancing_from_a_shared_offset_keeps_the_other_generations_proof() {
        let dir = tempfile::tempdir().unwrap();
        let _hold = HoldUnchangedGenerationCache::enter(dir.path());
        let path = dir.path().join("two-generations.jsonl");
        std::fs::write(&path, b"{\"v\":0}\n".repeat(4_096)).unwrap();
        spin_until_jsonl_change_settled(&path);
        let profile = resume_next_batch(&path, None, 1_024);
        let rewrite_generation = profile.new_cursor.file_id ^ 1;
        let project = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor {
                file_id: rewrite_generation,
                ..profile.new_cursor
            },
            Some(1_024),
            MAX_JSONL_RECORD_BYTES,
            Some(JsonlResumeState {
                generation: rewrite_generation,
                file_identity: profile.file_identity,
                fingerprint: profile.frames.last().unwrap().resume_fingerprint,
            }),
        )
        .unwrap();
        assert_eq!(project.start_offset, profile.new_cursor.position);
        assert!(project.new_cursor.position > profile.new_cursor.position);

        let resumed = resume_next_batch(&path, Some(&profile), 1_024);
        assert_eq!(resumed.start_offset, profile.new_cursor.position);
        let expected_prefix_validation = if cfg!(unix) {
            0
        } else {
            resumed.start_offset + resumed.read_through
        };
        assert_eq!(
            resumed.io.prefix_validation_bytes, expected_prefix_validation,
            "the profile rehashed its prefix after the project advanced from the same offset"
        );
    }

    #[test]
    fn cached_unchanged_generation_revalidates_an_in_place_tail_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let _hold = HoldUnchangedGenerationCache::enter(dir.path());
        let path = dir.path().join("memo-rewrite.jsonl");
        let original = b"{\"v\":0}\n".repeat(3_000);
        std::fs::write(&path, &original).unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };

        let unchanged = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        assert_eq!(unchanged.start_offset, first.new_cursor.position);

        let mut rewritten = original;
        let tail = rewritten.len() - b"{\"v\":0}\n".len();
        rewritten[tail..].copy_from_slice(b"{\"v\":1}\n");
        std::fs::write(&path, rewritten).unwrap();

        let rescanned = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            unchanged.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        assert_eq!(
            rescanned.start_offset, 0,
            "a memoized prefix must be invalidated by a same-size in-place rewrite"
        );
        assert_ne!(rescanned.new_cursor.file_id, checkpoint.generation);
    }

    #[test]
    fn same_quantum_rewrite_is_visible_after_the_change_time_settles() {
        let dir = tempfile::tempdir().unwrap();
        let _hold = HoldUnchangedGenerationCache::enter(dir.path());
        let path = dir.path().join("quantum.jsonl");
        let original = b"{\"v\":0}\n";
        let replacement = b"{\"v\":1}\n";
        assert_eq!(original.len(), replacement.len());
        std::fs::write(&path, original).unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };
        std::fs::write(&path, replacement).unwrap();
        spin_until_jsonl_change_settled(&path);

        let rescanned = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        assert_eq!(rescanned.start_offset, 0);
        assert_ne!(rescanned.new_cursor.file_id, checkpoint.generation);
        assert_eq!(rescanned.frames.len(), 1);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn cached_unchanged_generation_rejects_inode_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let _hold = HoldUnchangedGenerationCache::enter(dir.path());
        let path = dir.path().join("active.jsonl");
        let old = dir.path().join("old.jsonl");
        let replacement = dir.path().join("replacement.jsonl");
        let contents = b"{\"v\":0}\n";
        std::fs::write(&path, contents).unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };

        std::fs::write(&replacement, b"{\"v\":1}\n").unwrap();
        std::fs::rename(&path, &old).unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        let rescanned = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        assert_eq!(rescanned.start_offset, 0);
        assert_ne!(rescanned.new_cursor.file_id, checkpoint.generation);
        assert_eq!(rescanned.frames.len(), 1);
    }

    #[test]
    fn cached_unchanged_generation_rejects_concurrent_same_handle_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let _hold = HoldUnchangedGenerationCache::enter(dir.path());
        let path = dir.path().join("concurrent.jsonl");
        let original = b"{\"v\":0}\n";
        let replacement = b"{\"v\":1}\n";
        std::fs::write(&path, original).unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };
        let handle = std::fs::File::open(&path).unwrap();

        let outcome = try_stream_new_jsonl_raw_from_file(
            &path,
            handle,
            RawJsonlScanRequest {
                previous: first.new_cursor,
                max_new_bytes: None,
                max_frames: MAX_JSONL_FRAMES_PER_BATCH,
                max_record_bytes: MAX_JSONL_RECORD_BYTES,
                resume_state: Some(checkpoint),
                prefix_recovery: JsonlPrefixRecovery::rescan(),
                witness: RewriteWitness::NATIVE,
            },
            || std::fs::write(&path, replacement).unwrap(),
        );

        assert!(matches!(
            outcome,
            Err(TranscriptIngestError::ScanGenerationChanged { path: changed })
                if changed == path
        ));
    }

    /// Where no stat field witnesses a rewrite (NTFS leaves `ChangeTime` put
    /// when a writer restores `LastWriteTime`), a same-length rewrite of an
    /// already-ingested line that restores the exact mtime leaves identity,
    /// length and change token equal. A resumed scan racing it must still
    /// refuse the generation instead of appending onto a prefix the file no
    /// longer holds, so only the content digest can decide.
    #[test]
    fn without_a_rewrite_witness_an_mtime_restored_prefix_rewrite_refuses_the_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restored.jsonl");
        let head = b"{\"v\":\"head\"}\n";
        let ingested = b"{\"v\":\"old\"}\n";
        let rewritten = b"{\"v\":\"new\"}\n";
        assert_eq!(ingested.len(), rewritten.len());
        std::fs::write(&path, [&head[..], &ingested[..]].concat()).unwrap();
        let scan = |previous, resume_state, after_capture: &dyn Fn()| {
            try_stream_new_jsonl_raw_from_file(
                &path,
                std::fs::File::open(&path).unwrap(),
                RawJsonlScanRequest {
                    previous,
                    max_new_bytes: None,
                    max_frames: MAX_JSONL_FRAMES_PER_BATCH,
                    max_record_bytes: MAX_JSONL_RECORD_BYTES,
                    resume_state,
                    prefix_recovery: JsonlPrefixRecovery::rescan(),
                    witness: RewriteWitness::Absent,
                },
                after_capture,
            )
        };
        let first = scan(StoredCursor::default(), None, &|| {}).unwrap();
        assert_eq!(first.frames.len(), 2);
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"v\":\"tail\"}\n")
            .unwrap();
        spin_until_jsonl_change_settled(&path);
        let appended_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        let outcome = scan(first.new_cursor, Some(checkpoint), &|| {
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(head.len() as u64)).unwrap();
            file.write_all(rewritten).unwrap();
            file.set_modified(appended_mtime).unwrap();
        });

        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            appended_mtime,
            "the rewrite fixture must restore the exact modification time"
        );
        assert!(
            matches!(
                &outcome,
                Err(TranscriptIngestError::ScanGenerationChanged { path: changed })
                    if *changed == path
            ),
            "a resumed scan over a rewritten prefix must be refused, got frames {:?}",
            outcome.as_ref().map(|scan| scan
                .frames
                .iter()
                .map(|frame| String::from_utf8_lossy(&frame.bytes).into_owned())
                .collect::<Vec<_>>())
        );
    }

    #[test]
    fn one_append_hashes_prefix_once_and_reads_appended_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("append.jsonl");
        let first_line = b"{\"v\":0}\n";
        std::fs::write(&path, first_line).unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };
        let appended = b"{\"v\":1}\n";
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(appended)
            .unwrap();
        // The settled re-poll is the path that must hash the prefix once and
        // not the whole file. Inside the coarse quantum the change token is
        // not that proof, so wait until it is before measuring the hash.
        spin_until_jsonl_change_settled(&path);
        let second = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        let prefix = u64::try_from(first_line.len()).unwrap();
        let appended_len = u64::try_from(appended.len()).unwrap();
        // A platform without a rewrite witness cannot trust the token, so the
        // commit step re-proves the consumed prefix at `read_through` bytes.
        let commit_proof = if RewriteWitness::NATIVE.proves_unchanged_bytes() {
            0
        } else {
            second.read_through
        };
        assert_eq!(second.io.change, JsonlChangeKind::Appended);
        assert_eq!(second.io.content_bytes, appended_len);
        assert_eq!(
            second.io.scan_payload_read_bytes,
            prefix + 1 + appended_len + commit_proof,
            "the canonical handle reads one exact prefix proof, one frame-boundary byte, and only the delta"
        );
        assert_eq!(
            second.io.prefix_validation_bytes,
            prefix + commit_proof,
            "one append verifies the stored prefix once and reuses that digest"
        );
        assert_eq!(
            second.io.snapshot_hash_bytes, 0,
            "append-only resume must not snapshot-hash the already-validated prefix"
        );
    }
}

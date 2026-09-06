//! Verified-generation markers: proving a sealed generation once per set of
//! bytes instead of once per open.
//!
//! # What the full proof costs, and why it repeats
//!
//! `verify_recovered_generation` streams every stored entity and relation of a
//! sealed generation out of the container, canonicalizes each row into a
//! framed encoding, and SHA-256s the stream. That is the only proof that the
//! rows the database will now serve are the exact rows the relational
//! authority journaled. It is also work proportional to the whole generation,
//! and a sealed generation never changes -- so a daemon that restarts pays the
//! identical proof over identical bytes, every time, forever.
//!
//! The optimization here is *not* skipping verification. It is not re-hashing
//! bytes that are still the bytes that were hashed before.
//!
//! # What a marker asserts
//!
//! A marker records, for one `.grafeo` container:
//!
//! * the container's file identity -- device, inode, length, and modification
//!   time -- captured after the container was closed and synced, and
//! * for each generation proven against that container, the recovered digest
//!   that was proven and the number of canonical bytes the proof hashed.
//!
//! On a later open the container is identified from an opened handle
//! (microseconds) and compared against the recorded identity. An exact match
//! means the bytes that back the in-RAM store are the bytes the proof already
//! ran over, so the recorded digest stands and the enumeration is skipped.
//! Anything else -- a missing marker, an unparseable one, a self-digest
//! mismatch, an identity mismatch, a missing durable file identity, or a
//! generation the marker does not list -- falls back to the full proof.
//!
//! # What a marker cannot do
//!
//! **The expected digest never comes from the marker.** It comes from the
//! relational authority (the journaled verified head or replay), exactly as it
//! did before. A marker is only consulted to answer "has this exact expected
//! digest already been proven against this exact container?", and a lookup
//! that does not match the caller's expected digest is a miss. So a marker
//! forged to claim some other digest for a generation buys an attacker
//! nothing: it cannot name which generation is served, and it cannot make a
//! wrong digest acceptable. The only thing a marker can assert is *freshness*.
//!
//! The `body_digest` binds a marker to its own contents, so a truncated or
//! partially-written marker is rejected rather than half-believed. It is
//! integrity, not authenticity: anyone who can rewrite the marker can also
//! recompute that digest.
//!
//! # The integrity boundary, stated honestly
//!
//! File identity is an **OS-integrity assumption, not a cryptographic one**.
//! `(device, inode, length, mtime)` detects a container that was replaced,
//! extended, truncated, or rewritten through the filesystem. It does not
//! detect bytes that changed underneath a stable inode without moving mtime --
//! neither silent bit rot nor an adversary who restores the timestamp.
//!
//! Two things stand behind that boundary:
//!
//! * **Grafeo's own per-section CRC-32.** Every section read out of a
//!   `.grafeo` container is CRC-checked before it is deserialized, on both the
//!   heap and mmap paths (`grafeo-storage` `file/manager.rs`). Accidental
//!   corruption -- bit rot, a partial write, a bad sector -- fails the open
//!   with `StorageCorrupted`, which `recovery::map_open_error` maps to
//!   `GraphDbError::Corrupt` on a preexisting store. That is the layer a
//!   marker was never covering: the SHA-256 replay proof only ever ran *after*
//!   the CRC had already passed.
//!
//! What a marker genuinely gives up is the *cryptographic* half against an
//! adversary who can write to the store directory while preserving file
//! metadata. That adversary can already rewrite the marker, the container, and
//! -- being inside the daemon's private store -- the relational authority's
//! expected digest as well. The proof it would have skipped was not defending
//! against it either.
//!
//! # Why not chunked or per-page digests
//!
//! The proof does not read a byte range. It enumerates rows through the graph
//! store's node and relation indexes and re-canonicalizes each one, so its
//! cost is row decode and serialization rather than bytes off disk, and there
//! is no region for a lazily-verified chunk to correspond to. A per-page
//! checksum would also duplicate the CRC-32 grafeo already applies per
//! section. Neither buys what the marker buys, which is skipping the
//! enumeration entirely.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;

use crate::lease::GenerationLocator;

/// Domain separation for the marker's self-digest, so the bytes can never be
/// confused with any other SHA-256 this crate computes.
const MARKER_DIGEST_DOMAIN: &[u8] = b"tracedecay.graph-db.verified-generation-marker.v1\0";

/// The marker format this build writes and is willing to read.
const MARKER_VERSION: u32 = 1;

/// Upper bound on a marker file. A record is a couple of hundred bytes, so
/// this admits far more generations than a store ever retains while still
/// refusing to read an arbitrarily large file found at the marker path.
const MAX_MARKER_BYTES: usize = 8 * 1024 * 1024;

/// Temp-file discriminator for the atomic publish. Deliberately free of the
/// `.tracedecay-` substring that the backup contract treats as staging
/// residue.
const MARKER_TEMP_KIND: &str = "verified-marker";

/// How a generation's recovered digest was established on this open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GenerationVerification {
    /// A marker recorded this exact digest against this exact container, and
    /// the container is byte-identical to the one the marker was written
    /// against. No rows were enumerated.
    VerifiedFresh,
    /// No usable marker applied, so the full row-streaming proof ran. The
    /// marker set is updated so the next open can be fresh.
    Reverified,
}

impl GenerationVerification {
    #[cfg(feature = "hotpath")]
    #[hotpath::skip]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedFresh => "verified_fresh",
            Self::Reverified => "reverified",
        }
    }
}

/// The identity of a `.grafeo` container as the filesystem reports it.
///
/// Identity is taken from an opened container handle, not from a path-only
/// stat. On Unix that is the handle's `(device, inode, mtime)` triple. On
/// Windows it is the volume serial and file index from
/// `GetFileInformationByHandle`, plus length and modification time. A pair
/// of `(device, inode) = (0, 0)` is not a file identity -- that was the
/// historical non-Unix fallback, and it lets a same-length replacement that
/// preserved its timestamp reuse a stale marker. Readers therefore treat a
/// missing or zero file-id as a marker miss and run the full proof.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContainerIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) len: u64,
    pub(crate) modified_seconds: i64,
    pub(crate) modified_nanoseconds: u32,
}

impl ContainerIdentity {
    /// Reads the identity of `path`, or `None` when it cannot be established.
    ///
    /// A missing file, a symlink, an unreadable handle, a modification time
    /// the platform declines to report, or a handle that does not expose a
    /// durable file identity all yield `None`, which callers treat as "no
    /// usable marker" rather than as an error: failing to take a shortcut is
    /// never a failure.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        // Refuse to follow a symlink at the container path: the marker must
        // name the object the path itself denotes.
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if !metadata.file_type().is_file() {
            return None;
        }
        let file = std::fs::File::open(path).ok()?;
        Self::from_opened(&file)
    }

    fn from_opened(file: &std::fs::File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        Self::from_opened_metadata(file, &metadata)?.if_durable()
    }

    /// A file identity is usable only when the durable file-id pair is not
    /// the historical `(0, 0)` placeholder. Length and mtime alone cannot
    /// distinguish a replacement that preserved those fields.
    fn has_durable_file_id(self) -> bool {
        self.device != 0 || self.inode != 0
    }

    fn if_durable(self) -> Option<Self> {
        self.has_durable_file_id().then_some(self)
    }

    #[cfg(unix)]
    fn from_opened_metadata(_file: &std::fs::File, metadata: &std::fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: u32::try_from(metadata.mtime_nsec()).ok()?,
        })
    }

    #[cfg(windows)]
    fn from_opened_metadata(file: &std::fs::File, metadata: &std::fs::Metadata) -> Option<Self> {
        let information = tracedecay_private_fs::windows_file::information(file).ok()?;
        let (modified_seconds, modified_nanoseconds) = modified_stamp(metadata.modified().ok()?)?;
        Some(Self {
            device: u64::from(information.volume_serial_number),
            inode: information.file_index,
            len: metadata.len(),
            modified_seconds,
            modified_nanoseconds,
        })
    }

    #[cfg(not(any(unix, windows)))]
    fn from_opened_metadata(_file: &std::fs::File, _metadata: &std::fs::Metadata) -> Option<Self> {
        None
    }
}

#[cfg(windows)]
fn modified_stamp(modified: std::time::SystemTime) -> Option<(i64, u32)> {
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(since) => Some((i64::try_from(since.as_secs()).ok()?, since.subsec_nanos())),
        Err(before) => {
            let since = before.duration();
            Some((
                i64::try_from(since.as_secs()).ok()?.checked_neg()?,
                since.subsec_nanos(),
            ))
        }
    }
}

/// One generation proven against one container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct VerifiedGenerationRecord {
    namespace: String,
    projection: String,
    generation: String,
    recovered_digest: String,
    canonical_bytes: u64,
}

/// The digest-bound body of a marker file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct MarkerBody {
    version: u32,
    container: ContainerIdentity,
    generations: Vec<VerifiedGenerationRecord>,
}

impl MarkerBody {
    /// The self-digest over this body's canonical encoding.
    ///
    /// `generations` is required to be strictly sorted on load, so one body
    /// has exactly one encoding and the digest is well defined.
    fn digest(&self) -> Option<String> {
        let encoded = serde_json::to_vec(self).ok()?;
        let mut digest = Sha256::new();
        digest.update(MARKER_DIGEST_DOMAIN);
        digest.update(&encoded);
        Some(encode_tagged_lowercase_hex("sha256:", &digest.finalize()))
    }

    fn is_strictly_sorted(&self) -> bool {
        self.generations.windows(2).all(|pair| {
            let (left, right) = (&pair[0], &pair[1]);
            (&left.namespace, &left.projection, &left.generation)
                < (&right.namespace, &right.projection, &right.generation)
        })
    }
}

/// A marker file: a body plus the digest that binds it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct MarkerFile {
    body: MarkerBody,
    body_digest: String,
}

/// A generation's proven digest and the canonical byte count behind it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProvenGeneration {
    pub(crate) recovered_digest: String,
    pub(crate) canonical_bytes: u64,
}

/// The marker path for a container: `graph.grafeo` -> `graph.verified`.
fn marker_path(container: &Path) -> PathBuf {
    container.with_extension("verified")
}

/// Reads and validates the marker beside `container`, keeping it only when it
/// was written against the container as it stands right now.
///
/// Every rejection is silent and returns an empty set: a marker is a cache of
/// completed proofs, and the absence of one only ever costs a full proof.
fn load(
    container: &Path,
    observed: ContainerIdentity,
) -> BTreeMap<GenerationKey, ProvenGeneration> {
    let path = marker_path(container);
    let Ok(Some(bytes)) = tracedecay_private_fs::framed_log::read_bounded(&path, MAX_MARKER_BYTES)
    else {
        return BTreeMap::new();
    };
    let Ok(marker) = serde_json::from_slice::<MarkerFile>(&bytes) else {
        return BTreeMap::new();
    };
    if marker.body.version != MARKER_VERSION || !marker.body.is_strictly_sorted() {
        return BTreeMap::new();
    }
    // The self-digest is checked before anything in the body is believed, so a
    // torn write cannot vouch for the half that landed.
    if marker.body.digest().as_deref() != Some(marker.body_digest.as_str()) {
        return BTreeMap::new();
    }
    // The identity gate. A marker written against different bytes describes a
    // container this one is not. A `(0, 0)` file-id -- recorded by the
    // historical non-Unix fallback or observed when the platform cannot name
    // the file -- is not an identity, so it cannot authorize a hit.
    if !observed.has_durable_file_id()
        || !marker.body.container.has_durable_file_id()
        || marker.body.container != observed
    {
        return BTreeMap::new();
    }
    marker
        .body
        .generations
        .into_iter()
        .map(|record| {
            (
                GenerationKey {
                    namespace: record.namespace,
                    projection: record.projection,
                    generation: record.generation,
                },
                ProvenGeneration {
                    recovered_digest: record.recovered_digest,
                    canonical_bytes: record.canonical_bytes,
                },
            )
        })
        .collect()
}

/// The lookup key for one generation, matching `GenerationLocator` field for
/// field but owned as plain strings so it round-trips through the marker file.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GenerationKey {
    namespace: String,
    projection: String,
    generation: String,
}

impl GenerationKey {
    fn from_locator(locator: &GenerationLocator) -> Self {
        Self {
            namespace: locator.projection.namespace.as_str().to_owned(),
            projection: locator.projection.projection.as_str().to_owned(),
            generation: locator.generation.as_str().to_owned(),
        }
    }
}

/// The marker set for one open database.
///
/// Holds two things that never mix: `admitted`, the proofs a marker file
/// carried into this open and that are still believable, and `proven`, the
/// proofs this process established itself. Only `proven` is ever published,
/// because only those were established against rows this process actually
/// streamed -- with one exception noted in `record_fresh`.
pub(crate) struct GenerationMarkers {
    container: PathBuf,
    /// Proofs carried in from a marker file whose recorded identity matched
    /// the container at open. Empty for an in-memory or first-ever store.
    admitted: BTreeMap<GenerationKey, ProvenGeneration>,
    /// Proofs this open established or re-affirmed, published at close.
    proven: std::sync::Mutex<BTreeMap<GenerationKey, ProvenGeneration>>,
    /// False once anything has taken the exclusive claim on this database.
    ///
    /// The exclusive claim is this crate's documented gate for every write
    /// that rewrites the container, so clearing it here is the single point
    /// that stops `admitted` from being consulted once the store has diverged
    /// from the bytes the marker was written against.
    pristine: AtomicBool,
}

impl GenerationMarkers {
    /// Opens the marker set for a persistent container.
    ///
    /// `observed` must be the identity read from an opened handle **before**
    /// grafeo opens the container, because an open may checkpoint the WAL and
    /// move the modification time before any caller could read it. The durable
    /// file-id half of that identity does not move with the WAL.
    pub(crate) fn open(container: &Path, observed: Option<ContainerIdentity>) -> Self {
        let admitted = observed
            .map(|observed| load(container, observed))
            .unwrap_or_default();
        Self {
            container: container.to_path_buf(),
            admitted,
            proven: std::sync::Mutex::new(BTreeMap::new()),
            pristine: AtomicBool::new(true),
        }
    }

    /// A marker set for a database with no container to bind to.
    pub(crate) fn detached() -> Self {
        Self {
            container: PathBuf::new(),
            admitted: BTreeMap::new(),
            proven: std::sync::Mutex::new(BTreeMap::new()),
            pristine: AtomicBool::new(false),
        }
    }

    /// Notes that the exclusive claim was taken, permanently retiring the
    /// admitted proofs for this open.
    pub(crate) fn mark_container_mutated(&self) {
        self.pristine.store(false, Ordering::Release);
    }

    /// Looks up a completed proof of `expected` for `locator`.
    ///
    /// Returns the canonical byte count the original proof hashed, for the
    /// byte gauge, or `None` when the full proof has to run. The caller's
    /// `expected` digest -- which comes from the relational authority, never
    /// from the marker -- must match exactly.
    pub(crate) fn lookup(&self, locator: &GenerationLocator, expected: &str) -> Option<u64> {
        if !self.pristine.load(Ordering::Acquire) {
            return None;
        }
        let key = GenerationKey::from_locator(locator);
        // A proof this process established outranks an admitted one; both are
        // held to the same exact-digest comparison.
        let proven = self
            .proven
            .lock()
            .ok()
            .and_then(|proven| proven.get(&key).cloned());
        let record = proven.or_else(|| self.admitted.get(&key).cloned())?;
        (record.recovered_digest == expected).then_some(record.canonical_bytes)
    }

    /// Records a proof this process established by streaming the rows.
    pub(crate) fn record_proven(
        &self,
        locator: &GenerationLocator,
        recovered_digest: &str,
        canonical_bytes: u64,
    ) {
        if let Ok(mut proven) = self.proven.lock() {
            proven.insert(
                GenerationKey::from_locator(locator),
                ProvenGeneration {
                    recovered_digest: recovered_digest.to_owned(),
                    canonical_bytes,
                },
            );
        }
    }

    /// Carries an admitted proof forward into the set that will be published.
    ///
    /// A marker hit is not a weaker fact than a full proof of the same bytes:
    /// it *is* that proof, established earlier over a container this open has
    /// confirmed is byte-identical. Without this, a daemon that starts, serves
    /// reads, and stops without publishing anything would drop every proof it
    /// inherited and make the next open re-derive all of them.
    pub(crate) fn record_fresh(&self, locator: &GenerationLocator) {
        let key = GenerationKey::from_locator(locator);
        let Some(record) = self.admitted.get(&key).cloned() else {
            return;
        };
        if let Ok(mut proven) = self.proven.lock() {
            proven.entry(key).or_insert(record);
        }
    }

    /// Writes the marker for the container as it now stands.
    ///
    /// Must run **after** the container has been closed and synced, so the
    /// identity recorded is the one the next open will observe. The stat is
    /// taken here rather than passed in for the same reason.
    ///
    /// The digests published were established against rows, not bytes: a
    /// generation proven earlier in this open is still proven now, because the
    /// container was re-serialized from an in-RAM store in which a sealed
    /// generation's rows never changed.
    pub(crate) fn publish(&self) -> io::Result<()> {
        if self.container.as_os_str().is_empty() {
            return Ok(());
        }
        let Ok(proven) = self.proven.lock() else {
            return Ok(());
        };
        if proven.is_empty() {
            return Ok(());
        }
        let Some(container) = ContainerIdentity::read(&self.container) else {
            return Ok(());
        };
        let body = MarkerBody {
            version: MARKER_VERSION,
            container,
            generations: proven
                .iter()
                .map(|(key, record)| VerifiedGenerationRecord {
                    namespace: key.namespace.clone(),
                    projection: key.projection.clone(),
                    generation: key.generation.clone(),
                    recovered_digest: record.recovered_digest.clone(),
                    canonical_bytes: record.canonical_bytes,
                })
                .collect(),
        };
        let Some(body_digest) = body.digest() else {
            return Ok(());
        };
        let encoded = serde_json::to_vec(&MarkerFile { body, body_digest })
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        tracedecay_private_fs::framed_log::atomic_write(
            &marker_path(&self.container),
            MARKER_TEMP_KIND,
            &encoded,
            tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(len: u64) -> ContainerIdentity {
        ContainerIdentity {
            device: 7,
            inode: 11,
            len,
            modified_seconds: 1_700_000_000,
            modified_nanoseconds: 123,
        }
    }

    fn body(len: u64, digest: &str) -> MarkerBody {
        MarkerBody {
            version: MARKER_VERSION,
            container: identity(len),
            generations: vec![VerifiedGenerationRecord {
                namespace: "ns".to_owned(),
                projection: "proj".to_owned(),
                generation: "gen".to_owned(),
                recovered_digest: digest.to_owned(),
                canonical_bytes: 42,
            }],
        }
    }

    fn write_marker(container: &Path, body: MarkerBody, body_digest: String) {
        let file = MarkerFile { body, body_digest };
        std::fs::write(marker_path(container), serde_json::to_vec(&file).unwrap()).unwrap();
    }

    #[test]
    fn a_marker_written_against_these_bytes_is_admitted() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let body = body(64, "sha256:abc");
        let digest = body.digest().unwrap();
        write_marker(&container, body, digest);

        let admitted = load(&container, identity(64));
        assert_eq!(admitted.len(), 1);
    }

    #[test]
    fn a_marker_written_against_different_bytes_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let body = body(64, "sha256:abc");
        let digest = body.digest().unwrap();
        write_marker(&container, body, digest);

        // Same inode, different length: the container grew since the proof.
        assert!(load(&container, identity(65)).is_empty());
    }

    /// Length plus mtime is not a file identity. A marker that recorded
    /// `(device, inode) = (0, 0)` -- the historical non-Unix fallback -- must
    /// miss even when the observed stat matches those zeros exactly. Otherwise
    /// a same-size replacement that preserved its timestamp would reuse the
    /// witness, which is what Windows shard 4 observed.
    #[test]
    fn a_zero_identity_marker_is_rejected_even_when_length_and_mtime_match() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let mut body = body(64, "sha256:abc");
        body.container.device = 0;
        body.container.inode = 0;
        let digest = body.digest().unwrap();
        write_marker(&container, body.clone(), digest);

        assert!(
            load(&container, body.container).is_empty(),
            "a marker without a durable file identity must not be believed"
        );
    }

    #[test]
    fn replacing_a_file_with_identical_bytes_and_mtime_changes_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.grafeo");
        std::fs::write(&path, [7_u8; 128]).unwrap();
        let first = ContainerIdentity::read(&path).expect("original identity");
        assert!(
            first.has_durable_file_id(),
            "a real container must expose a durable file identity, got {first:?}"
        );

        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let staged = path.with_extension("grafeo-copy");
        std::fs::copy(&path, &staged).unwrap();
        let staged_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&staged)
            .unwrap();
        staged_file.set_modified(original_mtime).unwrap();
        staged_file.sync_all().unwrap();
        drop(staged_file);
        std::fs::rename(&staged, &path).unwrap();

        let second = ContainerIdentity::read(&path).expect("replacement identity");
        assert!(second.has_durable_file_id());
        assert_eq!(first.len, second.len);
        assert_ne!(
            (first.device, first.inode),
            (second.device, second.inode),
            "a replaced container must carry a new file identity, got {first:?} then {second:?}"
        );
    }

    /// The forged-marker case. Swapping the recorded digest without recomputing
    /// the self-digest is rejected outright; recomputing it makes the marker
    /// well-formed but still useless, because `lookup` compares against the
    /// authority's expected digest.
    #[test]
    fn a_marker_whose_body_digest_does_not_bind_its_body_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let honest = body(64, "sha256:abc");
        let digest = honest.digest().unwrap();
        write_marker(&container, body(64, "sha256:forged"), digest);

        assert!(load(&container, identity(64)).is_empty());
    }

    #[test]
    fn an_unsorted_marker_is_rejected_so_the_encoding_stays_canonical() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let mut unsorted = body(64, "sha256:abc");
        unsorted.generations.push(VerifiedGenerationRecord {
            namespace: "aa".to_owned(),
            projection: "proj".to_owned(),
            generation: "gen".to_owned(),
            recovered_digest: "sha256:def".to_owned(),
            canonical_bytes: 1,
        });
        let digest = unsorted.digest().unwrap();
        write_marker(&container, unsorted, digest);

        assert!(load(&container, identity(64)).is_empty());
    }

    #[test]
    fn a_missing_marker_is_an_empty_set_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        assert!(load(&temp.path().join("graph.grafeo"), identity(64)).is_empty());
    }

    #[test]
    fn a_marker_is_not_consulted_once_the_container_has_been_mutated() {
        let markers = GenerationMarkers::detached();
        markers.record_proven(&super::tests::locator(), "sha256:abc", 10);
        // `detached` starts non-pristine, which is the same gate
        // `mark_container_mutated` sets.
        assert!(markers.lookup(&locator(), "sha256:abc").is_none());
    }

    pub(super) fn locator() -> GenerationLocator {
        GenerationLocator::new(
            crate::GraphProjectionIdentity {
                namespace: crate::GraphNamespace::new("ns").unwrap(),
                projection: crate::GraphProjectionId::new("proj").unwrap(),
            },
            crate::GraphGenerationId::new("gen").unwrap(),
        )
    }
}

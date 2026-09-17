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
//! * the container's identity -- the checkpoint generation the engine's own
//!   handle reported after the container was closed and synced (see
//!   [`ContainerIdentity`]), and
//! * for each generation proven against that container, the recovered digest
//!   that was proven and the number of canonical bytes the proof hashed.
//!
//! On a later open the identity is taken from the engine's own handle, the
//! moment the engine has opened the container, and compared against the
//! recorded identity. An exact match means the bytes that back the in-RAM
//! store are the bytes the proof already ran over, so the recorded digest
//! stands and the enumeration is skipped. Anything else -- a missing marker,
//! an unparseable one, a self-digest mismatch, an identity mismatch, an
//! engine that could not report its container, or a generation the marker
//! does not list -- falls back to the full proof.
//!
//! # One owner: the engine
//!
//! The native engine is the only thing that knows which container it loaded,
//! so it is the only thing allowed to say which container a proof is about.
//! Every identity in this module is read through the engine's handle
//! (`GrafeoDB::file_manager`), never through a path: a path-only stat taken
//! before the open, or after the close, names whichever file occupies the
//! path at that instant, and a replacement between that stat and the engine
//! open would let a marker written for one container vouch for another.
//!
//! The proofs therefore live on the *engine incarnation*: admitted when an
//! engine binds (eager open, lazy first use, reopen after hibernation),
//! consulted only while that engine is resident and pristine, and published
//! under the identity the same handle reports once that engine has closed.
//! A handle with no resident engine has no proofs to offer.
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
//! Container identity is a **format-integrity assumption, not a cryptographic
//! one**. The active header names a checkpoint by iteration, write time, and
//! the CRC-32 of its section directory, and that directory carries the
//! CRC-32 of every section, so any change to the rows that went through the
//! container format changes the identity. It does not detect bytes that
//! changed underneath an unchanged header -- neither silent bit rot nor an
//! adversary who rewrites sections and their checksums in place.
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
//! adversary who can write to the store directory while keeping the header
//! consistent. That adversary can already rewrite the marker, the container,
//! and -- being inside the daemon's private store -- the relational
//! authority's expected digest as well. The proof it would have skipped was
//! not defending against it either.
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
use std::sync::Mutex;

use grafeo_engine::GrafeoDB;
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

/// The identity of a `.grafeo` container as the engine that opened it reports
/// it: the file header's creation stamp, the active database header the
/// engine loaded from -- field for field -- and the container length, all read
/// through the engine's own file handle.
///
/// The active header names one checkpoint of one container: its iteration,
/// the time it was written, its row watermarks, and the CRC-32 of its section
/// directory, which in turn checksums every section. Two containers holding
/// different rows therefore report different identities, a checkpoint by any
/// process moves the identity, and -- because it is read from the engine's
/// handle -- it is the identity of the bytes the engine actually consumed,
/// whatever file the path pointed at a moment earlier or later.
///
/// This is the same on every platform: no device/inode or volume/file-index
/// is involved, so there is no per-OS identity path to keep honest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContainerIdentity {
    /// `FileHeader::creation_timestamp_ms`, written once at container creation.
    created_ms: u64,
    /// Active `DbHeader::iteration`: the checkpoint counter the engine loaded.
    iteration: u64,
    /// Active `DbHeader::checksum`: CRC-32 of the section directory, which
    /// carries every section's own CRC-32.
    checksum: u32,
    snapshot_length: u64,
    epoch: u64,
    transaction_id: u64,
    node_count: u64,
    edge_count: u64,
    /// Active `DbHeader::timestamp_ms`: when this checkpoint was written.
    written_ms: u64,
    directory_offset: u64,
    /// Container length as the engine's handle reports it.
    len: u64,
}

impl ContainerIdentity {
    /// The identity of the container `database` has open, read through the
    /// engine's own handle, or `None` when the engine has no container (an
    /// in-memory store) or cannot report its length. `None` is "no usable
    /// marker", never an error: failing to take a shortcut is not a failure.
    pub(crate) fn from_engine(database: &GrafeoDB) -> Option<Self> {
        let manager = database.file_manager()?;
        let header = manager.active_header();
        Some(Self {
            created_ms: manager.file_header().creation_timestamp_ms,
            iteration: header.iteration,
            checksum: header.checksum,
            snapshot_length: header.snapshot_length,
            epoch: header.epoch,
            transaction_id: header.transaction_id,
            node_count: header.node_count,
            edge_count: header.edge_count,
            written_ms: header.timestamp_ms,
            directory_offset: header.directory_offset,
            len: manager.file_size().ok()?,
        })
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
    // container this one is not.
    if marker.body.container != observed {
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

/// The proofs that apply to one resident engine incarnation.
///
/// Holds two things that never mix: `admitted`, the proofs a marker file
/// carried in against the identity this engine reported at open and that are
/// still believable, and `proven`, the proofs this incarnation established
/// itself. Only `proven` is ever published, because only those were
/// established against rows this process actually streamed -- with one
/// exception noted in `GenerationMarkers::record_fresh`.
struct BoundEngine {
    /// The container the engine loaded, as its own handle reported it.
    identity: ContainerIdentity,
    admitted: BTreeMap<GenerationKey, ProvenGeneration>,
    proven: BTreeMap<GenerationKey, ProvenGeneration>,
    /// False once anything has taken the exclusive claim on this database.
    ///
    /// The exclusive claim is this crate's documented gate for every write
    /// that rewrites the container, so clearing it here is the single point
    /// that stops `admitted` from being consulted once the store has diverged
    /// from the bytes the marker was written against.
    pristine: bool,
}

/// The marker set for one database handle.
///
/// Proofs are bound to an engine incarnation, not to the handle: they are
/// admitted when the engine binds the container it opened, consulted only
/// while that engine is resident, and published under the identity the same
/// handle reports after it closes. Between incarnations -- before a lazy
/// first use, or while hibernated -- there is nothing to consult, because
/// there is no engine whose container a proof could be about.
pub(crate) struct GenerationMarkers {
    /// The marker lives beside this container; `None` for an in-memory store.
    container: Option<PathBuf>,
    engine: Mutex<Option<BoundEngine>>,
}

impl GenerationMarkers {
    /// A marker set for a database handle with no engine bound yet.
    pub(crate) fn new(container: Option<&Path>) -> Self {
        Self {
            container: container.map(Path::to_path_buf),
            engine: Mutex::new(None),
        }
    }

    /// Binds a freshly opened engine incarnation.
    ///
    /// `identity` is what the engine's own handle reports for the container
    /// it just loaded; the marker beside the container is admitted only when
    /// it was written against exactly that identity. `None` -- an in-memory
    /// store, or an engine that could not report its container -- binds
    /// nothing, so every lookup misses and nothing is published.
    pub(crate) fn bind(&self, identity: Option<ContainerIdentity>) {
        let Ok(mut engine) = self.engine.lock() else {
            return;
        };
        *engine = identity.map(|identity| BoundEngine {
            identity,
            admitted: self
                .container
                .as_deref()
                .map(|container| load(container, identity))
                .unwrap_or_default(),
            proven: BTreeMap::new(),
            pristine: true,
        });
    }

    /// Notes that the exclusive claim was taken, retiring the admitted proofs
    /// for the rest of this engine incarnation.
    pub(crate) fn mark_container_mutated(&self) {
        if let Ok(mut engine) = self.engine.lock()
            && let Some(engine) = engine.as_mut()
        {
            engine.pristine = false;
        }
    }

    /// Looks up a completed proof of `expected` for `locator` against the
    /// container the resident engine opened.
    ///
    /// Returns the canonical byte count the original proof hashed, for the
    /// byte gauge, or `None` when the full proof has to run -- including
    /// whenever no engine is resident. The caller's `expected` digest --
    /// which comes from the relational authority, never from the marker --
    /// must match exactly.
    pub(crate) fn lookup(&self, locator: &GenerationLocator, expected: &str) -> Option<u64> {
        let engine = self.engine.lock().ok()?;
        let engine = engine.as_ref().filter(|engine| engine.pristine)?;
        let key = GenerationKey::from_locator(locator);
        // A proof this process established outranks an admitted one; both are
        // held to the same exact-digest comparison.
        let record = engine
            .proven
            .get(&key)
            .or_else(|| engine.admitted.get(&key))?;
        (record.recovered_digest == expected).then_some(record.canonical_bytes)
    }

    /// Records a proof this process established by streaming the rows of the
    /// resident engine. With no engine resident there is no container the
    /// proof could be filed against, so it is dropped; the next open pays a
    /// proof and nothing else.
    pub(crate) fn record_proven(
        &self,
        locator: &GenerationLocator,
        recovered_digest: &str,
        canonical_bytes: u64,
    ) {
        if let Ok(mut engine) = self.engine.lock()
            && let Some(engine) = engine.as_mut()
        {
            engine.proven.insert(
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
    /// it *is* that proof, established earlier over a container this engine
    /// has confirmed is byte-identical. Without this, a daemon that starts,
    /// serves reads, and stops without publishing anything would drop every
    /// proof it inherited and make the next open re-derive all of them.
    pub(crate) fn record_fresh(&self, locator: &GenerationLocator) {
        if let Ok(mut engine) = self.engine.lock()
            && let Some(engine) = engine.as_mut()
        {
            let key = GenerationKey::from_locator(locator);
            if let Some(record) = engine.admitted.get(&key).cloned() {
                engine.proven.entry(key).or_insert(record);
            }
        }
    }

    /// Writes the resident engine's proofs under the identity it opened.
    ///
    /// For a read-only engine the container never moves while it is open, so
    /// the identity bound at open is the one a concurrent open of the same
    /// artifact observes; publishing mid-life lets that open resolve by
    /// marker instead of re-streaming the rows.
    pub(crate) fn publish_resident(&self) -> io::Result<()> {
        let Ok(engine) = self.engine.lock() else {
            return Ok(());
        };
        let Some(engine) = engine.as_ref() else {
            return Ok(());
        };
        self.write(engine.identity, &engine.proven)
    }

    /// Releases the engine incarnation, publishing its proofs under `closed`.
    ///
    /// `closed` is what the engine's own handle reports **after** the engine
    /// has closed and synced the container, so the identity recorded is the
    /// one the next open will read from the same bytes. `None` -- an
    /// uncertain close, or an engine that could not report its container --
    /// releases the incarnation without publishing.
    ///
    /// The digests published were established against rows, not bytes: a
    /// generation proven earlier in this incarnation is still proven now,
    /// because the container was re-serialized from an in-RAM store in which
    /// a sealed generation's rows never changed.
    pub(crate) fn release(&self, closed: Option<ContainerIdentity>) -> io::Result<()> {
        let Ok(mut engine) = self.engine.lock() else {
            return Ok(());
        };
        let Some(engine) = engine.take() else {
            return Ok(());
        };
        match closed {
            Some(closed) => self.write(closed, &engine.proven),
            None => Ok(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn bound_identity(&self) -> Option<ContainerIdentity> {
        self.engine
            .lock()
            .ok()
            .and_then(|engine| engine.as_ref().map(|engine| engine.identity))
    }

    fn write(
        &self,
        container: ContainerIdentity,
        proven: &BTreeMap<GenerationKey, ProvenGeneration>,
    ) -> io::Result<()> {
        let Some(path) = self.container.as_deref() else {
            return Ok(());
        };
        if proven.is_empty() {
            return Ok(());
        }
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
            &marker_path(path),
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
            created_ms: 1_700_000_000_000,
            iteration: 3,
            checksum: 0x1234_5678,
            snapshot_length: 0,
            epoch: 9,
            transaction_id: 27,
            node_count: 12,
            edge_count: 4,
            written_ms: 1_700_000_100_000,
            directory_offset: 16_384,
            len,
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
    fn a_marker_written_against_different_bytes_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let body = body(64, "sha256:abc");
        let digest = body.digest().unwrap();
        write_marker(&container, body, digest);

        // Same header, different length: the container grew since the proof.
        assert!(load(&container, identity(65)).is_empty());
    }

    /// A marker written against a different checkpoint of the same container
    /// -- same creation stamp, same length, later iteration -- is a miss: the
    /// rows behind the header the engine loaded are not the rows proven.
    #[test]
    fn a_marker_for_another_checkpoint_of_the_same_container_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let body = body(64, "sha256:abc");
        let digest = body.digest().unwrap();
        write_marker(&container, body, digest);

        let mut checkpointed = identity(64);
        checkpointed.iteration += 1;
        checkpointed.checksum ^= 1;
        assert!(load(&container, checkpointed).is_empty());
    }

    /// A marker in the shape an earlier build wrote -- an OS file identity
    /// instead of the engine-reported checkpoint -- is a miss, not an error.
    #[test]
    fn a_marker_carrying_a_foreign_identity_shape_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let honest = body(64, "sha256:abc");
        let digest = honest.digest().unwrap();
        let mut parsed = serde_json::to_value(MarkerFile {
            body: honest,
            body_digest: digest,
        })
        .unwrap();
        parsed["body"]["container"] = serde_json::json!({
            "device": 7,
            "inode": 11,
            "len": 64,
            "modified_seconds": 1_700_000_000,
            "modified_nanoseconds": 123,
        });
        std::fs::write(
            marker_path(&container),
            serde_json::to_vec(&parsed).unwrap(),
        )
        .unwrap();

        assert!(load(&container, identity(64)).is_empty());
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
        let markers = GenerationMarkers::new(None);
        markers.bind(Some(identity(64)));
        markers.record_proven(&locator(), "sha256:abc", 10);
        assert_eq!(markers.lookup(&locator(), "sha256:abc"), Some(10));

        markers.mark_container_mutated();
        assert!(markers.lookup(&locator(), "sha256:abc").is_none());
    }

    /// Proofs belong to the engine incarnation that established them. With no
    /// engine bound there is no container a proof could be about, so nothing
    /// is consulted, recorded, or published.
    #[test]
    fn proofs_are_consulted_only_while_an_engine_is_bound() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let markers = GenerationMarkers::new(Some(&container));

        markers.record_proven(&locator(), "sha256:abc", 10);
        assert!(markers.lookup(&locator(), "sha256:abc").is_none());
        markers.publish_resident().unwrap();
        assert!(!marker_path(&container).exists());

        markers.bind(Some(identity(64)));
        markers.record_proven(&locator(), "sha256:abc", 10);
        assert_eq!(markers.lookup(&locator(), "sha256:abc"), Some(10));

        // Releasing publishes under the identity the closed engine reports,
        // and leaves nothing to consult until the next engine binds.
        markers.release(Some(identity(65))).unwrap();
        assert!(markers.lookup(&locator(), "sha256:abc").is_none());
        assert_eq!(load(&container, identity(65)).len(), 1);
        assert!(load(&container, identity(64)).is_empty());

        markers.bind(Some(identity(65)));
        assert_eq!(markers.lookup(&locator(), "sha256:abc"), Some(10));
        markers.bind(None);
        assert!(markers.lookup(&locator(), "sha256:abc").is_none());
    }

    /// An uncertain close releases the incarnation without publishing: proofs
    /// established against an engine whose final bytes are unknown are not
    /// recorded against any identity.
    #[test]
    fn releasing_without_an_identity_publishes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        let markers = GenerationMarkers::new(Some(&container));
        markers.bind(Some(identity(64)));
        markers.record_proven(&locator(), "sha256:abc", 10);

        markers.release(None).unwrap();
        assert!(!marker_path(&container).exists());
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

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, sync_channel};
use std::time::Duration;

use grafeo_common::types::{ArcStr, NodeId};
use grafeo_core::graph::GraphStore;
use grafeo_engine::GrafeoDB;
use rayon::Yield;
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_store::runtime::MAX_GRAPH_REPLAY_SOURCE_BYTES_V1;

use crate::schema::decode_entity;
use crate::state::{
    EndpointIdentityCache, load_relation_by_locator_cached, projection_entity_nodes_sorted_checked,
    projection_relation_nodes_sorted_checked,
};
use crate::{GraphDbError, GraphNamespace};

use super::{
    CheckedDigestWriter, CheckedVecWriter, GraphEntityRef, GraphGenerationManifestIdentity,
    GraphGenerationRelation, GraphProjectionIdentity, RowLanes, frame_length_headers,
    physical_namespace_projection_map, recovered_entity_ref, row_frame_lanes,
    write_canonical_row_frame, write_generation_identity_frames,
};

/// Rows per encode chunk. Sized so one chunk is a few milliseconds of decode
/// and canonicalization work: small enough that cancellation and error
/// propagation stay responsive and per-chunk endpoint memos still catch hub
/// reuse, large enough that thread handoff is noise.
const PROOF_CHUNK_ROWS: usize = 512;

/// Ceiling on proof encode workers. The framed stream is hashed by exactly
/// one consumer (the digest is a single ordered SHA-256), so past the point
/// where parallel decode+encode saturates that consumer, more workers only
/// contend with the rest of a shared host. Eight covers that crossover on
/// the measured production row shapes with headroom.
const PROOF_MAX_WORKERS: usize = 8;

/// Rebuilds the recovered-generation digest by streaming the stored rows.
///
/// Takes only the manifest's identity: every entity and relation frame comes
/// from the database, never from an in-memory manifest row. That is what lets
/// publication release the staged bulk rows before this proof runs.
///
/// Returns the digest and the number of canonical bytes it hashed. The byte
/// count is what the verify outcome event reports and what a verified-generation
/// marker records, so a later marker hit can report the same magnitude of work
/// it avoided.
///
/// A generation larger than one chunk is proven through a bounded parallel
/// pipeline: workers decode rows and canonicalize frames in sorted-chunk
/// order while the calling thread hashes completed chunks strictly in order,
/// so the digest bytes are identical to the serial stream. Cancellation is
/// polled on the calling thread at least once per row plus every hashed
/// 64 KiB, exactly as before. In-flight chunk buffers are bounded (at most
/// `PROOF_MAX_WORKERS` encoded chunks outstanding), so verification memory
/// stays a small constant over the one-decoded-row posture of the serial
/// path. Generations at or below one chunk keep the strictly serial
/// single-pass stream.
///
/// Each row costs exactly one storage load: entities decode straight from
/// their enumerated node, and relation endpoints memoize their identity refs
/// per chunk so a hub entity resolves once per chunk instead of once per
/// incident relation. The digest comparison in `verify_recovered_generation`
/// is the content authority for this proof; per-row unique-key index
/// round-trips contributed no bytes to it and are deliberately absent.
#[tracing::instrument(name = "graph_db.generation.recover.digest", level = "trace", skip_all)]
pub(crate) fn recovered_generation_digest_from_database(
    database: &GrafeoDB,
    identity: &GraphGenerationManifestIdentity,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(String, u64), GraphDbError> {
    recovered_generation_digest_chunked(database, identity, check, PROOF_CHUNK_ROWS)
}

/// The chunk size is a parameter so tests can force the parallel pipeline
/// over small fixtures and pin its digest against the serial stream (and so
/// the cost probe can time the serial stream on generation-scale rows).
pub(crate) fn recovered_generation_digest_chunked(
    database: &GrafeoDB,
    identity: &GraphGenerationManifestIdentity,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    chunk_rows: usize,
) -> Result<(String, u64), GraphDbError> {
    let chunk_rows = chunk_rows.max(1);
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    let mut canonical = CheckedVecWriter::new(check, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    write_generation_identity_frames(
        &mut writer,
        &mut canonical,
        &identity.projection,
        &identity.generation,
        &identity.source_generation,
        &identity.watermark,
        &identity.dependencies,
    )?;

    let store = database.graph_store();
    let physical_namespace = identity.physical_namespace()?;
    let entities = projection_entity_nodes_sorted_checked(
        database,
        &physical_namespace,
        &identity.projection.projection,
        check,
    )?;
    let relations = projection_relation_nodes_sorted_checked(
        database,
        &physical_namespace,
        &identity.projection.projection,
        check,
    )?;
    let namespace_projection = physical_namespace_projection_map(identity)?;

    // Publication already runs under `parallelism::install`, so this proof
    // is almost always on a Rayon worker. `digest_rows_parallel` drains its
    // chunk channels cooperatively, so that worker runs queued encode work
    // between polls instead of parking on a channel receive while its tasks
    // sit queued on the same pool. Forcing the serial stream here made the
    // production caller hash every row on one thread (measured 2.57s serial
    // vs a parallel encode path that already exists).
    if entities.len().saturating_add(relations.len()) <= chunk_rows {
        digest_rows_serial(
            store.as_ref(),
            &entities,
            &relations,
            &namespace_projection,
            &mut writer,
            &mut canonical,
            check,
        )?;
    } else {
        digest_rows_parallel(
            store,
            &entities,
            &relations,
            &namespace_projection,
            &mut writer,
            check,
            chunk_rows,
        )?;
    }
    let canonical_bytes = writer.total_bytes();
    writer.finish()?;
    Ok((encode_lowercase_hex(&digest.finalize()), canonical_bytes))
}

/// Streams each stored relation of `identity`'s generation to `emit` with
/// the row-sum lanes of its frame as `identity` recovers it, in identity
/// order. A relation's frame names its endpoints' projection, so the same
/// stored rows hash differently under each projection that reads them.
pub(crate) fn recovered_relation_lanes(
    database: &GrafeoDB,
    identity: &GraphGenerationManifestIdentity,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    emit: &mut dyn FnMut(&str, RowLanes) -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let store = database.graph_store();
    let relations = projection_relation_nodes_sorted_checked(
        database,
        &identity.physical_namespace()?,
        &identity.projection.projection,
        check,
    )?;
    let namespace_projection = physical_namespace_projection_map(identity)?;
    let mut canonical = CheckedVecWriter::new(check, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    let mut endpoints = EndpointIdentityCache::default();
    let mut endpoint_refs = HashMap::new();
    for (sorted_identity, locator) in &relations {
        check()?;
        let relation = decode_sorted_relation(
            store.as_ref(),
            sorted_identity,
            *locator,
            &namespace_projection,
            &mut endpoints,
            &mut endpoint_refs,
        )?;
        let bytes = canonical.encode(&relation, "recovered generation relation")?;
        emit(
            sorted_identity.as_str(),
            row_frame_lanes("relation", bytes)?,
        )?;
    }
    Ok(())
}

/// The single-pass stream for generations at or below one chunk: one decoded
/// row resident at a time, every frame hashed as it is encoded.
#[tracing::instrument(
    name = "graph_db.generation.recover.digest_serial",
    level = "trace",
    skip_all
)]
fn digest_rows_serial(
    store: &dyn GraphStore,
    entities: &[(ArcStr, NodeId)],
    relations: &[(ArcStr, NodeId)],
    namespace_projection: &BTreeMap<GraphNamespace, GraphProjectionIdentity>,
    writer: &mut CheckedDigestWriter<'_>,
    canonical: &mut CheckedVecWriter<'_>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    for (sorted_identity, node) in entities {
        check()?;
        let entity = decode_sorted_entity(store, sorted_identity, *node)?;
        write_canonical_row_frame(
            writer,
            canonical,
            "entity",
            &entity,
            "recovered generation entity",
        )?;
    }
    let mut endpoints = EndpointIdentityCache::default();
    let mut endpoint_refs = HashMap::new();
    for (sorted_identity, locator) in relations {
        check()?;
        let relation = decode_sorted_relation(
            store,
            sorted_identity,
            *locator,
            namespace_projection,
            &mut endpoints,
            &mut endpoint_refs,
        )?;
        write_canonical_row_frame(
            writer,
            canonical,
            "relation",
            &relation,
            "recovered generation relation",
        )?;
    }
    Ok(())
}

/// One sorted slice of rows handed to an encode worker.
#[derive(Clone, Copy)]
enum ProofChunk<'a> {
    Entities(&'a [(ArcStr, NodeId)]),
    Relations(&'a [(ArcStr, NodeId)]),
}

/// The framed canonical bytes of one encoded chunk, with each row's frame
/// end recorded so the consumer can poll cancellation per row while hashing.
struct EncodedProofChunk {
    buffer: Vec<u8>,
    frame_ends: Vec<usize>,
}

impl EncodedProofChunk {
    fn with_rows(rows: usize) -> Self {
        Self {
            buffer: Vec::new(),
            frame_ends: Vec::with_capacity(rows),
        }
    }

    /// Appends one frame in exactly the layout `write_frame` streams:
    /// `tag_len | tag | byte_len | bytes`, lengths big-endian u64.
    fn push_frame(&mut self, tag: &str, bytes: &[u8]) -> Result<(), GraphDbError> {
        let (tag_len, byte_len) = frame_length_headers(tag, bytes)?;
        self.buffer.extend_from_slice(&tag_len);
        self.buffer.extend_from_slice(tag.as_bytes());
        self.buffer.extend_from_slice(&byte_len);
        self.buffer.extend_from_slice(bytes);
        self.frame_ends.push(self.buffer.len());
        Ok(())
    }
}

/// Bounded parallel proof: workers decode and canonicalize sorted chunks,
/// the calling thread hashes completed chunks strictly in chunk order.
///
/// Chunks encode as tasks on the persistent Rayon pool, never on threads
/// created per chunk, while this thread hashes the oldest chunk in order.
/// Each result opens one slot for the next sorted chunk, so at most
/// `PROOF_MAX_WORKERS` chunk buffers exist, and the drain is ordered, so
/// frames enter the digest in exactly the serial order and the first error
/// to surface is the earliest failing chunk, the same row a serial
/// enumeration would have failed on. `check` runs only on the calling
/// thread (it is not required to be `Sync`) at least once per hashed row,
/// so a spent deadline or a failed check stops the proof while later
/// chunks are still encoding; the shared abort flag then stops in-flight
/// encodes between rows instead of letting them finish stale work.
///
/// The drain runs queued local work between polls so a Rayon-worker caller
/// — the publication lane almost always is — still makes progress on a
/// one-worker or saturated pool. When `yield_local` reports `Idle` or the
/// caller is not a pool worker, it parks on a bounded receive instead of
/// spinning, and `check` still runs while the oldest encode is outstanding.
#[tracing::instrument(
    name = "graph_db.generation.recover.digest_parallel",
    level = "trace",
    skip_all
)]
fn digest_rows_parallel(
    store: Arc<dyn GraphStore>,
    entities: &[(ArcStr, NodeId)],
    relations: &[(ArcStr, NodeId)],
    namespace_projection: &BTreeMap<GraphNamespace, GraphProjectionIdentity>,
    writer: &mut CheckedDigestWriter<'_>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    chunk_rows: usize,
) -> Result<(), GraphDbError> {
    let chunks: Vec<ProofChunk<'_>> = entities
        .chunks(chunk_rows)
        .map(ProofChunk::Entities)
        .chain(relations.chunks(chunk_rows).map(ProofChunk::Relations))
        .collect();
    let workers = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(PROOF_MAX_WORKERS)
        .min(chunks.len())
        .max(1);
    let in_flight = workers;
    let abort = AtomicBool::new(false);
    rayon::in_place_scope(|scope| {
        let mut pending = VecDeque::with_capacity(in_flight);
        let mut next_chunk = 0usize;
        let result = (|| -> Result<(), GraphDbError> {
            loop {
                while pending.len() < in_flight && next_chunk < chunks.len() {
                    let chunk = chunks[next_chunk];
                    let store = Arc::clone(&store);
                    let abort = &abort;
                    let namespace_projection = &*namespace_projection;
                    let (sender, receiver) = sync_channel(1);
                    scope.spawn(move |_| {
                        let encoded = catch_unwind(AssertUnwindSafe(|| {
                            encode_proof_chunk(store.as_ref(), chunk, namespace_projection, abort)
                        }));
                        // The consumer stops listening only after it failed;
                        // its error is the one reported.
                        let _ = sender.send(encoded);
                    });
                    pending.push_back(receiver);
                    next_chunk += 1;
                }
                let Some(receiver) = pending.pop_front() else {
                    return Ok(());
                };
                let encoded = recv_while_working(&receiver, check)?.unwrap_or_else(|_| {
                    Err(GraphDbError::unavailable(
                        "recovered generation verification worker panicked",
                    ))
                })?;
                let mut start = 0usize;
                for &end in &encoded.frame_ends {
                    check()?;
                    writer.add_row_frame(&[&encoded.buffer[start..end]])?;
                    start = end;
                }
            }
        })();
        if result.is_err() {
            // Outstanding tasks see the flag between rows and stop; the scope
            // waits for them before returning.
            abort.store(true, Ordering::Release);
        }
        result
    })
}

/// How long the calling thread parks on a receive once its local work deque
/// is empty. Sends wake the park early, so the bound only caps the retry
/// interval after a spurious empty poll.
const WORKER_RECV_PARK: Duration = Duration::from_millis(1);

/// Receives a worker result without parking the calling thread when it is a
/// Rayon worker whose own queued tasks include the pending encodes: between
/// polls it runs local work, which is what lets a one-worker pool (or a
/// pool whose workers are all digest callers) complete a proof instead of
/// deadlocking on a receive that its own queued tasks would have to answer.
///
/// In rayon-core 1.13.0 a pool worker with an empty local deque returns
/// `Some(Yield::Idle)`; only a non-worker returns `None`. Both mean there
/// is no local encode to run, so the caller takes the bounded receive
/// instead of spinning. `Some(Yield::Executed)` stays on the immediate
/// poll path so the caller keeps participating in its own queued work.
///
/// `check` runs on every empty poll, including while the oldest encode is
/// still outstanding, so a spent deadline or cancellation exits through
/// the scope path that raises the shared abort flag.
fn recv_while_working<T>(
    receiver: &Receiver<T>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<T, GraphDbError> {
    loop {
        match receiver.try_recv() {
            Ok(value) => return Ok(value),
            Err(TryRecvError::Disconnected) => {
                return Err(GraphDbError::unavailable(
                    "recovered generation verification worker panicked",
                ));
            }
            Err(TryRecvError::Empty) => {
                check()?;
                match rayon::yield_local() {
                    Some(Yield::Executed) => {}
                    Some(Yield::Idle) | None => match receiver.recv_timeout(WORKER_RECV_PARK) {
                        Ok(value) => return Ok(value),
                        Err(RecvTimeoutError::Disconnected) => {
                            return Err(GraphDbError::unavailable(
                                "recovered generation verification worker panicked",
                            ));
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                    },
                }
            }
        }
    }
}

fn encode_proof_chunk(
    store: &dyn GraphStore,
    chunk: ProofChunk<'_>,
    namespace_projection: &BTreeMap<GraphNamespace, GraphProjectionIdentity>,
    abort: &AtomicBool,
) -> Result<EncodedProofChunk, GraphDbError> {
    let worker_check = || {
        if abort.load(Ordering::Acquire) {
            Err(GraphDbError::Cancelled)
        } else {
            Ok(())
        }
    };
    let mut canonical = CheckedVecWriter::new(&worker_check, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    match chunk {
        ProofChunk::Entities(rows) => {
            let mut encoded = EncodedProofChunk::with_rows(rows.len());
            for (sorted_identity, node) in rows {
                worker_check()?;
                let entity = decode_sorted_entity(store, sorted_identity, *node)?;
                let bytes = canonical.encode(&entity, "recovered generation entity")?;
                encoded.push_frame("entity", bytes)?;
            }
            Ok(encoded)
        }
        ProofChunk::Relations(rows) => {
            let mut encoded = EncodedProofChunk::with_rows(rows.len());
            let mut endpoints = EndpointIdentityCache::default();
            let mut endpoint_refs = HashMap::new();
            for (sorted_identity, locator) in rows {
                worker_check()?;
                let relation = decode_sorted_relation(
                    store,
                    sorted_identity,
                    *locator,
                    namespace_projection,
                    &mut endpoints,
                    &mut endpoint_refs,
                )?;
                let bytes = canonical.encode(&relation, "recovered generation relation")?;
                encoded.push_frame("relation", bytes)?;
            }
            Ok(encoded)
        }
    }
}

/// Loads and decodes one enumerated entity, refusing a row whose identity no
/// longer matches its sort key: a divergence means the row changed under the
/// enumeration and the frames would no longer be hashed in sorted order.
fn decode_sorted_entity(
    store: &dyn GraphStore,
    sorted_identity: &ArcStr,
    node: NodeId,
) -> Result<crate::GraphEntity, GraphDbError> {
    let record = store.get_node(node).ok_or_else(|| GraphDbError::Corrupt {
        message: "recovered generation entity disappeared during verification".to_owned(),
    })?;
    let entity = decode_entity(&record)?;
    if entity.identity.as_str() != sorted_identity.as_str() {
        return Err(GraphDbError::Corrupt {
            message: "recovered generation entity identity does not match its enumeration"
                .to_owned(),
        });
    }
    Ok(entity)
}

/// Loads and decodes one enumerated relation with memoized endpoint refs,
/// under the same sort-key refusal as entities.
fn decode_sorted_relation(
    store: &dyn GraphStore,
    sorted_identity: &ArcStr,
    locator: NodeId,
    namespace_projection: &BTreeMap<GraphNamespace, GraphProjectionIdentity>,
    endpoints: &mut EndpointIdentityCache,
    endpoint_refs: &mut HashMap<NodeId, GraphEntityRef>,
) -> Result<GraphGenerationRelation, GraphDbError> {
    let stored = load_relation_by_locator_cached(store, locator, endpoints)?;
    if stored.relation.identity.as_str() != sorted_identity.as_str() {
        return Err(GraphDbError::Corrupt {
            message: "recovered generation relation identity does not match its enumeration"
                .to_owned(),
        });
    }
    let from = memoized_endpoint_ref(store, endpoint_refs, stored.source, namespace_projection)?;
    let to = memoized_endpoint_ref(store, endpoint_refs, stored.target, namespace_projection)?;
    GraphGenerationRelation::new(
        stored.relation.identity,
        from,
        to,
        stored.relation.kind,
        stored.relation.properties,
    )
}

/// Resolves one relation endpoint to its `GraphEntityRef`, memoized by
/// `NodeId` for the duration of one chunk (parallel) or one enumeration
/// (serial).
///
/// Hub entities are endpoints of many relations; without the memo every
/// incident relation re-loads the full endpoint node, all properties
/// included, just to extract two identity strings. The memo stores
/// identity-sized refs only, never entity rows, so the verification memory
/// posture is preserved while each distinct endpoint is read at most once
/// per memo scope.
fn memoized_endpoint_ref(
    store: &dyn GraphStore,
    memo: &mut HashMap<NodeId, GraphEntityRef>,
    node: NodeId,
    namespace_projection: &BTreeMap<GraphNamespace, GraphProjectionIdentity>,
) -> Result<GraphEntityRef, GraphDbError> {
    if let Some(reference) = memo.get(&node) {
        return Ok(reference.clone());
    }
    let reference = recovered_entity_ref(store, node, namespace_projection)?;
    memo.insert(node, reference.clone());
    Ok(reference)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    use super::{recovered_generation_digest_chunked, recovered_generation_digest_from_database};
    use crate::{
        GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability,
        GraphEntity, GraphEntityId, GraphEntityRef, GraphFormatVersion, GraphGenerationId,
        GraphGenerationManifest, GraphGenerationRelation, GraphLabel, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
        GraphRelationId, GraphRelationKind, GraphWatermark, NeverCancelled, SourceGeneration,
    };

    fn property_name(name: &str) -> GraphPropertyName {
        GraphPropertyName::new(name).unwrap()
    }

    fn entity_identity(index: u32) -> GraphEntityId {
        GraphEntityId::new(format!("entity:{index:04}")).unwrap()
    }

    /// A generation whose digest exercises every frame ingredient: entities
    /// inserted in reverse identity order with domain labels and every scalar
    /// property type, plus a hub-heavy relation topology so endpoint nodes
    /// repeat across many relations.
    fn fixture_manifest() -> GraphGenerationManifest {
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new("recovered-digest-probe").unwrap(),
            GraphProjectionId::new("code").unwrap(),
        );
        let entities = (0..96_u32)
            .rev()
            .map(|index| {
                GraphEntity::new(
                    entity_identity(index),
                    BTreeSet::from([
                        GraphLabel::new("function").unwrap(),
                        GraphLabel::new(format!("bucket-{}", index % 7)).unwrap(),
                    ]),
                    BTreeMap::from([
                        (
                            property_name("name"),
                            GraphProperty::String(format!("symbol_{index}")),
                        ),
                        (
                            property_name("arity"),
                            GraphProperty::I64(i64::from(index % 5)),
                        ),
                        (
                            property_name("exported"),
                            GraphProperty::Bool(index % 2 == 0),
                        ),
                        (
                            property_name("score"),
                            GraphProperty::F64(f64::from(index) / 3.0),
                        ),
                        (
                            property_name("fingerprint"),
                            GraphProperty::Bytes(index.to_be_bytes().to_vec()),
                        ),
                    ]),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let entity_ref =
            |index: u32| GraphEntityRef::new(projection.clone(), entity_identity(index));
        let mut relations = Vec::new();
        for index in 1..96_u32 {
            relations.push(
                GraphGenerationRelation::new(
                    GraphRelationId::new(format!("relation:hub:{index:04}")).unwrap(),
                    entity_ref(index),
                    entity_ref(0),
                    GraphRelationKind::new("calls").unwrap(),
                    BTreeMap::from([(
                        property_name("weight"),
                        GraphProperty::I64(i64::from(index)),
                    )]),
                )
                .unwrap(),
            );
        }
        for index in 1..95_u32 {
            relations.push(
                GraphGenerationRelation::new(
                    GraphRelationId::new(format!("relation:chain:{index:04}")).unwrap(),
                    entity_ref(index),
                    entity_ref(index + 1),
                    GraphRelationKind::new("references").unwrap(),
                    BTreeMap::new(),
                )
                .unwrap(),
            );
        }
        GraphGenerationManifest::new(
            projection,
            GraphGenerationId::new("generation-digest-probe").unwrap(),
            SourceGeneration::new("source-digest-probe").unwrap(),
            GraphWatermark::new("watermark-digest-probe").unwrap(),
            vec![],
            entities,
            relations,
        )
        .unwrap()
    }

    fn staged_database() -> (GraphDbOwner, crate::GraphDbLeaseV1, GraphGenerationManifest) {
        let manifest = fixture_manifest();
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        database
            .apply_generation_unverified(Arc::new(manifest.clone()), &|| Ok(()))
            .unwrap();
        (owner, database, manifest)
    }

    /// The manifest canonicalization in `generation.rs` is the untouched
    /// digest authority every publication pinned; the streamed enumeration
    /// must reproduce it byte for byte from the stored rows alone.
    #[test]
    fn streamed_digest_is_byte_identical_to_the_manifest_digest() {
        let (_owner, database, manifest) = staged_database();
        let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let guard = database.read_guard().unwrap();
        let native = guard.as_ref().unwrap();

        let (streamed, _canonical_bytes) =
            recovered_generation_digest_from_database(native, &manifest.identity(), &|| Ok(()))
                .unwrap();

        assert_eq!(format!("sha256:{streamed}"), expected.as_str());
    }

    /// The parallel pipeline must reproduce the serial stream exactly,
    /// digest and counted canonical bytes, across chunk sizes that split
    /// entities and relations mid-slice and scatter the hub endpoints over
    /// many per-chunk memos.
    #[test]
    fn parallel_digest_is_byte_identical_to_the_serial_stream() {
        let (_owner, database, manifest) = staged_database();
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let guard = database.read_guard().unwrap();
        let native = guard.as_ref().unwrap();

        let (serial, serial_bytes) =
            recovered_generation_digest_chunked(native, &identity, &|| Ok(()), usize::MAX).unwrap();
        assert_eq!(format!("sha256:{serial}"), expected.as_str());

        for chunk_rows in [1, 7, 32, 96] {
            let (parallel, parallel_bytes) =
                recovered_generation_digest_chunked(native, &identity, &|| Ok(()), chunk_rows)
                    .unwrap();
            assert_eq!(
                parallel, serial,
                "chunked digest diverged at chunk_rows={chunk_rows}"
            );
            assert_eq!(
                parallel_bytes, serial_bytes,
                "counted canonical bytes diverged at chunk_rows={chunk_rows}"
            );
        }
    }

    #[test]
    fn streamed_digest_cancels_mid_enumeration() {
        let (_owner, database, manifest) = staged_database();
        let identity = manifest.identity();
        let guard = database.read_guard().unwrap();
        let native = guard.as_ref().unwrap();

        let total_polls = Cell::new(0_usize);
        let counting = || {
            total_polls.set(total_polls.get() + 1);
            Ok(())
        };
        recovered_generation_digest_from_database(native, &identity, &counting).unwrap();
        let total = total_polls.get();
        // The enumeration polls at least once per row; 96 entities and 190
        // relations put any mid-stream trip point far past the handful of
        // polls the leading identity frames consume.
        assert!(total > 300, "expected row-driven poll cadence, saw {total}");

        let cancel_at = total / 2;
        let polls = Cell::new(0_usize);
        let cancelling = || {
            let poll = polls.get() + 1;
            polls.set(poll);
            if poll >= cancel_at {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            recovered_generation_digest_from_database(native, &identity, &cancelling),
            Err(GraphDbError::Cancelled)
        ));
        assert!(
            polls.get() < total,
            "cancellation must stop the enumeration early: {} polls of {total}",
            polls.get()
        );
    }

    /// Cancellation under the parallel pipeline still trips on the calling
    /// thread's own check, the closure is never shared with workers, and
    /// stops the run early with the typed error.
    #[test]
    fn parallel_digest_cancels_mid_stream() {
        let (_owner, database, manifest) = staged_database();
        let identity = manifest.identity();
        let guard = database.read_guard().unwrap();
        let native = guard.as_ref().unwrap();

        let total_polls = Cell::new(0_usize);
        let counting = || {
            total_polls.set(total_polls.get() + 1);
            Ok(())
        };
        recovered_generation_digest_chunked(native, &identity, &counting, 16).unwrap();
        let total = total_polls.get();
        // The consumer polls at least once per hashed row: 286 rows put the
        // cadence well past the identity-frame polls.
        assert!(
            total > 286,
            "expected row-driven consumer poll cadence, saw {total}"
        );

        let cancel_at = total / 2;
        let polls = Cell::new(0_usize);
        let cancelling = || {
            let poll = polls.get() + 1;
            polls.set(poll);
            if poll >= cancel_at {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            recovered_generation_digest_chunked(native, &identity, &cancelling, 16),
            Err(GraphDbError::Cancelled)
        ));
        assert!(
            polls.get() < total,
            "cancellation must stop the pipeline early: {} polls of {total}",
            polls.get()
        );
    }

    /// Publication already sits on a Rayon worker via `parallelism::install`.
    /// The recovered digest must still take the in-place parallel encode path
    /// there; forcing serial because `current_thread_index` is `Some` was the
    /// measured mis-sized work on a Rspack-sized generation.
    #[test]
    fn parallel_digest_from_a_rayon_worker_matches_the_serial_stream() {
        let (_owner, database, manifest) = staged_database();
        let identity = manifest.identity();
        let guard = database.read_guard().unwrap();
        let native = guard.as_ref().unwrap();

        let (serial, serial_bytes) =
            recovered_generation_digest_chunked(native, &identity, &|| Ok(()), usize::MAX).unwrap();

        let (parallel, parallel_bytes) = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .expect("rayon pool")
            .install(|| recovered_generation_digest_chunked(native, &identity, &|| Ok(()), 16))
            .expect("parallel digest on a rayon worker");

        assert_eq!(parallel, serial);
        assert_eq!(parallel_bytes, serial_bytes);

        // A one-worker indexing pool is a supported configuration: the sole
        // worker cannot run tasks it queued while itself blocked on a
        // channel receive, so the encode must join the work, not park on it.
        let (one_worker, one_worker_bytes) = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("one-worker rayon pool")
            .install(|| recovered_generation_digest_chunked(native, &identity, &|| Ok(()), 16))
            .expect("parallel digest on a one-worker pool");

        assert_eq!(one_worker, serial);
        assert_eq!(one_worker_bytes, serial_bytes);
    }

    /// The same cancelling caller on a one-worker pool must observe its own
    /// `check` and return the typed error, not wedge the sole pool worker on
    /// encode tasks queued behind a blocking channel receive.
    #[test]
    fn parallel_digest_cancels_on_a_one_worker_pool() {
        let (_owner, database, manifest) = staged_database();
        let identity = manifest.identity();
        let guard = database.read_guard().unwrap();
        let native = guard.as_ref().unwrap();

        let total_polls = AtomicUsize::new(0);
        let counting = || {
            total_polls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        };
        recovered_generation_digest_chunked(native, &identity, &counting, 16).unwrap();
        let total = total_polls.load(Ordering::Relaxed);
        let cancel_at = total / 2;

        let polls = AtomicUsize::new(0);
        let cancelled = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("one-worker rayon pool")
            .install(|| {
                recovered_generation_digest_chunked(
                    native,
                    &identity,
                    &|| {
                        if polls.fetch_add(1, Ordering::Relaxed) + 1 >= cancel_at {
                            Err(GraphDbError::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    16,
                )
            });

        assert!(
            matches!(cancelled, Err(GraphDbError::Cancelled)),
            "{cancelled:?}"
        );
        assert!(
            polls.load(Ordering::Relaxed) < total,
            "cancellation must stop the joined encode early: {} polls of {total}",
            polls.load(Ordering::Relaxed)
        );
    }

    /// A one-worker caller must run its own queued send (no deadlock). A
    /// two-worker caller whose local deque is empty while the other worker
    /// owns the send must park on the bounded receive, not spin: rayon-core
    /// 1.13.0 reports that state as `Yield::Idle`, not `None`.
    #[test]
    fn recv_while_working_joins_local_work_and_parks_when_idle() {
        let one_worker_polls = AtomicUsize::new(0);
        let one_worker = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("one-worker rayon pool")
            .install(|| {
                let (sender, receiver) = sync_channel(1);
                rayon::spawn(move || {
                    sender
                        .send(11_u32)
                        .expect("one-worker send reaches the waiter");
                });
                super::recv_while_working(&receiver, &|| {
                    one_worker_polls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                })
            })
            .expect("one-worker drain");
        assert_eq!(one_worker, 11);

        let started = Arc::new(AtomicBool::new(false));
        let drain_polls = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = sync_channel(1);
        let receiver = Arc::new(std::sync::Mutex::new(Some(receiver)));
        let stolen = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .expect("two-worker rayon pool")
            .broadcast({
                let started = Arc::clone(&started);
                let drain_polls = Arc::clone(&drain_polls);
                let receiver = Arc::clone(&receiver);
                move |ctx| {
                    if ctx.index() == 0 {
                        let receiver = receiver
                            .lock()
                            .expect("receiver lock")
                            .take()
                            .expect("single waiter");
                        while !started.load(Ordering::Acquire) {
                            std::thread::yield_now();
                        }
                        Some(
                            super::recv_while_working(&receiver, &|| {
                                drain_polls.fetch_add(1, Ordering::Relaxed);
                                Ok(())
                            })
                            .expect("stolen encode arrives"),
                        )
                    } else {
                        started.store(true, Ordering::Release);
                        std::thread::sleep(Duration::from_millis(40));
                        sender
                            .send(23_u32)
                            .expect("stolen encode send reaches the waiter");
                        None
                    }
                }
            });
        let stolen_value = stolen
            .into_iter()
            .find_map(|value| value)
            .expect("worker 0 received the stolen encode");
        assert_eq!(stolen_value, 23);
        let polls = drain_polls.load(Ordering::Relaxed);
        // A 1 ms park over ~40 ms is tens of polls. A busy Idle spin is
        // orders of magnitude more; a single blocking recv without
        // re-checking would stay at 1.
        assert!(
            (8..=200).contains(&polls),
            "stolen-encode drain must park, not spin or block: {polls} polls"
        );
    }

    /// `check` must fire while the oldest encode has not produced a result,
    /// so a spent deadline can abort without waiting for that chunk.
    #[test]
    fn recv_while_working_cancels_before_the_oldest_result_arrives() {
        let started = Arc::new(AtomicBool::new(false));
        let drain_polls = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = sync_channel(1);
        let receiver = Arc::new(std::sync::Mutex::new(Some(receiver)));
        let cancelled = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .expect("two-worker rayon pool")
            .broadcast({
                let started = Arc::clone(&started);
                let drain_polls = Arc::clone(&drain_polls);
                let receiver = Arc::clone(&receiver);
                move |ctx| {
                    if ctx.index() == 0 {
                        let receiver = receiver
                            .lock()
                            .expect("receiver lock")
                            .take()
                            .expect("single waiter");
                        while !started.load(Ordering::Acquire) {
                            std::thread::yield_now();
                        }
                        super::recv_while_working(&receiver, &|| {
                            let polls = drain_polls.fetch_add(1, Ordering::Relaxed) + 1;
                            if polls >= 8 {
                                Err(GraphDbError::Cancelled)
                            } else {
                                Ok(())
                            }
                        })
                    } else {
                        started.store(true, Ordering::Release);
                        std::thread::sleep(Duration::from_millis(80));
                        let _ = sender.send(29_u32);
                        Ok(29_u32)
                    }
                }
            });
        assert!(
            cancelled
                .iter()
                .any(|result| matches!(result, Err(GraphDbError::Cancelled))),
            "{cancelled:?}"
        );
        let polls = drain_polls.load(Ordering::Relaxed);
        assert!(
            (8..=200).contains(&polls),
            "empty-channel cancel must observe check before the send: {polls}"
        );
    }
}

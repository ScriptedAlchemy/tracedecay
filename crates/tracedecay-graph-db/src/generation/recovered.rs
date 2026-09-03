use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use grafeo_common::types::{ArcStr, NodeId};
use grafeo_core::graph::GraphStore;
use grafeo_engine::GrafeoDB;
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
    GraphGenerationRelation, GraphProjectionIdentity, frame_length_headers,
    physical_namespace_projection_map, recovered_entity_ref, write_canonical_frame,
    write_digest_bytes, write_generation_identity_frames,
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
/// count is what the verify gauge reports and what a verified-generation
/// marker records, so a later marker hit can report the same magnitude of work
/// it avoided.
///
/// A generation larger than one chunk is proven through a bounded parallel
/// pipeline: workers decode rows and canonicalize frames in sorted-chunk
/// order while the calling thread hashes completed chunks strictly in order,
/// so the digest bytes are identical to the serial stream. Cancellation is
/// polled on the calling thread at least once per row plus every hashed
/// 64 KiB, exactly as before; workers additionally abort between rows once
/// the calling thread has observed a failure. In-flight chunk buffers are
/// bounded (workers × 2), so verification memory stays a small constant over
/// the one-decoded-row posture of the serial path. Generations at or below
/// one chunk keep the strictly serial single-pass stream.
///
/// Each row costs exactly one storage load: entities decode straight from
/// their enumerated node, and relation endpoints memoize their identity refs
/// per chunk so a hub entity resolves once per chunk instead of once per
/// incident relation. The digest comparison in `verify_recovered_generation`
/// is the content authority for this proof; per-row unique-key index
/// round-trips contributed no bytes to it and are deliberately absent.
#[hotpath::measure(label = "graph_db.generation.recover.digest")]
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

/// The single-pass stream for generations at or below one chunk: one decoded
/// row resident at a time, every frame hashed as it is encoded.
#[hotpath::measure(label = "graph_db.generation.recover.digest_serial")]
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
        write_canonical_frame(
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
        write_canonical_frame(
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
/// Chunks are joined oldest-first, so frames enter the digest in exactly the
/// serial order and the first error to surface is the earliest failing chunk
/// — the same row a serial enumeration would have failed on. `check` runs
/// only on the calling thread (it is not required to be `Sync`); workers
/// poll the shared abort flag between rows and inside long row encodes, so
/// an observed failure stops in-flight encoding promptly.
#[hotpath::measure(label = "graph_db.generation.recover.digest_parallel")]
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
    // One scoped thread per worker keeps the runtime ceiling truthful while
    // the consumer hashes the oldest chunk. Each join opens one slot for the
    // next sorted chunk, so at most `PROOF_MAX_WORKERS` chunk buffers exist.
    let in_flight = workers;
    let abort = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let mut pending = VecDeque::with_capacity(in_flight);
        let mut next_chunk = 0usize;
        let result = (|| -> Result<(), GraphDbError> {
            loop {
                while pending.len() < in_flight && next_chunk < chunks.len() {
                    let chunk = chunks[next_chunk];
                    let store = Arc::clone(&store);
                    let abort = &abort;
                    let namespace_projection = &*namespace_projection;
                    pending.push_back(scope.spawn(move || {
                        encode_proof_chunk(store.as_ref(), chunk, namespace_projection, abort)
                    }));
                    next_chunk += 1;
                }
                let Some(handle) = pending.pop_front() else {
                    return Ok(());
                };
                let encoded = handle.join().map_err(|_| {
                    GraphDbError::unavailable("recovered generation verification worker panicked")
                })??;
                let mut start = 0usize;
                for &end in &encoded.frame_ends {
                    check()?;
                    write_digest_bytes(writer, &encoded.buffer[start..end])?;
                    start = end;
                }
            }
        })();
        if result.is_err() {
            abort.store(true, Ordering::Release);
        }
        // Drain outstanding workers so the scope closes without blocking on
        // encoders that no longer have a consumer.
        for handle in pending {
            let _ = handle.join();
        }
        result
    })
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
/// incident relation re-loads the full endpoint node — all properties
/// included — just to extract two identity strings. The memo stores
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

    /// The parallel pipeline must reproduce the serial stream exactly —
    /// digest and counted canonical bytes — across chunk sizes that split
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
    /// thread's own check — the closure is never shared with workers — and
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
}

//! Crash and durability contract for the registry-owned persistent graph store.
//!
//! These cases exercise the three residual failure shapes the verified
//! publication surface must survive:
//!
//! 1. A missing or foreign `.grafeo.wal` sidecar beside a checkpointed store is
//!    not durable evidence, and a torn `.grafeo` tail must fault the reopen as
//!    `Corrupt`. Neither may advance the relational verified head past the last
//!    verified generation.
//! 2. Convergence interrupted before the relational compare-and-swap must leave
//!    the prior verified snapshot serving and must replay to the identical
//!    generation once the authority recovers — never half-visible.
//! 3. A foreign, non-final store shape must be typed `ResetRequired` on open,
//!    and a fresh recreation must republish from the canonical manifest and
//!    serve the new verified head.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use tempfile::TempDir;
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::{
    GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationDependency,
    GraphGenerationId, GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
    GraphWatermark, SourceGeneration, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    GraphProjectionIdentityV1, GraphPublicationInputDigestV1, GraphPublicationKeyV1,
    GraphPublicationOperationContextV1, GraphPublicationProjectionPageRequestV1,
    GraphPublicationProjectionPageV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayPageRequestV1, GraphPublicationReplayPageV1,
    GraphPublicationReplayRecordV1, GraphPublicationReplayRetirementV1, GraphPublicationReplayV1,
    GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationRetiredCleanupPageV1,
    GraphPublicationSequenceV1, GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1,
    GraphPublicationStoreV1, GraphReplayAppendOutcomeV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1,
};

mod support;

use support::{RegisteredGraph, TestCancellation, graph_path, registration};

/// Mirrors the runtime request probe the other graph-db contract suites use;
/// the harness owns interruption state so a test can interrupt convergence.
struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Arc<AtomicU8>,
    polls: Arc<AtomicUsize>,
    commit_started: AtomicBool,
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        match self.interruption.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(RuntimeInterruptionV1::Cancelled),
            2 => Some(RuntimeInterruptionV1::DeadlineExceeded),
            _ => unreachable!("test interruption state is closed"),
        }
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

fn control_and_probe() -> (RuntimeRequestControlV1, Probe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new("durability-crash-cancellation").unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new("durability-crash-deadline").unwrap(),
    };
    (
        RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            deadline: deadline.clone(),
            cancellation: cancellation.clone(),
        },
        Probe {
            cancellation,
            deadline,
            interruption: Arc::new(AtomicU8::new(0)),
            polls: Arc::new(AtomicUsize::new(0)),
            commit_started: AtomicBool::new(false),
        },
    )
}

/// In-memory stand-in for the relational publication authority. Only the
/// surface the verified publication and recovery paths actually call is
/// implemented; the replay-garbage-collection surface is unreachable here and
/// panics loudly rather than inventing semantics.
#[derive(Default)]
struct RelationalAuthority {
    next_sequence: u64,
    records: BTreeMap<GraphPublicationKeyV1, GraphPublicationReplayRecordV1>,
    pending: BTreeMap<GraphProjectionIdentityV1, GraphPublicationReplayRecordV1>,
    heads: BTreeMap<GraphProjectionIdentityV1, GraphVerifiedHeadV1>,
    /// Simulates the process dying exactly at the relational linearization
    /// point: the native generation is applied and verified, but the head
    /// never advances.
    fail_next_cas: bool,
    cas_attempts: usize,
    cas_advances: usize,
}

impl RelationalAuthority {
    fn stage(&mut self, publication: GraphPublicationReplayV1) -> GraphPublicationReplayRecordV1 {
        self.next_sequence += 1;
        let record = GraphPublicationReplayRecordV1::new(
            GraphPublicationSequenceV1::new(self.next_sequence).unwrap(),
            publication,
        )
        .unwrap();
        self.records
            .insert(record.publication.key.clone(), record.clone());
        self.pending
            .insert(record.publication.key.projection.clone(), record.clone());
        record
    }

    fn head(&self, projection: &GraphProjectionIdentityV1) -> Option<&GraphVerifiedHeadV1> {
        self.heads.get(projection)
    }
}

impl GraphPublicationStoreV1 for RelationalAuthority {
    fn append_replay(
        &mut self,
        publication: &GraphPublicationReplayV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphReplayAppendOutcomeV1> {
        if let Some(record) = self.records.get(&publication.key) {
            return Ok(GraphReplayAppendOutcomeV1::ExactReplay(record.clone()));
        }
        Ok(GraphReplayAppendOutcomeV1::Appended(
            self.stage(publication.clone()),
        ))
    }

    fn pending_replay(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
        Ok(self.pending.get(projection).cloned())
    }

    fn replay(
        &mut self,
        key: &GraphPublicationKeyV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayLookupV1> {
        Ok(match self.records.get(key) {
            Some(record) => GraphPublicationReplayLookupV1::Active(record.clone()),
            None => GraphPublicationReplayLookupV1::Missing,
        })
    }

    fn projection_page(
        &mut self,
        _request: &GraphPublicationProjectionPageRequestV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphPublicationProjectionPageV1> {
        unreachable!("durability crash contract never enumerates projections")
    }

    fn retire_replay(
        &mut self,
        _request: &GraphPublicationReplayRetirementV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1> {
        unreachable!("durability crash contract never retires a replay")
    }

    fn replay_page(
        &mut self,
        _request: &GraphPublicationReplayPageRequestV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayPageV1> {
        unreachable!("durability crash contract never pages replays")
    }

    fn retired_cleanup_page(
        &mut self,
        _request: &GraphPublicationRetiredCleanupPageRequestV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphPublicationRetiredCleanupPageV1> {
        unreachable!("durability crash contract never pages retired cleanup")
    }

    fn finalize_retired_replay_cleanup(
        &mut self,
        _request: &GraphPublicationReplayRetirementV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphRetiredReplayCleanupFinalizeOutcomeV1> {
        unreachable!("durability crash contract never finalizes retired cleanup")
    }

    fn verified_head(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>> {
        Ok(self.heads.get(projection).cloned())
    }

    fn compare_and_swap_verified_head(
        &mut self,
        request: &GraphVerifiedHeadCompareAndSwapV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphVerifiedHeadCasOutcomeV1> {
        self.cas_attempts += 1;
        if self.fail_next_cas {
            self.fail_next_cas = false;
            return Err(GraphPublicationStoreErrorV1::Infrastructure);
        }
        let record = self
            .records
            .get(&request.publication_key)
            .cloned()
            .ok_or(GraphPublicationStoreErrorV1::Infrastructure)?;
        if self.heads.get(&request.publication_key.projection)
            != request.expected_prior_head.as_ref()
        {
            return Ok(GraphVerifiedHeadCasOutcomeV1::Conflict {
                actual: self.heads.get(&request.publication_key.projection).cloned(),
            });
        }
        let head =
            GraphVerifiedHeadV1::from_replay(&record, request.recovered_digest.clone()).unwrap();
        self.heads
            .insert(request.publication_key.projection.clone(), head.clone());
        self.pending.remove(&request.publication_key.projection);
        self.cas_advances += 1;
        Ok(GraphVerifiedHeadCasOutcomeV1::Advanced(head))
    }
}

fn digest(byte: char) -> GraphPublicationInputDigestV1 {
    GraphPublicationInputDigestV1::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn projection(namespace: &str, projection: &str) -> GraphProjectionIdentity {
    GraphProjectionIdentity::new(
        GraphNamespace::new(namespace).unwrap(),
        GraphProjectionId::new(projection).unwrap(),
    )
}

fn entity(identity: &str, marker: &str) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(identity).unwrap(),
        BTreeSet::new(),
        BTreeMap::from([(
            GraphPropertyName::new("marker").unwrap(),
            GraphProperty::String(marker.to_owned()),
        )]),
    )
    .unwrap()
}

fn manifest(
    projection: GraphProjectionIdentity,
    generation: &str,
    marker: &str,
) -> GraphGenerationManifest {
    GraphGenerationManifest::new(
        projection,
        GraphGenerationId::new(generation).unwrap(),
        SourceGeneration::new(format!("source:{generation}")).unwrap(),
        GraphWatermark::new(format!("watermark:{generation}")).unwrap(),
        Vec::<GraphGenerationDependency>::new(),
        vec![entity("entity:shared", marker)],
        Vec::<GraphGenerationRelation>::new(),
    )
    .unwrap()
}

fn stage_manifest(
    authority: &mut RelationalAuthority,
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    manifest: &GraphGenerationManifest,
    idempotency: &str,
    expected: Option<GraphVerifiedHeadV1>,
    input: char,
) -> GraphPublicationReplayRecordV1 {
    authority.stage(
        manifest
            .relational_replay(
                binding.shard_id.clone(),
                GraphIdempotencyKey::new(idempotency).unwrap(),
                digest(input),
                expected,
                &|| Ok(()),
            )
            .unwrap(),
    )
}

fn sidecar_wal_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".wal");
    std::path::PathBuf::from(sidecar)
}

fn marker_of(snapshot: &VerifiedGraphSnapshot, identity: &GraphProjectionIdentity) -> String {
    let entity = snapshot
        .entity(
            &GraphEntityRef::new(
                identity.clone(),
                GraphEntityId::new("entity:shared").unwrap(),
            ),
            Arc::new(TestCancellation),
        )
        .unwrap()
        .expect("the verified snapshot serves its own entity");
    match entity
        .properties
        .get(&GraphPropertyName::new("marker").unwrap())
    {
        Some(GraphProperty::String(value)) => value.clone(),
        other => panic!("marker property must be a string, got {other:?}"),
    }
}

/// Writes a Grafeo store carrying a TraceDecay format marker whose schema is
/// not the final native scalar schema — the foreign/non-final shape the open
/// path must reject as `ResetRequired`.
fn write_non_final_shape(path: &std::path::Path) {
    let raw = grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(path)
            .with_storage_format(grafeo_engine::config::StorageFormat::SingleFile),
    )
    .unwrap();
    let mut session = raw.session();
    session.begin_transaction().unwrap();
    session
        .create_node_with_props(
            &["__tracedecay_graph_db_format"],
            [
                ("__tracedecay_graph_db_version", 2_i64.into()),
                (
                    "__tracedecay_graph_db_schema",
                    "tracedecay-foreign-shape-v0".into(),
                ),
                ("__tracedecay_graph_db_sequence", 0_i64.into()),
            ],
        )
        .unwrap();
    session.commit().unwrap();
    raw.close().unwrap();
}

#[test]
fn torn_durable_store_faults_reopen_and_never_advances_the_verified_head() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("crash", "torn");

    let g1 = manifest(identity.clone(), "g1", "g1");
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:g1",
        None,
        'a',
    );
    let first = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    let verified_head = first.head.clone();
    drop(first);
    let projection_key = g1_record.publication.key.projection.clone();
    assert_eq!(authority.head(&projection_key), Some(&verified_head));

    // A clean close checkpoints the `.grafeo.wal` sidecar away, so after a
    // shutdown the single `.grafeo` file carries the entire durable store.
    assert!(registered.close().unwrap());
    let path = graph_path(temp.path());
    let sidecar = sidecar_wal_path(&path);
    assert!(path.is_file());
    assert!(!sidecar.exists());

    // A foreign or corrupt WAL sidecar left beside a checkpointed store is not
    // durable evidence: the reopen serves the last verified generation from the
    // single file and the relational head is untouched.
    std::fs::create_dir_all(&sidecar).unwrap();
    std::fs::write(sidecar.join("000001.wal"), vec![0xAB_u8; 4096]).unwrap();
    registered.mount().unwrap();
    let after_foreign_sidecar = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &projection_key,
        )
        .unwrap();
    assert_eq!(
        after_foreign_sidecar.generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    assert_eq!(marker_of(&after_foreign_sidecar, &identity), "g1");
    drop(after_foreign_sidecar);
    assert_eq!(authority.head(&projection_key), Some(&verified_head));
    assert!(registered.close().unwrap());

    // A torn write in the single durable file is the real crash surface.
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        bytes.len() > 64,
        "a published store must have a non-trivial durable body"
    );
    std::fs::write(&path, &bytes[..bytes.len() / 3]).unwrap();

    let cas_attempts_before = authority.cas_attempts;
    let error = registered.mount().unwrap_err();
    // `map_open_error` types malformed IO on a preexisting store as `Corrupt`
    // (crates/tracedecay-graph-db/src/recovery.rs), never as a silent reopen.
    assert!(
        matches!(error, GraphDbError::Corrupt { .. }),
        "a torn durable store must surface a typed Corrupt fault, got {error:?}"
    );

    // The fault is retained by the registry: the shard stays faulted rather
    // than silently reopening onto a truncated store.
    let repeated = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &projection_key,
        )
        .unwrap_err();
    assert_eq!(repeated, error);

    // A publication staged after the tear must not advance the relational head.
    let g2 = manifest(identity.clone(), "g2", "g2");
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:g2",
        Some(verified_head.clone()),
        'b',
    );
    let publish_error = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g2_record.publication.key,
            None,
        )
        .unwrap_err();
    assert_eq!(publish_error, error);
    assert_eq!(authority.cas_attempts, cas_attempts_before);
    assert_eq!(
        authority.head(&projection_key),
        Some(&verified_head),
        "the relational verified head must never advance past the last verified generation"
    );
}

#[test]
fn interrupted_convergence_serves_the_prior_snapshot_and_replays_identically() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("crash", "interrupted");

    let g1 = manifest(identity.clone(), "g1", "g1");
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:g1",
        None,
        'a',
    );
    let first = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    let prior_head = first.head.clone();
    drop(first);
    let projection_key = g1_record.publication.key.projection.clone();

    // Stage g2 and kill the authority exactly at the relational linearization
    // point: the native generation converges, but the head never advances.
    let g2 = manifest(identity.clone(), "g2", "g2");
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:g2",
        Some(prior_head.clone()),
        'b',
    );
    authority.fail_next_cas = true;
    let interrupted = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g2_record.publication.key,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(interrupted, GraphDbError::Unavailable { .. }),
        "a relational authority failure at CAS is an availability fault, got {interrupted:?}"
    );
    assert_eq!(authority.cas_advances, 1, "only g1 ever advanced the head");
    assert_eq!(authority.head(&projection_key), Some(&prior_head));

    // Kill the runtime with the interrupted work half-applied natively.
    assert!(registered.close().unwrap());

    // The prior verified snapshot still serves after restart; the interrupted
    // generation is not visible through the verified surface.
    registered.mount().unwrap();
    let recovered = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &projection_key,
        )
        .unwrap();
    assert_eq!(
        recovered.generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    assert_eq!(marker_of(&recovered, &identity), "g1");
    assert_eq!(
        registered
            .registry
            .verified_snapshot(
                registration(registered.binding.clone(), temp.path()),
                &identity,
            )
            .unwrap()
            .generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    drop(recovered);

    // The durable replay record still describes exactly one outcome, so the
    // interrupted work replays to the identical generation once the authority
    // is available again.
    let replayed = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g2_record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(
        replayed.snapshot.generation(),
        &GraphGenerationId::new("g2").unwrap()
    );
    let replayed_head = replayed.head.clone();
    assert_eq!(marker_of(&replayed.snapshot, &identity), "g2");
    drop(replayed);
    assert_eq!(authority.cas_advances, 2);
    assert_eq!(authority.head(&projection_key), Some(&replayed_head));

    // Re-driving the same key is an exact replay: identical head, no new CAS.
    let attempts_after_replay = authority.cas_attempts;
    let exact = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g2_record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(exact.head, replayed_head);
    assert_eq!(
        exact.snapshot.generation(),
        &GraphGenerationId::new("g2").unwrap()
    );
    assert_eq!(authority.cas_attempts, attempts_after_replay);
    assert_eq!(authority.cas_advances, 2);
}

#[test]
fn reset_required_shape_is_recreated_fresh_and_republished_from_the_manifest() {
    let temp = TempDir::new().unwrap();
    let path = graph_path(temp.path());
    write_non_final_shape(&path);

    let stale = RegisteredGraph::new(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("crash", "reset");
    let g1 = manifest(identity.clone(), "g1", "g1");
    let record = stage_manifest(&mut authority, &stale.binding, &g1, "publish:g1", None, 'a');
    let projection_key = record.publication.key.projection.clone();

    let error = stale.mount().unwrap_err();
    assert!(
        matches!(error, GraphDbError::ResetRequired { .. }),
        "a foreign non-final store shape must be typed ResetRequired, got {error:?}"
    );
    assert_eq!(authority.cas_attempts, 0);
    assert_eq!(authority.head(&projection_key), None);

    // Fresh recreation: the reset-required store is discarded and a new
    // profile-local runtime opens onto the same canonical path.
    std::fs::remove_file(&path).unwrap();
    let _ = std::fs::remove_dir_all(sidecar_wal_path(&path));
    let fresh = RegisteredGraph::new_mounted(temp.path()).unwrap();

    let commit = fresh
        .registry
        .publish_verified(
            registration(fresh.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            None,
        )
        .unwrap();
    let head = commit.head.clone();
    assert_eq!(
        commit.snapshot.generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    drop(commit);
    assert_eq!(authority.cas_advances, 1);
    assert_eq!(authority.head(&projection_key), Some(&head));

    let snapshot = fresh
        .registry
        .verified_snapshot(registration(fresh.binding.clone(), temp.path()), &identity)
        .unwrap();
    assert_eq!(
        snapshot.generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    assert_eq!(marker_of(&snapshot, &identity), "g1");
}

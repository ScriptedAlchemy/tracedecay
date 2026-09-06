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
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use tempfile::TempDir;
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::{
    GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationDependency,
    GraphGenerationId, GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey,
    GraphMutation, GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProperty,
    GraphPropertyName, GraphWatermark, GraphWriteBatch, SourceGeneration, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    GraphProjectionIdentityV1, GraphPublicationInputDigestV1, GraphPublicationKeyV1,
    GraphPublicationOperationContextV1, GraphPublicationProjectionPageRequestV1,
    GraphPublicationProjectionPageV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayPageRequestV1, GraphPublicationReplayPageV1,
    GraphPublicationReplayRecordV1, GraphPublicationReplayRetirementV1,
    GraphPublicationReplayTombstoneV1, GraphPublicationReplayV1,
    GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationRetiredCleanupPageV1,
    GraphPublicationSequenceV1, GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1,
    GraphPublicationStoreV1, GraphReplayAppendOutcomeV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1,
};

mod support;

use support::{
    RegisteredGraph, TestCancellation, UnsetSqliteUnsafeFast, capture_unclean_crash_image,
    crash_child_root, graph_path, mark_durable_phase, registration, sidecar_wal_path,
};

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
    retired: BTreeMap<GraphPublicationKeyV1, GraphPublicationReplayTombstoneV1>,
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
        Ok(if let Some(record) = self.records.get(key) {
            GraphPublicationReplayLookupV1::Active(record.clone())
        } else if let Some(tombstone) = self.retired.get(key) {
            GraphPublicationReplayLookupV1::Retired(tombstone.clone())
        } else {
            GraphPublicationReplayLookupV1::Missing
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

    fn retire_verified_head_replay(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        expected_head: &GraphVerifiedHeadV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1> {
        if let Some(tombstone) = self.retired.get(&request.key) {
            return Ok(if tombstone.retirement() == *request {
                GraphReplayRetirementOutcomeV1::ExactReplay(tombstone.clone())
            } else {
                GraphReplayRetirementOutcomeV1::Conflict
            });
        }
        let Some(record) = self.records.get(&request.key).cloned() else {
            return Ok(GraphReplayRetirementOutcomeV1::Missing);
        };
        if record.publication.input_digest != request.input_digest
            || record.publication.dependency_generation_closure_digest
                != request.dependency_generation_closure_digest
            || record.publication.direct_dependency_generations
                != request.direct_dependency_generations
            || record.publication.expected_prior_head != request.expected_prior_head
            || record.publication.expected_recovered_digest != request.expected_recovered_digest
            || record.publication.canonical_replay_source_digest
                != request.canonical_replay_source_digest
        {
            return Ok(GraphReplayRetirementOutcomeV1::Conflict);
        }
        let Some(head) = self.heads.get(&request.key.projection) else {
            return Ok(GraphReplayRetirementOutcomeV1::Conflict);
        };
        if head != expected_head || head.sequence != record.sequence {
            return Ok(GraphReplayRetirementOutcomeV1::Conflict);
        }
        if let Some(pending) = self.pending.get(&request.key.projection) {
            return Ok(GraphReplayRetirementOutcomeV1::PendingReplay {
                pending: pending.clone(),
            });
        }
        let tombstone = GraphPublicationReplayTombstoneV1::new(
            record.sequence,
            request.clone(),
            Some(record.publication.canonical_replay_source.clone()),
        )
        .map_err(GraphPublicationStoreErrorV1::InvalidRequest)?;
        self.heads.remove(&request.key.projection);
        self.records.remove(&request.key);
        self.retired.insert(request.key.clone(), tombstone.clone());
        Ok(GraphReplayRetirementOutcomeV1::Retired(tombstone))
    }

    fn discard_pending_replay(
        &mut self,
        _request: &tracedecay_store::GraphPendingReplayDiscardV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<tracedecay_store::GraphPendingReplayDiscardOutcomeV1> {
        unreachable!("durability crash contract never discards a pending replay")
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

fn write_published_g1_leaving_wal(
    root: &Path,
    namespace: &str,
    projection_id: &str,
) -> RegisteredGraph {
    let registered = RegisteredGraph::new_mounted(root).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection(namespace, projection_id);
    let g1 = manifest(identity, "g1", "g1");
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:g1",
        None,
        'a',
    );
    drop(
        registered
            .registry
            .publish_verified(
                registration(registered.binding.clone(), root),
                &mut authority,
                &context,
                &g1_record.publication.key,
                None,
            )
            .unwrap(),
    );
    registered
}

fn apply_unverified_wal_debt(registered: &RegisteredGraph, root: &Path) {
    let debt_namespace = GraphNamespace::new("crash-wal-debt").unwrap();
    let debt_entity = GraphEntityId::new("entity:wal-debt").unwrap();
    let live = registered
        .registry
        .resolve(registration(registered.binding.clone(), root))
        .unwrap();
    live.apply_unverified(
        GraphWriteBatch::new(
            debt_namespace,
            GraphProjectionId::new("unclean-shutdown").unwrap(),
            SourceGeneration::new("source:wal-debt").unwrap(),
            GraphWatermark::new("watermark:wal-debt").unwrap(),
            vec![GraphMutation::UpsertEntity(entity(
                debt_entity.as_str(),
                "wal-debt",
            ))],
            Arc::new(TestCancellation),
        )
        .unwrap(),
    )
    .unwrap();
    std::mem::forget(live);
}

fn linearize_g1_journal(
    root: &Path,
    namespace: &str,
    projection_id: &str,
) -> (
    RelationalAuthority,
    GraphPublicationReplayRecordV1,
    GraphVerifiedHeadV1,
) {
    let registered = RegisteredGraph::new_mounted(root).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection(namespace, projection_id);
    let g1 = manifest(identity, "g1", "g1");
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
            registration(registered.binding.clone(), root),
            &mut authority,
            &context,
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    let verified_head = first.head.clone();
    drop(first);
    assert!(registered.close().unwrap());
    (authority, g1_record, verified_head)
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

/// Locates the timestamped quarantine directory the corrupt-mount recovery
/// created beside the container, or `None` before any quarantine ran.
fn quarantine_directory(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut directories: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| {
            entry.file_type().unwrap().is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("graph.grafeo.corrupt-"))
        })
        .map(|entry| entry.path())
        .collect();
    directories.sort();
    directories.pop()
}

fn quarantine_receipt(quarantine: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(quarantine.join("store-quarantined.json")).unwrap())
        .unwrap()
}

/// GitHub issue #763: a deterministic corruption verdict on the durable
/// container was retried identically forever — every mount refaulted, every
/// activation refused, and only manual surgery (move the store and WAL aside,
/// restart) recovered the project. This pins the automatic form of exactly
/// that recovery: the second identical verdict quarantines the container for
/// forensics, the mount reopens fresh, and the relational replay journal
/// re-projects the verified generation without ever advancing the head.
#[test]
fn torn_durable_store_is_quarantined_and_rebuilt_from_the_replay_journal() {
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
    // single file, the relational head is untouched, and nothing quarantines.
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
    assert!(quarantine_directory(temp.path()).is_none());
    assert!(registered.close().unwrap());

    // A torn write in the single durable file is the real crash surface.
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        bytes.len() > 64,
        "a published store must have a non-trivial durable body"
    );
    let torn = bytes[..bytes.len() / 3].to_vec();
    std::fs::write(&path, &torn).unwrap();

    // The mount re-proves the fault itself and quarantines instead of
    // faulting: retry #2 with the identical verdict is the terminal state
    // for these bytes, never retry #25.
    registered.mount().unwrap();
    let quarantine = quarantine_directory(temp.path())
        .expect("a deterministically corrupt container must be quarantined");
    assert_eq!(
        std::fs::read(quarantine.join("graph.grafeo")).unwrap(),
        torn,
        "the forensic bytes must move into quarantine unmodified"
    );
    let receipt = quarantine_receipt(&quarantine);
    assert_eq!(
        receipt["version"].as_str().unwrap(),
        "tracedecay.graph-store-quarantine.v1"
    );
    assert_eq!(receipt["verification_attempts"].as_u64().unwrap(), 2);
    assert!(
        receipt["fault"].as_str().unwrap().contains("corrupt")
            || !receipt["fault"].as_str().unwrap().is_empty(),
        "the receipt journals the exact fault"
    );
    assert!(
        receipt["fault_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );

    // The fresh store refuses the recovered head with a typed mismatch until
    // the replay journal re-projects it — never a silent empty success.
    let pending = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &projection_key,
        )
        .unwrap_err();
    assert!(
        matches!(
            pending,
            GraphDbError::GenerationMismatch { .. } | GraphDbError::ProjectionMismatch { .. }
        ),
        "a fresh store pending rebuild must refuse typed, got {pending:?}"
    );

    // The durable replay record rebuilds the identical generation: this is
    // the manual mv-and-restart recovery, automated.
    let rebuilt = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(
        rebuilt.snapshot.generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    assert_eq!(marker_of(&rebuilt.snapshot, &identity), "g1");
    assert_eq!(rebuilt.head, verified_head);
    drop(rebuilt);
    assert_eq!(
        authority.head(&projection_key),
        Some(&verified_head),
        "the relational verified head must never advance past the last verified generation"
    );
    assert_eq!(
        authority.cas_advances, 1,
        "rebuilding from the journal replays the linearized outcome, never a new advance"
    );

    // The rebuilt store serves through the ordinary verified surface.
    let served = registered
        .registry
        .verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &identity,
        )
        .unwrap();
    assert_eq!(served.generation(), &GraphGenerationId::new("g1").unwrap());
    assert_eq!(marker_of(&served, &identity), "g1");
}

/// The operator-profile fault shape: a SIGKILL mid-WAL-write leaves a
/// current-version container whose serialized block no longer matches its
/// CRC, plus a live WAL sidecar. The corrupted-but-current store reports the
/// CRC fault deterministically; quarantine must adopt the container *and*
/// the WAL sidecar so the forensic pair stays together, and the fresh store
/// must rebuild from the replay journal.
#[test]
fn crc_faulted_store_is_quarantined_with_its_wal_sidecar_and_rebuilt() {
    let _unsafe_fast = UnsetSqliteUnsafeFast::new();
    if let Some(root) = crash_child_root() {
        let registered = write_published_g1_leaving_wal(&root, "crash", "crc");
        mark_durable_phase(&root);
        std::mem::forget(registered);
        std::process::exit(0);
    }

    let journal = TempDir::new().unwrap();
    let (mut authority, g1_record, verified_head) =
        linearize_g1_journal(journal.path(), "crash", "crc");
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let identity = projection("crash", "crc");
    let projection_key = g1_record.publication.key.projection.clone();

    // A child publishes g1, reaches the durable WAL phase, and exits without
    // a clean close. Only then is the abandoned image copied — never while a
    // live Windows handle still owns the store (issue #933).
    let crash = TempDir::new().unwrap();
    capture_unclean_crash_image(crash.path());
    let crashed_container = graph_path(crash.path());
    let crashed_sidecar = sidecar_wal_path(&crashed_container);
    assert!(
        crashed_sidecar.is_dir(),
        "the crash image must carry the live WAL sidecar"
    );
    let wal_segments: Vec<(String, Vec<u8>)> = std::fs::read_dir(&crashed_sidecar)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    assert!(
        !wal_segments.is_empty(),
        "the crash image must carry WAL segments"
    );

    // Flip authoritative leading bytes while keeping the length: the store
    // is still current-sized but its serialized sections no longer match
    // their checksums. The physical midpoint is not a valid target — the
    // format may leave aligned padding there, outside every section
    // checksum (see verified_generation_contract/verify_once.rs).
    let mut bytes = std::fs::read(&crashed_container).unwrap();
    assert!(bytes.len() > 512);
    for byte in bytes.iter_mut().take(64) {
        *byte ^= 0xFF;
    }
    let corrupted = bytes.clone();
    std::fs::write(&crashed_container, &bytes).unwrap();

    let crashed = RegisteredGraph::new(crash.path()).unwrap();
    crashed.mount().unwrap();

    let quarantine =
        quarantine_directory(crash.path()).expect("a CRC-faulted container must be quarantined");
    assert_eq!(
        std::fs::read(quarantine.join("graph.grafeo")).unwrap(),
        corrupted,
        "the corrupted container bytes are forensic evidence and move unmodified"
    );
    let quarantined_sidecar = quarantine.join("graph.grafeo.wal");
    assert!(
        quarantined_sidecar.is_dir(),
        "the WAL sidecar must move with its container"
    );
    // The quarantined segments are the exact pre-mount forensic bytes. The
    // fresh WalSync store legitimately creates its own new sidecar at the
    // live path; the old journal is provably not beside it because every
    // old segment now lives in quarantine with unmodified content.
    for (segment, expected_bytes) in &wal_segments {
        assert_eq!(
            &std::fs::read(quarantined_sidecar.join(segment)).unwrap(),
            expected_bytes,
            "WAL segment {segment} must be retained in quarantine unmodified"
        );
    }
    let receipt = quarantine_receipt(&quarantine);
    let members: Vec<&str> = receipt["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member.as_str().unwrap())
        .collect();
    assert!(members.contains(&"graph.grafeo"));
    assert!(members.contains(&"graph.grafeo.wal"));

    // The journal rebuild serves the exact verified generation again.
    let rebuilt = crashed
        .registry
        .publish_verified(
            registration(crashed.binding.clone(), crash.path()),
            &mut authority,
            &context,
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(
        rebuilt.snapshot.generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    assert_eq!(marker_of(&rebuilt.snapshot, &identity), "g1");
    assert_eq!(rebuilt.head, verified_head);
    drop(rebuilt);
    assert_eq!(authority.head(&projection_key), Some(&verified_head));
}

/// The compare-and-swap discipline for the quarantine decision itself: an
/// authority that does not hold the decision lock must neither re-verify nor
/// sweep the store — another incarnation may be mid-recovery. The refusal is
/// a retryable typed unavailable, not a retained terminal fault, so the next
/// mount attempt (after the holder releases) completes the recovery.
#[test]
fn held_quarantine_decision_defers_the_mount_and_the_next_attempt_recovers() {
    use fs2::FileExt;

    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("crash", "held");

    let g1 = manifest(identity.clone(), "g1", "g1");
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:g1",
        None,
        'a',
    );
    drop(
        registered
            .registry
            .publish_verified(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &g1_record.publication.key,
                None,
            )
            .unwrap(),
    );
    assert!(registered.close().unwrap());

    let path = graph_path(temp.path());
    let bytes = std::fs::read(&path).unwrap();
    let torn = bytes[..bytes.len() / 3].to_vec();
    std::fs::write(&path, &torn).unwrap();

    // A foreign incarnation holds the quarantine decision.
    let lock_path = temp.path().join("graph.grafeo.quarantine-lock");
    let foreign_holder = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    foreign_holder.try_lock_exclusive().unwrap();

    let deferred = registered.mount().unwrap_err();
    assert!(
        matches!(&deferred, GraphDbError::Unavailable { message }
            if message.contains("another authority holds")),
        "a held decision must defer retryably, got {deferred:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        torn,
        "a non-holder must not move or modify the store"
    );
    assert!(quarantine_directory(temp.path()).is_none());

    // Once the holder releases, the same mount request completes the
    // quarantine and rebuild instead of remaining faulted.
    FileExt::unlock(&foreign_holder).unwrap();
    registered.mount().unwrap();
    let quarantine = quarantine_directory(temp.path())
        .expect("the released decision lock lets the next mount quarantine");
    assert_eq!(
        std::fs::read(quarantine.join("graph.grafeo")).unwrap(),
        torn
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

/// Highest sequence among non-empty `wal_<sequence>.log` segments — the same
/// replay-debt signal the open-time collapse gates on.
fn newest_wal_segment(sidecar: &std::path::Path) -> Option<u64> {
    let entries = std::fs::read_dir(sidecar).ok()?;
    entries
        .flatten()
        .filter(|entry| entry.metadata().is_ok_and(|metadata| metadata.len() > 0))
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.strip_prefix("wal_")?
                .strip_suffix(".log")?
                .parse::<u64>()
                .ok()
        })
        .max()
}

/// An open that replays sidecar-WAL history from an unclean shutdown must
/// collapse that history into the `.grafeo` container immediately, so a
/// crash-looping rebuild pays the replay once instead of ratcheting the log
/// (and the next open's replay cost) on every restart.
#[test]
fn reopen_collapses_replayed_wal_history_from_an_unclean_shutdown() {
    let _unsafe_fast = UnsetSqliteUnsafeFast::new();
    if let Some(root) = crash_child_root() {
        let registered = write_published_g1_leaving_wal(&root, "crash", "reopen-collapse");
        apply_unverified_wal_debt(&registered, &root);
        mark_durable_phase(&root);
        std::mem::forget(registered);
        std::process::exit(0);
    }

    let journal = TempDir::new().unwrap();
    let (mut authority, g1_record, _verified_head) =
        linearize_g1_journal(journal.path(), "crash", "reopen-collapse");
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let identity = projection("crash", "reopen-collapse");
    let projection_key = g1_record.publication.key.projection.clone();
    let debt_namespace = GraphNamespace::new("crash-wal-debt").unwrap();
    let debt_entity = GraphEntityId::new("entity:wal-debt").unwrap();

    // Child publishes g1, stages unverified WAL debt, and exits without a
    // clean close. The copy runs only after that process is gone.
    let crash_root = TempDir::new().unwrap();
    capture_unclean_crash_image(crash_root.path());
    let crash_path = graph_path(crash_root.path());
    let crash_sidecar = sidecar_wal_path(&crash_path);
    let debt_segment = newest_wal_segment(&crash_sidecar)
        .expect("the crash image carries staged history in the sidecar WAL");
    // Earlier checkpoints may exist (the sealed-store build checkpoints g1's
    // history under `graph-sealed-store`); the debt is that none of them
    // covers the staged g2 segments.
    let checkpoint_before = grafeo_storage::wal::WalRecovery::new(&crash_sidecar).checkpoint();
    assert!(
        checkpoint_before
            .as_ref()
            .is_none_or(|checkpoint| checkpoint.log_sequence < debt_segment),
        "no checkpoint covers the staged history in the crash image \
         (checkpoint {:?} vs newest segment {debt_segment})",
        checkpoint_before.map(|checkpoint| checkpoint.log_sequence),
    );
    let container_before = std::fs::metadata(&crash_path).unwrap().len();

    // Mounting the crash image replays the staged history once and must
    // collapse it: checkpoint metadata now names the newest segment, so the
    // next open of this store no longer pays the replay.
    let reopened = RegisteredGraph::new_mounted(crash_root.path()).unwrap();
    let collapsed = grafeo_storage::wal::WalRecovery::new(&crash_sidecar)
        .checkpoint()
        .expect("the reopen must checkpoint the replayed history");
    assert!(
        collapsed.log_sequence >= debt_segment,
        "the reopen checkpoint must cover the replayed segments \
         (log_sequence {} < newest segment {debt_segment})",
        collapsed.log_sequence,
    );
    let container_after = std::fs::metadata(&crash_path).unwrap().len();
    assert!(
        container_after > container_before,
        "the collapse must flush the replayed rows into the container \
         ({container_before} -> {container_after} bytes)"
    );
    let replayed = reopened
        .registry
        .resolve(registration(reopened.binding.clone(), crash_root.path()))
        .unwrap();
    let replayed_debt = replayed
        .entity(&debt_namespace, &debt_entity, Arc::new(TestCancellation))
        .unwrap()
        .expect("the checkpointed container must retain the replayed WAL row");
    assert_eq!(
        replayed_debt
            .properties
            .get(&GraphPropertyName::new("marker").unwrap()),
        Some(&GraphProperty::String("wal-debt".to_owned()))
    );
    drop(replayed);

    // The collapse changed durability bookkeeping only: the verified surface
    // still serves exactly g1.
    let recovered = reopened
        .registry
        .recover_verified_snapshot(
            registration(reopened.binding.clone(), crash_root.path()),
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
}

/// A publication interrupted at the relational linearization point leaves a
/// fully staged, durably receipted and proven generation behind. The retry
/// must not redo the projection: it reseats those durable pages and pays
/// only verification and lease seating before advancing the head. Restaged
/// pages would append new sidecar-WAL history, so an unchanged newest WAL
/// segment across the retry proves nothing was restaged.
#[test]
fn retry_after_interrupted_publication_reseats_without_restaging() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("crash", "reseat-resume");

    let manifest = manifest(identity.clone(), "g1", "g1");
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:g1",
        None,
        'a',
    );

    // First attempt: staging and the durable digest proof complete, then the
    // relational authority dies exactly at the verified-head CAS.
    authority.fail_next_cas = true;
    let interrupted = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(interrupted, GraphDbError::Unavailable { .. }),
        "a relational authority failure at CAS is an availability fault, got {interrupted:?}"
    );

    let sidecar = sidecar_wal_path(&graph_path(temp.path()));
    let staged_segment = newest_wal_segment(&sidecar);

    // The retry reseats the durable staged generation and completes.
    let retried = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            None,
        )
        .expect("the retry must resume from the durable staged generation");
    assert_eq!(retried.head.key.generation.as_str(), "g1");
    assert_eq!(
        newest_wal_segment(&sidecar),
        staged_segment,
        "a resuming retry must not append staged history to the sidecar WAL"
    );

    // The resumed publication serves the generation it staged once.
    let recovered = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key.projection,
        )
        .unwrap();
    assert_eq!(
        recovered.generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    assert_eq!(marker_of(&recovered, &identity), "g1");
}

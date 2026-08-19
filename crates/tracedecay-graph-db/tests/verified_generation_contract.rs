use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use tempfile::TempDir;
use tracedecay_domain::{CodeGenerationId, RepositoryId, UtcMicros};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef,
    GraphGenerationDependency, GraphGenerationId, GraphGenerationManifest, GraphGenerationRelation,
    GraphIdempotencyKey, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
    GraphProjectorRevision, GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind,
    GraphRelationRef, GraphReplayCollectionOutcome, GraphWatermark, SealedCodeGenerationReplay,
    SealedGraphStateDigest, SourceGeneration,
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
    RuntimeRequestControlV1, RuntimeRequestProbeV1, StoreShardIdV1,
};

#[path = "verified_generation_contract/metadata_replay.rs"]
mod metadata_replay;
#[path = "verified_generation_contract/replay_decode.rs"]
mod replay_decode;
mod support;

use support::{RegisteredGraph, TestCancellation, registration};

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

struct AtomicTestCancellation(Arc<AtomicU8>);

impl GraphCancellation for AtomicTestCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst) != 0
    }
}

fn control_and_probe() -> (RuntimeRequestControlV1, Probe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new("generation-test-cancellation").unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new("generation-test-deadline").unwrap(),
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

#[derive(Default)]
struct RelationalAuthority {
    next_sequence: u64,
    records: BTreeMap<GraphPublicationKeyV1, GraphPublicationReplayRecordV1>,
    retired: BTreeMap<GraphPublicationKeyV1, GraphPublicationReplayTombstoneV1>,
    pending: BTreeMap<GraphProjectionIdentityV1, GraphPublicationReplayRecordV1>,
    heads: BTreeMap<GraphProjectionIdentityV1, GraphVerifiedHeadV1>,
    cancel_after_cas: Option<Arc<AtomicU8>>,
    cancel_after_retire: Option<Arc<AtomicU8>>,
    cas_calls: usize,
    read_calls: usize,
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
        self.read_calls += 1;
        Ok(self.pending.get(projection).cloned())
    }

    fn replay(
        &mut self,
        key: &GraphPublicationKeyV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayLookupV1> {
        self.read_calls += 1;
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
        request: &GraphPublicationProjectionPageRequestV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphPublicationProjectionPageV1> {
        let mut projections = self
            .records
            .keys()
            .map(|key| key.projection.clone())
            .filter(|projection| {
                projection.shard_id == request.shard_id
                    && request
                        .after
                        .as_ref()
                        .is_none_or(|after| projection > after)
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let has_more = projections.len() > usize::from(request.max_records);
        projections.truncate(usize::from(request.max_records));
        let continuation = has_more.then(|| projections.last().unwrap().clone());
        GraphPublicationProjectionPageV1::new(projections, continuation)
            .map_err(GraphPublicationStoreErrorV1::InvalidRequest)
    }

    fn retire_replay(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1> {
        if let Some(tombstone) = self.retired.get(&request.key) {
            return Ok(if tombstone.retirement() == *request {
                GraphReplayRetirementOutcomeV1::ExactReplay(tombstone.clone())
            } else {
                GraphReplayRetirementOutcomeV1::Conflict
            });
        }
        if let Some(head) = self.heads.get(&request.key.projection)
            && head.key == request.key
        {
            return Ok(GraphReplayRetirementOutcomeV1::CurrentVerifiedHead { head: head.clone() });
        }
        if let Some(pending) = self.pending.get(&request.key.projection)
            && pending.publication.key == request.key
        {
            return Ok(GraphReplayRetirementOutcomeV1::PendingReplay {
                pending: pending.clone(),
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
        let tombstone = GraphPublicationReplayTombstoneV1::new(
            record.sequence,
            request.clone(),
            Some(record.publication.canonical_replay_source.clone()),
        )
        .map_err(GraphPublicationStoreErrorV1::InvalidRequest)?;
        self.records.remove(&request.key);
        self.retired.insert(request.key.clone(), tombstone.clone());
        if let Some(interruption) = &self.cancel_after_retire {
            interruption.store(1, Ordering::SeqCst);
        }
        Ok(GraphReplayRetirementOutcomeV1::Retired(tombstone))
    }

    fn replay_page(
        &mut self,
        request: &GraphPublicationReplayPageRequestV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayPageV1> {
        self.read_calls += 1;
        let mut records = self
            .records
            .values()
            .filter(|record| {
                record.publication.key.projection == request.projection
                    && request
                        .after
                        .as_ref()
                        .is_none_or(|after| record.sequence > after.sequence)
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.sequence);
        // Deliberately force one-record pages so every multi-generation GC
        // exercises bounded continuation rather than an accidental full scan.
        let page_size = usize::from(request.max_records).min(1);
        let has_more = records.len() > page_size;
        records.truncate(page_size);
        let continuation = has_more.then(|| {
            tracedecay_store::GraphPublicationReplayCursorV1::new(
                request.projection.clone(),
                records
                    .last()
                    .expect("a non-empty bounded page has a cursor")
                    .sequence,
            )
            .unwrap()
        });
        GraphPublicationReplayPageV1::new(records, continuation)
            .map_err(GraphPublicationStoreErrorV1::InvalidRequest)
    }

    fn retired_cleanup_page(
        &mut self,
        request: &GraphPublicationRetiredCleanupPageRequestV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphPublicationRetiredCleanupPageV1> {
        let mut records = self
            .retired
            .values()
            .filter(|record| {
                record.key.projection == request.projection
                    && record.canonical_replay_source.is_some()
                    && request
                        .after
                        .as_ref()
                        .is_none_or(|after| record.sequence > after.sequence)
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.sequence);
        let has_more = records.len() > usize::from(request.max_records);
        records.truncate(usize::from(request.max_records));
        let continuation = has_more.then(|| {
            tracedecay_store::GraphPublicationReplayCursorV1::new(
                request.projection.clone(),
                records.last().expect("bounded page has a record").sequence,
            )
            .unwrap()
        });
        GraphPublicationRetiredCleanupPageV1::new(records, continuation)
            .map_err(GraphPublicationStoreErrorV1::InvalidRequest)
    }

    fn finalize_retired_replay_cleanup(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphRetiredReplayCleanupFinalizeOutcomeV1> {
        let Some(tombstone) = self.retired.get_mut(&request.key) else {
            return Ok(GraphRetiredReplayCleanupFinalizeOutcomeV1::Missing);
        };
        if tombstone.retirement() != *request {
            return Ok(GraphRetiredReplayCleanupFinalizeOutcomeV1::Conflict);
        }
        if tombstone.canonical_replay_source.is_none() {
            return Ok(GraphRetiredReplayCleanupFinalizeOutcomeV1::ExactReplay(
                tombstone.clone(),
            ));
        }
        tombstone.canonical_replay_source = None;
        Ok(GraphRetiredReplayCleanupFinalizeOutcomeV1::Finalized(
            tombstone.clone(),
        ))
    }

    fn verified_head(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>> {
        self.read_calls += 1;
        Ok(self.heads.get(projection).cloned())
    }

    fn compare_and_swap_verified_head(
        &mut self,
        request: &GraphVerifiedHeadCompareAndSwapV1,
        _context: &GraphPublicationOperationContextV1,
    ) -> GraphPublicationStoreResultV1<GraphVerifiedHeadCasOutcomeV1> {
        self.cas_calls += 1;
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
        if let Some(interruption) = &self.cancel_after_cas {
            interruption.store(1, Ordering::SeqCst);
        }
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
    dependencies: Vec<GraphGenerationDependency>,
    relations: Vec<GraphGenerationRelation>,
) -> GraphGenerationManifest {
    GraphGenerationManifest::new(
        projection,
        GraphGenerationId::new(generation).unwrap(),
        SourceGeneration::new(format!("source:{generation}")).unwrap(),
        GraphWatermark::new(format!("watermark:{generation}")).unwrap(),
        dependencies,
        vec![entity("entity:shared", marker)],
        relations,
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

/// Sealed code generations journal only their replay source; the projection
/// manifest is rebuilt from the sealed artifact and supplied by the
/// publication owner. The supplied manifest must bind through the journaled
/// identity digests and publish. Refusing every sealed supplied manifest as
/// `Conflict` failed each code-graph publication immediately after its own
/// journal append, so no sealed generation could ever become the verified
/// head and code-index activation looped on `code graph database conflict`.
#[test]
fn sealed_code_generation_publishes_with_its_supplied_manifest() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("code-scope:sealed-supplied", "code-generation");
    let sealed_manifest = manifest(
        identity.clone(),
        "code-graph:sealed-supplied",
        "sealed",
        vec![],
        vec![],
    );
    let sealed_source = SealedCodeGenerationReplay {
        repository: RepositoryId::new("repository.sealed-supplied").unwrap(),
        generation: CodeGenerationId::new("code-generation.sealed-supplied").unwrap(),
        sealed_state_digest: SealedGraphStateDigest::try_from(format!("sha256:{}", "7".repeat(64)))
            .unwrap(),
        projector_revision: GraphProjectorRevision::try_from(
            "projector.sealed-supplied".to_owned(),
        )
        .unwrap(),
    };
    // The journal row is exactly what the code-index publisher appends: the
    // sealed replay source rides in the payload while the identity digests
    // pin the projection manifest built from the sealed artifact.
    let record = authority.stage(
        sealed_manifest
            .relational_sealed_replay(
                registered.binding.shard_id.clone(),
                GraphIdempotencyKey::new("publish:code-graph:sealed-supplied").unwrap(),
                digest('1'),
                None,
                sealed_source,
                &|| Ok(()),
            )
            .unwrap(),
    );

    // A foreign manifest for the same journaled sealed replay stays refused:
    // different projected content cannot pass the journaled recovered digest.
    let foreign = manifest(
        identity.clone(),
        "code-graph:sealed-supplied",
        "foreign-content",
        vec![],
        vec![],
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .publish_verified(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &record.publication.key,
                Some(foreign),
            )
            .unwrap_err(),
        GraphDbError::Conflict
    );

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            Some(sealed_manifest.clone()),
        )
        .expect("the exact supplied sealed projection manifest must publish");
    assert_eq!(commit.head.key, record.publication.key);
    let snapshot = registered
        .registry
        .verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &identity,
        )
        .unwrap();
    assert_eq!(snapshot.generation(), &sealed_manifest.generation);
}

#[test]
fn retired_replay_survives_native_delete_failure_until_restart_cleanup_finalizes() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("retirement", "restart");
    let sealed_generation = CodeGenerationId::new("code-generation.retired").unwrap();
    let sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "9".repeat(64))).unwrap();
    let wrong_sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "8".repeat(64))).unwrap();

    let g1 = manifest(identity.clone(), "retired-g1", "g1", vec![], vec![]);
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:retired-g1",
        None,
        '1',
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let g1_commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    let g1_head = g1_commit.head.clone();
    drop(g1_commit);

    let sealed_publication = g1
        .relational_sealed_replay(
            registered.binding.shard_id.clone(),
            GraphIdempotencyKey::new("publish:retired-g1").unwrap(),
            digest('1'),
            None,
            SealedCodeGenerationReplay {
                repository: RepositoryId::new("repository.retirement").unwrap(),
                generation: sealed_generation.clone(),
                sealed_state_digest: sealed_digest.clone(),
                projector_revision: GraphProjectorRevision::try_from(
                    "projector.retirement".to_owned(),
                )
                .unwrap(),
            },
            &|| Ok(()),
        )
        .unwrap();
    authority.records.insert(
        g1_record.publication.key.clone(),
        GraphPublicationReplayRecordV1::new(g1_record.sequence, sealed_publication).unwrap(),
    );

    let g2 = manifest(identity, "retained-g2", "g2", vec![], vec![]);
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:retained-g2",
        Some(g1_head),
        '2',
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let g2_commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g2_record.publication.key,
            None,
        )
        .unwrap();
    drop(g2_commit);

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered.registry.retire_one_code_generation_replay(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &sealed_generation,
            &wrong_sealed_digest,
        ),
        Err(GraphDbError::Conflict)
    );

    let cancellation = Arc::new(AtomicU8::new(0));
    authority.cancel_after_retire = Some(Arc::clone(&cancellation));
    let mut cancelled_registration = registration(registered.binding.clone(), temp.path());
    cancelled_registration.cancellation =
        Arc::new(AtomicTestCancellation(Arc::clone(&cancellation)));
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered.registry.retire_one_code_generation_replay(
            cancelled_registration,
            &mut authority,
            &context,
            &sealed_generation,
            &sealed_digest,
        ),
        Err(GraphDbError::Cancelled)
    );
    let tombstone = authority
        .retired
        .get(&g1_record.publication.key)
        .expect("retirement committed before native deletion failed");
    assert!(tombstone.canonical_replay_source.is_some());

    cancellation.store(0, Ordering::SeqCst);
    authority.cancel_after_retire = None;
    assert!(registered.close().unwrap());
    drop(registered);
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered.registry.retire_one_code_generation_replay(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &sealed_generation,
            &wrong_sealed_digest,
        ),
        Err(GraphDbError::Conflict)
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        registered.registry.verified_generation_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g1_record.publication.key,
        ),
        Err(GraphDbError::Conflict)
    ));

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered.registry.retire_one_code_generation_replay(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &sealed_generation,
            &sealed_digest,
        ),
        Ok(GraphReplayCollectionOutcome::Absent)
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(
        registered
            .registry
            .finalize_one_code_generation_replay_cleanup(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &sealed_generation,
                &sealed_digest,
            )
            .unwrap()
    );
    assert!(
        authority
            .retired
            .get(&g1_record.publication.key)
            .expect("retired identity remains durable")
            .canonical_replay_source
            .is_none()
    );
}

#[test]
fn verified_generations_keep_old_reads_dependencies_and_leases_stable() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let shared_name_a = projection("namespace:a", "same-name");
    let shared_name_b = projection("namespace:b", "same-name");

    let b_relation = GraphGenerationRelation::new(
        GraphRelationId::new("relation:cross").unwrap(),
        GraphEntityRef::new(
            shared_name_b.clone(),
            GraphEntityId::new("entity:shared").unwrap(),
        ),
        GraphEntityRef::new(
            shared_name_b.clone(),
            GraphEntityId::new("entity:shared").unwrap(),
        ),
        GraphRelationKind::new("self").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    let b1 = manifest(shared_name_b.clone(), "b1", "b1", vec![], vec![b_relation]);
    let b1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &b1,
        "publish:b1",
        None,
        'a',
    );
    let b1_commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &b1_record.publication.key,
            None,
        )
        .unwrap();

    let dependency = GraphGenerationDependency::new(
        shared_name_b.clone(),
        GraphGenerationId::new("b1").unwrap(),
        GraphIdempotencyKey::new("publish:b1").unwrap(),
    );
    let relation = GraphGenerationRelation::new(
        GraphRelationId::new("relation:cross").unwrap(),
        GraphEntityRef::new(
            shared_name_a.clone(),
            GraphEntityId::new("entity:shared").unwrap(),
        ),
        GraphEntityRef::new(
            shared_name_b.clone(),
            GraphEntityId::new("entity:shared").unwrap(),
        ),
        GraphRelationKind::new("depends").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    let a1 = manifest(
        shared_name_a.clone(),
        "a1",
        "a1",
        vec![dependency],
        vec![relation],
    );
    let a1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &a1,
        "publish:a1",
        None,
        'b',
    );
    let a1_commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &a1_record.publication.key,
            None,
        )
        .unwrap();
    let a1_snapshot = registered
        .registry
        .verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &shared_name_a,
        )
        .unwrap();
    let cross = a1_snapshot
        .relation(
            &GraphRelationRef::new(
                shared_name_a.clone(),
                GraphRelationId::new("relation:cross").unwrap(),
            ),
            Arc::new(TestCancellation),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cross.from.projection, shared_name_a);
    assert_eq!(cross.to.projection, shared_name_b);
    let dependency_relation = a1_snapshot
        .relation(
            &GraphRelationRef::new(
                shared_name_b.clone(),
                GraphRelationId::new("relation:cross").unwrap(),
            ),
            Arc::new(TestCancellation),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        dependency_relation.kind,
        GraphRelationKind::new("self").unwrap()
    );

    let b2 = manifest(shared_name_b.clone(), "b2", "b2", vec![], vec![]);
    let b2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &b2,
        "publish:b2",
        Some(b1_commit.head.clone()),
        'c',
    );
    authority.cancel_after_cas = Some(Arc::clone(&probe.interruption));
    registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &b2_record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(authority.cas_calls, 3);
    assert_eq!(
        a1_snapshot
            .entity(
                &GraphEntityRef::new(
                    shared_name_b.clone(),
                    GraphEntityId::new("entity:shared").unwrap(),
                ),
                Arc::new(TestCancellation),
            )
            .unwrap()
            .unwrap()
            .properties
            .get(&GraphPropertyName::new("marker").unwrap()),
        Some(&GraphProperty::String("b1".to_owned()))
    );
    probe.interruption.store(0, Ordering::SeqCst);
    authority.cancel_after_cas = None;

    let a2 = manifest(shared_name_a.clone(), "a2", "a2", vec![], vec![]);
    let a2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &a2,
        "publish:a2",
        Some(a1_commit.head),
        'f',
    );
    registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &a2_record.publication.key,
            None,
        )
        .unwrap();
    drop(a1_snapshot);
}

#[test]
fn restart_reverification_installs_once_and_steady_reads_need_no_authority() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("restart", "work");
    let g1 = manifest(identity.clone(), "g1", "g1", vec![], vec![]);
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:g1",
        None,
        'd',
    );
    let first = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            None,
        )
        .unwrap();
    let first_head = first.head.clone();
    drop(first);
    let g2 = manifest(identity.clone(), "g2", "g2", vec![], vec![]);
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:g2",
        Some(first_head),
        'e',
    );
    registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g2_record.publication.key,
            None,
        )
        .unwrap();
    assert!(registered.close().unwrap());
    registered.mount().unwrap();
    registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g2_record.publication.key.projection,
        )
        .unwrap();
    let reads_after_recovery = authority.read_calls;
    let snapshot = registered
        .registry
        .verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &identity,
        )
        .unwrap();
    assert_eq!(
        snapshot.generation(),
        &GraphGenerationId::new("g2").unwrap()
    );
    assert_eq!(authority.read_calls, reads_after_recovery);
    assert!(probe.polls.load(Ordering::SeqCst) > 2);
    let historical = registered
        .registry
        .verified_generation_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
        )
        .unwrap();
    assert_eq!(
        historical.generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
    assert_eq!(
        historical
            .entity(
                &GraphEntityRef::new(
                    identity.clone(),
                    GraphEntityId::new("entity:shared").unwrap(),
                ),
                Arc::new(TestCancellation),
            )
            .unwrap()
            .unwrap()
            .properties
            .get(&GraphPropertyName::new("marker").unwrap()),
        Some(&GraphProperty::String("g1".to_owned()))
    );
    assert_eq!(
        registered
            .registry
            .verified_snapshot(
                registration(registered.binding.clone(), temp.path()),
                &identity,
            )
            .unwrap()
            .generation(),
        &GraphGenerationId::new("g2").unwrap()
    );
    drop(historical);
}

#[test]
fn cancellation_before_relational_cas_keeps_the_prior_head_current() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("cancel", "work");
    let g1 = manifest(identity.clone(), "g1", "g1", vec![], vec![]);
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:g1",
        None,
        'e',
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
    let calls_after_g1 = authority.cas_calls;
    let exact = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(exact.head, first.head);
    assert_eq!(authority.cas_calls, calls_after_g1);

    let g2 = manifest(identity.clone(), "g2", "g2", vec![], vec![]);
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:g2",
        Some(first.head),
        'f',
    );
    probe.interruption.store(1, Ordering::SeqCst);
    assert_eq!(
        registered
            .registry
            .publish_verified(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &g2_record.publication.key,
                None,
            )
            .unwrap_err(),
        tracedecay_graph_db::GraphDbError::Cancelled
    );
    assert_eq!(authority.cas_calls, calls_after_g1);
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
}

/// Work-topology publications carry labeled entities with byte-record
/// properties; the verified head must advance for that manifest shape exactly
/// as it does for the plain string-property manifests above.
#[test]
fn labeled_byte_record_entities_reach_a_verified_head() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection(
        &format!("work-topology:sha256:{}", "2b".repeat(32)),
        "work-topology",
    );
    let task_record = serde_json::json!({
        "task_id": "task.repro",
        "title": "repro",
        "dependencies": [],
    });
    let task = GraphEntity::new(
        GraphEntityId::new(format!("task:{}", "03".repeat(32))).unwrap(),
        BTreeSet::from([tracedecay_graph_db::GraphLabel::new("WorkTask").unwrap()]),
        BTreeMap::from([
            (
                GraphPropertyName::new("task-id").unwrap(),
                GraphProperty::String("task.repro".to_owned()),
            ),
            (
                GraphPropertyName::new("task-record").unwrap(),
                GraphProperty::Bytes(serde_json::to_vec(&task_record).unwrap()),
            ),
        ]),
    )
    .unwrap();
    let generation = format!("work-topology:sha256:{}", "c4".repeat(32));
    let manifest = GraphGenerationManifest::new(
        identity.clone(),
        GraphGenerationId::new(&generation).unwrap(),
        SourceGeneration::new(format!("sha256:{}", "f6".repeat(32))).unwrap(),
        GraphWatermark::new(format!("sha256:{}", "f6".repeat(32))).unwrap(),
        vec![],
        vec![task],
        vec![],
    )
    .unwrap();
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        &format!("publish:{generation}"),
        None,
        'a',
    );
    let commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(commit.head.key, record.publication.key);
    let snapshot = registered
        .registry
        .verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &identity,
        )
        .unwrap();
    assert_eq!(
        snapshot.generation(),
        &GraphGenerationId::new(&generation).unwrap()
    );
}

/// A registration may spell the database file through a symlinked ancestor
/// (macOS reaches `/var` and `/tmp` through `/private/...`). The registry
/// collapses every spelling to the file's one canonical name — while still
/// refusing a store directory that is itself a symlink — so an operation
/// whose lease carries the aliased spelling must publish rather than be
/// refused as a foreign database.
#[cfg(unix)]
#[test]
fn registration_spelled_through_symlinked_ancestor_publishes() {
    let temp = TempDir::new().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir_all(real.join("store")).unwrap();
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let aliased_store = alias.join("store");
    let registered = RegisteredGraph::new_mounted(&aliased_store).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("alias", "work");
    let g1 = manifest(identity.clone(), "g1", "g1", vec![], vec![]);
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:g1",
        None,
        'e',
    );
    let commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), &aliased_store),
            &mut authority,
            &context,
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    assert_eq!(commit.head.key, g1_record.publication.key);
    assert_eq!(
        registered
            .registry
            .verified_snapshot(
                registration(registered.binding.clone(), &aliased_store),
                &identity,
            )
            .unwrap()
            .generation(),
        &GraphGenerationId::new("g1").unwrap()
    );
}

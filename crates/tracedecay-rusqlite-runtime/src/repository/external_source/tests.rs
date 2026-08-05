use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    AccessPolicyDigest, CapabilityId, ComponentVersion, LocatorDigest, ManifestDigest,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProviderId,
    ResolutionAuthorizationV1, RetrievalAnchorId, SanitizationReceiptId, SanitizationReceiptRefV1,
    ScopeResolutionId, SourceAcquisitionCapabilitiesV1, SourceAcquisitionContractV1,
    SourceAggregateFrontierV1, SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1,
    SourceContentStateV1, SourceCoverageV1, SourceCursorV1, SourceDefinitionV1,
    SourceDeletionSemanticsV1, SourceEventAdmissionDispositionV1, SourceEventAdmissionReceiptV1,
    SourceEventV1, SourceInstanceId, SourceNativeObjectIdV1, SourceObjectObservationV1,
    SourceObjectRevisionV1, SourcePartitionFrontierV1, SourcePartitionIdV1,
    SourceRefetchStrategyV1, SourceRefreshCauseV1, SourceRefreshReceiptV1,
    SourceSnapshotCompletionV1, SourceSnapshotIdV1, UtcMicros, canonical_sha256,
};
use tracedecay_store::{
    SourceAcquisitionQueueCasV1, SourceAcquisitionQueueStateV1, SourceAuthorityPublicationV1,
    SourceObjectMutationV1, SourceObjectTransitionV1, SourceObservationEvidenceV1,
    SourceScheduledRefetchV1, build_source_projection,
};

use super::*;

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
}

fn fixture() -> (SourceCommitV1, SourceBindingIdentityV1) {
    let definition = SourceDefinitionV1::new(
        SourceInstanceId::new("source.runtime-fixture").unwrap(),
        1,
        SourceAcquisitionContractV1::new(
            ProviderId::new("github").unwrap(),
            SourceAcquisitionCapabilitiesV1::new(
                BTreeSet::from([SourceCaptureModeV1::Poll]),
                BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
                BTreeSet::from([SourceDeletionSemanticsV1::CompleteSnapshotAbsence]),
            )
            .unwrap(),
        )
        .unwrap(),
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
        1,
    )
    .unwrap();
    let binding = SourceBindingV1::new(
        &definition,
        SourceBindingOwnerV1::Project(ProjectId::new("project.runtime-fixture").unwrap()),
        PrivacyDomainId::new("privacy.runtime-fixture").unwrap(),
        LocatorDigest::new(digest('a').as_str()).unwrap(),
        1,
    )
    .unwrap();
    let identity = binding.immutable_identity().unwrap();
    let partition = SourcePartitionIdV1::new(digest('b'));
    let snapshot = SourceSnapshotIdV1::new(digest('c'));
    let observation = SourceObjectObservationV1::new(
        SourceNativeObjectIdV1::new(digest('d')),
        SourceObjectRevisionV1::new(digest('e')),
        digest('f'),
        SourceContentStateV1::Live,
    )
    .unwrap();
    let frontier = SourcePartitionFrontierV1::new(
        identity.clone(),
        partition.clone(),
        None,
        Some(snapshot.clone()),
        None,
        SourceCoverageV1::Complete,
        1,
        None,
        digest('1'),
    )
    .unwrap();
    let aggregate =
        SourceAggregateFrontierV1::with_updated_partition(identity.clone(), None, frontier)
            .unwrap();
    let completion = SourceSnapshotCompletionV1::new(
        partition.clone(),
        snapshot,
        BTreeSet::from([observation.native_object().clone()]),
    )
    .unwrap();
    let evidence = SourceObservationEvidenceV1::new(
        identity.clone(),
        partition.clone(),
        &observation,
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.external-source.runtime-fixture").unwrap(),
            ComponentVersion::new("sanitizer.external-source.v1").unwrap(),
        )
        .unwrap(),
        RetrievalAnchorId::new("retrieval.external-source.runtime-fixture").unwrap(),
        ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.external-source.runtime-fixture")
                .unwrap(),
            privacy_domain_id: identity.privacy_domain.clone(),
            access_policy_digest: AccessPolicyDigest::new(digest('4').as_str()).unwrap(),
            capability_id: CapabilityId::new("capability.external-source.runtime-fixture").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(digest('5').as_str())
                .unwrap(),
        },
        digest('6'),
    )
    .unwrap();
    let mutation = SourceObjectMutationV1::new(
        observation,
        None,
        SourceObjectTransitionV1::Initial,
        evidence,
    )
    .unwrap();
    let commit = SourceCommitV1::new(
        definition,
        binding,
        partition,
        digest('2'),
        digest('3'),
        None,
        aggregate,
        vec![mutation],
        Some(completion),
    )
    .unwrap();
    (commit, identity)
}

fn acquisition_state() -> (SourceAcquisitionQueueStateV1, SourceBindingIdentityV1) {
    let definition = SourceDefinitionV1::new(
        SourceInstanceId::new("source.runtime-acquisition-fixture").unwrap(),
        1,
        SourceAcquisitionContractV1::new(
            ProviderId::new("github").unwrap(),
            SourceAcquisitionCapabilitiesV1::new(
                BTreeSet::from([SourceCaptureModeV1::Event]),
                BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
                BTreeSet::from([SourceDeletionSemanticsV1::ExplicitOnly]),
            )
            .unwrap(),
        )
        .unwrap(),
        SourceCaptureModeV1::Event,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::ExplicitOnly,
        1,
    )
    .unwrap();
    let binding = SourceBindingV1::new(
        &definition,
        SourceBindingOwnerV1::Project(ProjectId::new("project.runtime-fixture").unwrap()),
        PrivacyDomainId::new("privacy.runtime-fixture").unwrap(),
        LocatorDigest::new(digest('9').as_str()).unwrap(),
        1,
    )
    .unwrap();
    let identity = binding.immutable_identity().unwrap();
    let event = SourceEventV1::new(identity.clone(), digest('a')).unwrap();
    let refresh = SourceRefreshReceiptV1::new(
        identity.clone(),
        definition.provider.clone(),
        digest('b'),
        SourceRefreshCauseV1::Event,
        SourceCaptureModeV1::Event,
        SourceRefetchStrategyV1::WholeRoot,
    )
    .unwrap();
    let receipt = SourceEventAdmissionReceiptV1::new(
        &event,
        event.event_key().clone(),
        refresh,
        SourceEventAdmissionDispositionV1::Enqueued,
    )
    .unwrap();
    let scheduled = SourceScheduledRefetchV1::new(
        definition.clone(),
        binding.clone(),
        receipt.clone(),
        None,
        0,
        UtcMicros(10),
    )
    .unwrap();
    let state = SourceAcquisitionQueueStateV1::new(
        definition,
        binding,
        Some(scheduled),
        None,
        BTreeMap::from([(receipt.event_key().clone(), receipt)]),
    )
    .unwrap();
    (state, identity)
}

#[test]
fn acquisition_queue_cas_survives_restart_and_rejects_stale_writers() {
    let temporary = tempfile::tempdir().unwrap();
    let database_path = temporary.path().join("external-source-acquisition.sqlite");
    let (state, binding) = acquisition_state();
    {
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_acquisition_state_cas(
                &savepoint,
                &SourceAcquisitionQueueCasV1::new(binding.clone(), None, state.clone()).unwrap(),
            )
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let mut connection = rusqlite::Connection::open(&database_path).unwrap();
    let transaction = connection.transaction().unwrap();
    assert_eq!(
        ExternalSourceExecutor
            .execute_read(
                &transaction,
                &ExternalSourceReadOperationV1::AcquisitionState {
                    binding: binding.clone(),
                },
            )
            .unwrap(),
        ExternalSourceReadResultV1::AcquisitionState(Some(Box::new(state.clone())))
    );
    assert_eq!(
        ExternalSourceExecutor
            .execute_read(
                &transaction,
                &ExternalSourceReadOperationV1::NextReadyAcquisition { now: UtcMicros(10) },
            )
            .unwrap(),
        ExternalSourceReadResultV1::AcquisitionState(Some(Box::new(state.clone())))
    );
    drop(transaction);

    let mut transaction = connection.transaction().unwrap();
    let savepoint = transaction.savepoint().unwrap();
    assert!(
        ExternalSourceExecutor
            .execute_acquisition_state_cas(
                &savepoint,
                &SourceAcquisitionQueueCasV1::new(binding, None, state).unwrap(),
            )
            .is_err(),
        "a restarted stale writer must not replace the durable queue state"
    );
}

fn empty_successor(prior: &SourceStoreStateV1, sequence: u64, seed: char) -> SourceCommitV1 {
    let partition = prior.receipt().partition().clone();
    let next_partition = SourcePartitionFrontierV1::new(
        prior.binding().immutable_identity().unwrap(),
        partition.clone(),
        None,
        None,
        Some(SourceCursorV1::new(digest(seed))),
        SourceCoverageV1::Partial,
        sequence,
        prior
            .source_frontier()
            .partition(&partition)
            .and_then(SourcePartitionFrontierV1::last_complete_snapshot),
        digest(seed),
    )
    .unwrap();
    let next_frontier = SourceAggregateFrontierV1::with_updated_partition(
        prior.binding().immutable_identity().unwrap(),
        Some(prior.source_frontier()),
        next_partition,
    )
    .unwrap();
    SourceCommitV1::new(
        prior.definition().clone(),
        prior.binding().clone(),
        partition,
        digest(seed),
        digest('a'),
        Some(prior.source_frontier().clone()),
        next_frontier,
        Vec::new(),
        None,
    )
    .unwrap()
}

fn numbered_empty_successor(prior: &SourceStoreStateV1, sequence: u64) -> SourceCommitV1 {
    let partition = prior.receipt().partition().clone();
    let digest_for = |purpose: &str| {
        canonical_sha256(&(
            "tracedecay.external-source.history-cost-fixture.v1",
            purpose,
            sequence,
        ))
        .unwrap()
    };
    let next_partition = SourcePartitionFrontierV1::new(
        prior.binding().immutable_identity().unwrap(),
        partition.clone(),
        None,
        None,
        Some(SourceCursorV1::new(digest_for("cursor"))),
        SourceCoverageV1::Partial,
        sequence,
        prior
            .source_frontier()
            .partition(&partition)
            .and_then(SourcePartitionFrontierV1::last_complete_snapshot),
        digest_for("envelope"),
    )
    .unwrap();
    let next_frontier = SourceAggregateFrontierV1::with_updated_partition(
        prior.binding().immutable_identity().unwrap(),
        Some(prior.source_frontier()),
        next_partition,
    )
    .unwrap();
    SourceCommitV1::new(
        prior.definition().clone(),
        prior.binding().clone(),
        partition,
        digest_for("idempotency"),
        digest_for("request"),
        Some(prior.source_frontier().clone()),
        next_frontier,
        Vec::new(),
        None,
    )
    .unwrap()
}

fn empty_successor_with_coverage(
    prior: &SourceStoreStateV1,
    sequence: u64,
    coverage: SourceCoverageV1,
    present: Option<BTreeSet<SourceNativeObjectIdV1>>,
) -> SourceCommitV1 {
    let partition = prior.receipt().partition().clone();
    let digest_for = |purpose: &str| {
        canonical_sha256(&(
            "tracedecay.external-source.coverage-fixture.v1",
            purpose,
            sequence,
        ))
        .unwrap()
    };
    let snapshot = (coverage == SourceCoverageV1::Complete)
        .then(|| SourceSnapshotIdV1::new(digest_for("snapshot")));
    let continuation =
        (coverage == SourceCoverageV1::Partial).then(|| SourceCursorV1::new(digest_for("cursor")));
    let next_partition = SourcePartitionFrontierV1::new(
        prior.binding().immutable_identity().unwrap(),
        partition.clone(),
        None,
        snapshot.clone(),
        continuation,
        coverage,
        sequence,
        prior
            .source_frontier()
            .partition(&partition)
            .and_then(SourcePartitionFrontierV1::last_complete_snapshot),
        digest_for("envelope"),
    )
    .unwrap();
    let next_frontier = SourceAggregateFrontierV1::with_updated_partition(
        prior.binding().immutable_identity().unwrap(),
        Some(prior.source_frontier()),
        next_partition,
    )
    .unwrap();
    let completion = snapshot.map(|snapshot| {
        SourceSnapshotCompletionV1::new(
            partition.clone(),
            snapshot,
            present.expect("complete test snapshot declares its exact object set"),
        )
        .unwrap()
    });
    SourceCommitV1::new(
        prior.definition().clone(),
        prior.binding().clone(),
        partition,
        digest_for("idempotency"),
        digest_for("request"),
        Some(prior.source_frontier().clone()),
        next_frontier,
        Vec::new(),
        completion,
    )
    .unwrap()
}

#[test]
fn source_commits_enqueue_and_restart_drain_exact_predecessor_chain() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("external-source-backlog.sqlite");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
    let (first, binding) = fixture();
    for (sequence, commit) in [(1, first)] {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
        assert_eq!(sequence, 1);
    }
    for (sequence, seed) in [(2, '7'), (3, '9')] {
        let prior = load_state(&connection, &binding).unwrap().unwrap();
        let commit = empty_successor(&prior, sequence, seed);
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }

    let rows = connection
        .prepare(
            "SELECT predecessor_frontier_digest, successor_frontier_digest,
                        source_receipt_digest
                 FROM external_source_pending_projections_v1
                 WHERE binding_id = ?1
                 ORDER BY successor_sequence",
        )
        .unwrap()
        .query_map([binding.binding_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].0, rows[0].1);
    assert_eq!(rows[2].0, rows[1].1);
    drop(connection);

    let mut connection = rusqlite::Connection::open(&path).unwrap();
    let state = load_state(&connection, &binding).unwrap().unwrap();
    let first_pending = load_next_pending_projection(&connection, &state)
        .unwrap()
        .unwrap();
    assert_eq!(
        load_next_pending_projection_any(&connection)
            .unwrap()
            .unwrap(),
        first_pending,
        "restart replay must discover pending work without an in-memory binding registry"
    );
    let projector = ComponentVersion::new("external-source-projector-v1").unwrap();
    let first_projection = build_source_projection(&first_pending, projector.clone()).unwrap();
    let after_first =
        match apply_source_projection(&state, &first_pending, first_projection.clone()).unwrap() {
            SourceProjectionApplyOutcomeV1::Projected(state) => state,
            other => panic!("expected first projection, got {other:?}"),
        };
    let second_receipt = load_commit_receipt_by_digest(&connection, &binding, &rows[1].2)
        .unwrap()
        .unwrap();
    let second_pending = SourcePendingProjectionV1::from_state(
        after_first.as_ref(),
        state.definition().clone(),
        state.binding().clone(),
        second_receipt,
    )
    .unwrap();
    let second_projection = build_source_projection(&second_pending, projector.clone()).unwrap();
    {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        assert!(
            ExternalSourceExecutor
                .execute_projection_write(&savepoint, &second_projection)
                .is_err(),
            "a reordered successor must not skip the oldest pending receipt"
        );
    }

    for expected_sequence in 1..=3 {
        let state = load_state(&connection, &binding).unwrap().unwrap();
        let pending = load_next_pending_projection(&connection, &state)
            .unwrap()
            .unwrap();
        let projection = build_source_projection(&pending, projector.clone()).unwrap();
        assert_eq!(
            projection
                .source_frontier()
                .partition(
                    projection
                        .mutations()
                        .first()
                        .map_or(pending.receipt().partition(), |mutation| mutation
                            .evidence()
                            .partition(),)
                )
                .unwrap()
                .sequence(),
            expected_sequence
        );
        for _ in 0..2 {
            let mut transaction = connection.transaction().unwrap();
            let savepoint = transaction.savepoint().unwrap();
            ExternalSourceExecutor
                .execute_projection_write(&savepoint, &projection)
                .unwrap();
            savepoint.commit().unwrap();
            transaction.commit().unwrap();
        }
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_pending_projections_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn ten_thousand_receipts_do_not_make_current_read_or_write_scan_history() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
    let (first, binding) = fixture();
    let first_key = first.idempotency_key().clone();
    let mut transaction = connection.transaction().unwrap();
    {
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &first)
            .unwrap();
        savepoint.commit().unwrap();
    }
    for sequence in 2..=10_000 {
        let state = load_state(&transaction, &binding).unwrap().unwrap();
        let commit = numbered_empty_successor(&state, sequence);
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
    }
    transaction.commit().unwrap();

    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_commit_receipts_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        10_000
    );
    let current = load_state(&connection, &binding).unwrap().unwrap();
    assert!(
        serde_json::to_vec(&current).unwrap().len() < 64 * 1024,
        "current-state bytes must not include receipt history"
    );
    let mut lookup = connection
        .prepare(
            "SELECT receipt_json FROM external_source_commit_receipts_v1
                 WHERE binding_id = ?1 AND idempotency_key = ?2",
        )
        .unwrap();
    let _: String = lookup
        .query_row(
            params![binding.binding_id.as_str(), first_key.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        lookup.get_status(rusqlite::StatementStatus::FullscanStep),
        0,
        "exact receipt replay must use its primary-key index"
    );
    drop(lookup);

    let pages_before: i64 = connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    let commit = numbered_empty_successor(&current, 10_001);
    let mut transaction = connection.transaction().unwrap();
    let savepoint = transaction.savepoint().unwrap();
    ExternalSourceExecutor
        .execute_write(&savepoint, &commit)
        .unwrap();
    savepoint.commit().unwrap();
    transaction.commit().unwrap();
    let pages_after: i64 = connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    assert!(
        pages_after - pages_before <= 32,
        "one ordinary commit must append bounded bytes independent of history"
    );
}

#[test]
fn empty_complete_is_noop_and_partial_never_derives_absence() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
    let (first, binding) = fixture();
    let object = first.mutations()[0].observation().native_object().clone();
    let projector = ComponentVersion::new("external-source-projector-v1").unwrap();
    {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &first)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    for (sequence, coverage, present, expected_effects) in [
        (2, SourceCoverageV1::Partial, None, 0),
        (
            3,
            SourceCoverageV1::Complete,
            Some(BTreeSet::from([object.clone()])),
            0,
        ),
        (4, SourceCoverageV1::Complete, Some(BTreeSet::new()), 1),
    ] {
        let state = load_state(&connection, &binding).unwrap().unwrap();
        let pending = load_next_pending_projection(&connection, &state)
            .unwrap()
            .unwrap();
        let projection = build_source_projection(&pending, projector.clone()).unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_projection_write(&savepoint, &projection)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();

        let state = load_state(&connection, &binding).unwrap().unwrap();
        let commit = empty_successor_with_coverage(&state, sequence, coverage, present);
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
        let state = load_state(&connection, &binding).unwrap().unwrap();
        let pending = load_next_pending_projection(&connection, &state)
            .unwrap()
            .unwrap();
        let projection = build_source_projection(&pending, projector.clone()).unwrap();
        assert_eq!(projection.effects().len(), expected_effects);
    }
}

#[test]
fn stale_source_fork_rejection_preserves_the_committed_pending_chain() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
    let (first, binding) = fixture();
    {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &first)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let predecessor = load_state(&connection, &binding).unwrap().unwrap();
    let accepted = empty_successor(&predecessor, 2, '7');
    let fork = empty_successor(&predecessor, 2, '9');
    {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &accepted)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        assert!(
            ExternalSourceExecutor
                .execute_write(&savepoint, &fork)
                .is_err()
        );
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_pending_projections_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_commit_receipts_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
}

#[test]
fn separate_projection_write_rolls_back_effect_and_checkpoint_together() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
    let (commit, binding) = fixture();
    {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let source_state = load_state(&connection, &binding).unwrap().unwrap();
    assert!(source_state.projection().is_none());
    let pending = load_next_pending_projection(&connection, &source_state)
        .unwrap()
        .unwrap();
    let projection = build_source_projection(
        &pending,
        ComponentVersion::new("external-source-projector-v1").unwrap(),
    )
    .unwrap();

    connection
        .execute_batch(
            "CREATE TRIGGER fail_external_source_projection
                 BEFORE UPDATE ON external_source_states_v1
                 BEGIN
                   SELECT RAISE(ABORT, 'injected projection publication failure');
                 END;",
        )
        .unwrap();
    {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        assert!(
            ExternalSourceExecutor
                .execute_projection_write(&savepoint, &projection)
                .is_err()
        );
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_projection_publications_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert!(
        load_state(&connection, &binding)
            .unwrap()
            .unwrap()
            .projection()
            .is_none()
    );

    connection
        .execute("DROP TRIGGER fail_external_source_projection", [])
        .unwrap();
    for _ in 0..2 {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_projection_write(&savepoint, &projection)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let projected = load_state(&connection, &binding).unwrap().unwrap();
    assert_eq!(projected.projected_objects().len(), 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_projection_publications_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn commit_replay_and_restart_read_share_one_durable_state() {
    let temporary = tempfile::tempdir().unwrap();
    let database_path = temporary.path().join("external-source.sqlite");
    let (commit, binding) = fixture();
    {
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
        let mut interrupted = connection.transaction().unwrap();
        let savepoint = interrupted.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
        drop(interrupted);
    }
    {
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        let transaction = connection.transaction().unwrap();
        assert!(matches!(
            ExternalSourceExecutor
                .execute_read(
                    &transaction,
                    &ExternalSourceReadOperationV1::State {
                        binding: binding.clone(),
                    },
                )
                .unwrap(),
            ExternalSourceReadResultV1::State(None)
        ));
    }
    {
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let mut connection = rusqlite::Connection::open(&database_path).unwrap();
    let mut replay = connection.transaction().unwrap();
    let savepoint = replay.savepoint().unwrap();
    ExternalSourceExecutor
        .execute_write(&savepoint, &commit)
        .unwrap();
    savepoint.commit().unwrap();
    replay.commit().unwrap();
    let transaction = connection.transaction().unwrap();
    let state = match ExternalSourceExecutor
        .execute_read(
            &transaction,
            &ExternalSourceReadOperationV1::State {
                binding: binding.clone(),
            },
        )
        .unwrap()
    {
        ExternalSourceReadResultV1::State(Some(state)) => state,
        other => panic!("expected durable external source state, got {other:?}"),
    };
    assert_eq!(state.receipt().idempotency_key(), commit.idempotency_key());
    assert_eq!(state.observed_objects().len(), 1);
    assert!(state.projected_objects().is_empty());
    assert!(state.projection().is_none());
    let state_json_columns: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('external_source_states_v1')
                 WHERE name = 'state_json'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state_json_columns, 0);
    let durable_json: String = transaction
        .query_row(
            "SELECT source_frontier_json || mutation_json
                 FROM external_source_states_v1
                 JOIN external_source_objects_v1 USING (binding_id)
                 WHERE binding_id = ?1",
            [binding.binding_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!durable_json.contains("secret"));
    assert!(!durable_json.contains("https://"));
}

#[test]
fn authority_and_source_receipt_histories_survive_restart_and_rollback() {
    let temporary = tempfile::tempdir().unwrap();
    let database_path = temporary.path().join("external-source-history.sqlite");
    let (commit, _) = fixture();
    let definition_v1 = commit.definition().clone();
    let binding_v1 = commit.binding().clone();
    {
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let definition_v2 = SourceDefinitionV1::new(
        definition_v1.source_id.clone(),
        2,
        SourceAcquisitionContractV1::new(
            definition_v1.provider.clone(),
            definition_v1.acquisition_capabilities.clone(),
        )
        .unwrap(),
        definition_v1.capture_mode,
        definition_v1.refetch_strategy,
        definition_v1.deletion_semantics,
        definition_v1.max_partitions,
    )
    .unwrap();
    let binding_v2 = SourceBindingV1::new(
        &definition_v2,
        binding_v1.owner.clone(),
        binding_v1.privacy_domain.clone(),
        binding_v1.native_root.clone(),
        2,
    )
    .unwrap();
    let publication = SourceAuthorityPublicationV1::new(
        &definition_v2,
        &binding_v2,
        definition_v1.definition_digest.clone(),
        binding_v1.binding_digest.clone(),
        digest('7'),
        digest('8'),
    )
    .unwrap();
    {
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        let mut interrupted = connection.transaction().unwrap();
        let savepoint = interrupted.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_authority_publication(&savepoint, &publication)
            .unwrap();
        savepoint.commit().unwrap();
        drop(interrupted);
    }
    {
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_source_definition_revisions_v1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }
    {
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_authority_publication(&savepoint, &publication)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    {
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_authority_publication(&savepoint, &publication)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_definition_revisions_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_binding_revisions_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_authority_receipts_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_commit_receipts_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM external_source_projection_publications_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

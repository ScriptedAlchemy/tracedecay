use super::*;
use tracedecay_store::GraphPublicationReplayCursorV1;

#[test]
fn oversized_sequences_are_rejected_by_both_page_request_paths() {
    let projection = serde_json::to_value(projection("code")).unwrap();
    let oversized = i64::MAX.unsigned_abs() + 1;
    let cursor = serde_json::json!({
        "projection": projection.clone(),
        "sequence": oversized,
    });

    assert!(
        serde_json::from_value::<GraphPublicationReplayPageRequestV1>(serde_json::json!({
            "projection": projection.clone(),
            "after": cursor.clone(),
            "max_records": 1,
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<GraphPublicationRetiredCleanupPageRequestV1>(serde_json::json!({
            "projection": projection,
            "after": cursor,
            "max_records": 1,
        }),)
        .is_err()
    );
}

#[test]
fn replay_and_cleanup_cursors_reject_a_foreign_projection() {
    let foreign_cursor = GraphPublicationReplayCursorV1::new(
        projection("foreign"),
        tracedecay_store::GraphPublicationSequenceV1::new(1).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        GraphPublicationReplayPageRequestV1::new(
            projection("code"),
            Some(foreign_cursor.clone()),
            1,
        ),
        Err(
            tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay page cursor projection"
            }
        )
    ));
    assert!(matches!(
        GraphPublicationRetiredCleanupPageRequestV1::new(
            projection("code"),
            Some(foreign_cursor),
            1,
        ),
        Err(
            tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph retired cleanup cursor projection"
            }
        )
    ));
}

fn context(suffix: &str) -> (RuntimeRequestControlV1, Probe) {
    control_and_probe(suffix, None)
}

#[test]
fn dependency_generations_require_an_active_verified_replay_and_round_trip() {
    let fixture = Fixture::new();
    let mut storage = fixture.storage();
    let (control, probe) = context("dependency-append");
    let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();

    let missing_owner = replay_with_dependencies(
        projection("missing-owner"),
        "generation.owner.1",
        "publish.owner.1",
        'a',
        'b',
        vec![dependency("missing", "generation.missing.1")],
        None,
        b"missing",
    );
    assert!(matches!(
        storage.append_replay(&missing_owner, &operation),
        Err(GraphPublicationStoreErrorV1::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay dependency generation"
            }
        ))
    ));
    assert_eq!(
        storage.replay(&missing_owner.key, &operation).unwrap(),
        GraphPublicationReplayLookupV1::Missing
    );

    let dependency_replay = replay(
        projection("dependency"),
        "generation.dependency.1",
        "publish.dependency.1",
        'c',
        'd',
        None,
        b"dependency",
    );
    append_with_fresh_context(&mut storage, &dependency_replay, "dependency.replay").unwrap();
    let unverified_owner = replay_with_dependencies(
        projection("unverified-owner"),
        "generation.owner.1",
        "publish.owner.1",
        'e',
        'f',
        vec![dependency("dependency", "generation.dependency.1")],
        None,
        b"unverified",
    );
    assert!(matches!(
        storage.append_replay(&unverified_owner, &operation),
        Err(GraphPublicationStoreErrorV1::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay dependency generation"
            }
        ))
    ));

    advance_head(&mut storage, &dependency_replay);
    let verified_owner = replay_with_dependencies(
        projection("verified-owner"),
        "generation.owner.1",
        "publish.owner.1",
        'a',
        'b',
        vec![dependency("dependency", "generation.dependency.1")],
        None,
        b"verified",
    );
    assert!(matches!(
        append_with_fresh_context(&mut storage, &verified_owner, "dependency.owner").unwrap(),
        GraphReplayAppendOutcomeV1::Appended(_)
    ));
    assert!(matches!(
        storage.replay(&verified_owner.key, &operation).unwrap(),
        GraphPublicationReplayLookupV1::Active(record)
            if record.publication.direct_dependency_generations
                == verified_owner.direct_dependency_generations
    ));

    let retired_first = replay(
        projection("retired-dependency"),
        "generation.retired.1",
        "publish.retired.1",
        'a',
        'b',
        None,
        b"retired-one",
    );
    append_with_fresh_context(&mut storage, &retired_first, "dependency.retired.first").unwrap();
    let retired_first_head = advance_head(&mut storage, &retired_first);
    let retired_second = replay(
        projection("retired-dependency"),
        "generation.retired.2",
        "publish.retired.2",
        'c',
        'd',
        Some(retired_first_head),
        b"retired-two",
    );
    append_with_fresh_context(&mut storage, &retired_second, "dependency.retired.second").unwrap();
    advance_head(&mut storage, &retired_second);
    let (retire_control, retire_probe) = context("dependency-retired");
    let retire_operation =
        GraphPublicationOperationContextV1::new(&retire_control, &retire_probe).unwrap();
    assert!(matches!(
        storage
            .retire_replay(&retirement(&retired_first), &retire_operation)
            .unwrap(),
        GraphReplayRetirementOutcomeV1::Retired(_)
    ));
    let retired_owner = replay_with_dependencies(
        projection("retired-owner"),
        "generation.owner.1",
        "publish.owner.1",
        'e',
        'f',
        vec![dependency("retired-dependency", "generation.retired.1")],
        None,
        b"retired-owner",
    );
    assert!(matches!(
        storage.append_replay(&retired_owner, &operation),
        Err(GraphPublicationStoreErrorV1::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay dependency generation"
            }
        ))
    ));
}

#[test]
fn dependency_decode_rejects_non_contiguous_ordinals() {
    let fixture = Fixture::new();
    let mut storage = fixture.storage();
    let dependency_replay = replay(
        projection("ordinal-dependency"),
        "generation.dependency.1",
        "publish.dependency.1",
        'a',
        'b',
        None,
        b"dependency",
    );
    let (control, probe) = context("ordinal.dependency.append");
    let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    append_with_fresh_context(&mut storage, &dependency_replay, "ordinal.dependency").unwrap();
    advance_head(&mut storage, &dependency_replay);

    let owner = replay_with_dependencies(
        projection("ordinal-owner"),
        "generation.owner.1",
        "publish.owner.1",
        'c',
        'd',
        vec![dependency("ordinal-dependency", "generation.dependency.1")],
        None,
        b"owner",
    );
    let (control, probe) = context("ordinal.owner.append");
    let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let sequence = match append_with_fresh_context(&mut storage, &owner, "ordinal.owner").unwrap() {
        GraphReplayAppendOutcomeV1::Appended(record) => record.sequence,
        outcome => panic!("unexpected owner append outcome: {outcome:?}"),
    };
    fixture
        .handle
        .execute(
            ExactSqlStatement::new(
                "UPDATE graph_publication_replay_dependencies_v1
                 SET ordinal=1 WHERE owner_replay_sequence=?1"
                    .to_owned(),
                vec![ExactSqlValue::Integer(
                    i64::try_from(sequence.get()).unwrap(),
                )],
            )
            .unwrap(),
        )
        .unwrap();

    assert!(matches!(
        storage.replay(&owner.key, &operation),
        Err(GraphPublicationStoreErrorV1::Corrupt(_))
    ));
}

#[test]
fn retirement_refuses_active_inbound_dependents() {
    let fixture = Fixture::new();
    let mut storage = fixture.storage();
    let (control, probe) = context("dependency-retirement");
    let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let first = replay(
        projection("dependency"),
        "generation.dependency.1",
        "publish.dependency.1",
        'a',
        'b',
        None,
        b"one",
    );
    append_with_fresh_context(&mut storage, &first, "inbound.first").unwrap();
    let first_head = advance_head(&mut storage, &first);
    let second = replay(
        projection("dependency"),
        "generation.dependency.2",
        "publish.dependency.2",
        'c',
        'd',
        Some(first_head),
        b"two",
    );
    append_with_fresh_context(&mut storage, &second, "inbound.second").unwrap();
    advance_head(&mut storage, &second);
    let owner = replay_with_dependencies(
        projection("owner"),
        "generation.owner.1",
        "publish.owner.1",
        'e',
        'f',
        vec![dependency("dependency", "generation.dependency.1")],
        None,
        b"owner",
    );
    append_with_fresh_context(&mut storage, &owner, "inbound.owner").unwrap();

    let (retire_control, retire_probe) = context("dependency-retirement-commit");
    let retire_operation =
        GraphPublicationOperationContextV1::new(&retire_control, &retire_probe).unwrap();
    assert_eq!(
        storage
            .retire_replay(&retirement(&first), &retire_operation)
            .unwrap(),
        GraphReplayRetirementOutcomeV1::Conflict
    );
}

#[test]
fn dependency_append_and_retirement_race_preserves_one_valid_state() {
    let fixture = Fixture::new();
    let mut setup = fixture.storage();
    let first = replay(
        projection("racing-dependency"),
        "generation.dependency.1",
        "publish.dependency.1",
        'a',
        'b',
        None,
        b"one",
    );
    append_with_fresh_context(&mut setup, &first, "race.first").unwrap();
    let first_head = advance_head(&mut setup, &first);
    let second = replay(
        projection("racing-dependency"),
        "generation.dependency.2",
        "publish.dependency.2",
        'c',
        'd',
        Some(first_head),
        b"two",
    );
    append_with_fresh_context(&mut setup, &second, "race.second").unwrap();
    advance_head(&mut setup, &second);
    let owner = replay_with_dependencies(
        projection("racing-owner"),
        "generation.owner.1",
        "publish.owner.1",
        'e',
        'f',
        vec![dependency("racing-dependency", "generation.dependency.1")],
        None,
        b"owner",
    );

    let barrier = Arc::new(Barrier::new(2));
    let append_handle = fixture.handle.clone();
    let append_barrier = Arc::clone(&barrier);
    let append_owner = owner.clone();
    let append_thread = std::thread::spawn(move || {
        let (control, probe) = context("race.append");
        let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        append_barrier.wait();
        GraphPublicationExactSqlStorage::from_authorized_handle(append_handle)
            .unwrap()
            .append_replay(&append_owner, &operation)
    });
    let retire_handle = fixture.handle.clone();
    let retire_barrier = barrier;
    let retire_first = first.clone();
    let retire_thread = std::thread::spawn(move || {
        let (control, probe) = context("race.retire");
        let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        retire_barrier.wait();
        GraphPublicationExactSqlStorage::from_authorized_handle(retire_handle)
            .unwrap()
            .retire_replay(&retirement(&retire_first), &operation)
    });

    let append_outcome = append_thread.join().unwrap();
    let retirement_outcome = retire_thread.join().unwrap();
    let append_won = matches!(&append_outcome, Ok(GraphReplayAppendOutcomeV1::Appended(_)))
        && matches!(
            &retirement_outcome,
            Ok(GraphReplayRetirementOutcomeV1::Conflict)
        );
    let retirement_won = matches!(
        &append_outcome,
        Err(GraphPublicationStoreErrorV1::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay dependency generation"
            }
        ))
    ) && matches!(
        &retirement_outcome,
        Ok(GraphReplayRetirementOutcomeV1::Retired(_))
    );
    assert!(
        append_won || retirement_won,
        "atomic writer serialization must preserve either the dependency or its retirement: \
         append={append_outcome:?}, retirement={retirement_outcome:?}"
    );

    let (control, probe) = context("race.observe");
    let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let mut observed = fixture.storage();
    match observed.replay(&owner.key, &operation).unwrap() {
        GraphPublicationReplayLookupV1::Active(_) => assert!(matches!(
            observed.replay(&first.key, &operation).unwrap(),
            GraphPublicationReplayLookupV1::Active(_)
        )),
        GraphPublicationReplayLookupV1::Missing => assert!(matches!(
            observed.replay(&first.key, &operation).unwrap(),
            GraphPublicationReplayLookupV1::Retired(_)
        )),
        GraphPublicationReplayLookupV1::Retired(_) => {
            panic!("the dependent owner was never eligible for retirement")
        }
    }
}

#[test]
fn retired_cleanup_retains_source_until_exact_finalization() {
    let fixture = Fixture::new();
    let mut storage = fixture.storage();
    let projection = projection("code");
    let (control, probe) = context("cleanup-setup");
    let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let first = replay(
        projection.clone(),
        "generation.1",
        "publish.1",
        'a',
        'b',
        None,
        b"cleanup-source",
    );
    append_with_fresh_context(&mut storage, &first, "cleanup.first").unwrap();
    let first_head = advance_head(&mut storage, &first);
    let second = replay(
        projection.clone(),
        "generation.2",
        "publish.2",
        'c',
        'd',
        Some(first_head),
        b"current",
    );
    append_with_fresh_context(&mut storage, &second, "cleanup.second").unwrap();
    advance_head(&mut storage, &second);

    let (retire_control, retire_probe) = context("cleanup-retire");
    let retire_operation =
        GraphPublicationOperationContextV1::new(&retire_control, &retire_probe).unwrap();
    let tombstone = match storage
        .retire_replay(&retirement(&first), &retire_operation)
        .unwrap()
    {
        GraphReplayRetirementOutcomeV1::Retired(tombstone) => tombstone,
        outcome => panic!("unexpected retirement outcome: {outcome:?}"),
    };
    assert_eq!(
        tombstone.canonical_replay_source.as_deref(),
        Some(&b"cleanup-source"[..])
    );
    let mut restarted = fixture.storage();
    let cleanup = restarted
        .retired_cleanup_page(
            &GraphPublicationRetiredCleanupPageRequestV1::new(projection.clone(), None, 1).unwrap(),
            &operation,
        )
        .unwrap();
    assert_eq!(cleanup.records, vec![tombstone]);

    let mut changed = retirement(&first);
    changed.input_digest = GraphPublicationInputDigestV1::new(digest('f')).unwrap();
    let (conflict_control, conflict_probe) = context("cleanup-finalize-conflict");
    let conflict_operation =
        GraphPublicationOperationContextV1::new(&conflict_control, &conflict_probe).unwrap();
    assert_eq!(
        restarted
            .finalize_retired_replay_cleanup(&changed, &conflict_operation)
            .unwrap(),
        GraphRetiredReplayCleanupFinalizeOutcomeV1::Conflict
    );
    assert_eq!(
        restarted
            .retired_cleanup_page(
                &GraphPublicationRetiredCleanupPageRequestV1::new(projection.clone(), None, 1,)
                    .unwrap(),
                &operation,
            )
            .unwrap()
            .records[0]
            .canonical_replay_source
            .as_deref(),
        Some(&b"cleanup-source"[..])
    );

    let (finalize_control, finalize_probe) = context("cleanup-finalize");
    let finalize_operation =
        GraphPublicationOperationContextV1::new(&finalize_control, &finalize_probe).unwrap();
    assert!(matches!(
        restarted
            .finalize_retired_replay_cleanup(&retirement(&first), &finalize_operation)
            .unwrap(),
        GraphRetiredReplayCleanupFinalizeOutcomeV1::Finalized(tombstone)
            if tombstone.canonical_replay_source.is_none()
    ));
    assert!(matches!(
        restarted.replay(&first.key, &operation).unwrap(),
        GraphPublicationReplayLookupV1::Retired(tombstone)
            if tombstone.canonical_replay_source.is_none()
    ));

    let (retry_control, retry_probe) = context("cleanup-finalize-retry");
    let retry_operation =
        GraphPublicationOperationContextV1::new(&retry_control, &retry_probe).unwrap();
    assert!(matches!(
        restarted
            .finalize_retired_replay_cleanup(&retirement(&first), &retry_operation)
            .unwrap(),
        GraphRetiredReplayCleanupFinalizeOutcomeV1::ExactReplay(_)
    ));
}

#[test]
fn retired_cleanup_materializes_only_the_near_limit_record_admitted_to_each_page() {
    let fixture = Fixture::new();
    let mut storage = fixture.storage();
    let projection = projection("large-cleanup");
    let (control, probe) = context("large-cleanup-setup");
    let operation = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let source_bytes = tracedecay_store::MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1 - 2;
    let first = replay(
        projection.clone(),
        "generation.large.1",
        "publish.large.1",
        'a',
        'b',
        None,
        &vec![1; source_bytes],
    );
    append_with_fresh_context(&mut storage, &first, "near-limit.first").unwrap();
    let first_head = advance_head(&mut storage, &first);
    let second = replay(
        projection.clone(),
        "generation.large.2",
        "publish.large.2",
        'c',
        'd',
        Some(first_head),
        &vec![2; source_bytes],
    );
    append_with_fresh_context(&mut storage, &second, "near-limit.second").unwrap();
    let second_head = advance_head(&mut storage, &second);
    let current = replay(
        projection.clone(),
        "generation.large.3",
        "publish.large.3",
        'e',
        'f',
        Some(second_head),
        b"current",
    );
    append_with_fresh_context(&mut storage, &current, "near-limit.current").unwrap();
    advance_head(&mut storage, &current);

    for (suffix, publication) in [
        ("large-cleanup-retire-1", &first),
        ("large-cleanup-retire-2", &second),
    ] {
        let (retire_control, retire_probe) = context(suffix);
        let retire_operation =
            GraphPublicationOperationContextV1::new(&retire_control, &retire_probe).unwrap();
        assert!(matches!(
            storage
                .retire_replay(&retirement(publication), &retire_operation)
                .unwrap(),
            GraphReplayRetirementOutcomeV1::Retired(_)
        ));
    }

    let first_page = storage
        .retired_cleanup_page(
            &GraphPublicationRetiredCleanupPageRequestV1::new(
                projection.clone(),
                None,
                tracedecay_store::MAX_GRAPH_REPLAY_PAGE_RECORDS_V1,
            )
            .unwrap(),
            &operation,
        )
        .unwrap();
    assert_eq!(first_page.records.len(), 1);
    assert_eq!(first_page.records[0].key, first.key);
    let continuation = first_page
        .continuation
        .expect("the second near-limit source must remain");

    let second_page = storage
        .retired_cleanup_page(
            &GraphPublicationRetiredCleanupPageRequestV1::new(
                projection,
                Some(continuation),
                tracedecay_store::MAX_GRAPH_REPLAY_PAGE_RECORDS_V1,
            )
            .unwrap(),
            &operation,
        )
        .unwrap();
    assert_eq!(second_page.records.len(), 1);
    assert_eq!(second_page.records[0].key, second.key);
    assert_eq!(
        second_page.records[0].expected_prior_head,
        second.expected_prior_head
    );
    assert_eq!(second_page.continuation, None);
}

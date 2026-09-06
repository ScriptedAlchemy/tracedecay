use super::*;
use tracedecay_store::{
    SemanticVectorCodeScopeHash, SemanticVectorSourceScopeBindingLookup,
    SemanticVectorStageCancelOutcome,
};

#[test]
fn final_schema_has_no_vector_or_source_payload_column() {
    let fixture = Fixture::new();
    let rows = fixture
        .handle
        .query(
            ExactSqlStatement::new(
                "SELECT name,type FROM pragma_table_info('semantic_vector_stages')
                 UNION ALL
                 SELECT name,type FROM pragma_table_info('semantic_vector_stage_batches')
                 UNION ALL
                 SELECT name,type FROM pragma_table_info('semantic_vector_stage_chunk_receipts')"
                    .to_owned(),
                vec![],
            )
            .unwrap(),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
    assert!(rows.rows.iter().all(|row| {
        !matches!(&row.values[1], ExactSqlValue::Text(kind) if kind == "BLOB")
            && !matches!(
                &row.values[0],
                ExactSqlValue::Text(name)
                    if matches!(
                        name.as_str(),
                        "vector_payload" | "embedding_bytes" | "source_content" | "source_payload"
                    )
            )
    }));
}

#[test]
fn pending_stage_reservation_rejects_generic_graph_replay_append() {
    let fixture = Fixture::new();
    let plan = plan(
        &fixture,
        "pending-reservation",
        chunk_manifest("chunk.pending-reservation"),
    );
    let (control, probe) = operation("pending.reservation.begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();

    let (control, probe) = operation("pending.reservation.generic-append");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_replay(&publication_replay(&plan), &context),
        Err(GraphPublicationStoreErrorV1::Infrastructure)
    ));
}

#[test]
fn cancelled_attempt_can_be_rebuilt_and_published_generation_recovers_exactly() {
    let fixture = Fixture::new();
    let empty_manifest = semantic_vector_chunk_manifest_digest(&[]).unwrap();
    let cancelled = plan_with_count(&fixture, "semantic-generation-attempt", empty_manifest, 0);
    let (control, probe) = operation("semantic-generation.cancelled.begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&cancelled, &context).unwrap();
    let (control, probe) = operation("semantic-generation.cancelled.cancel");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .cancel_stage(&cancelled.key, &cancelled.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageCancelOutcome::Cancelled(_)
    ));

    let published = alternative_publication_plan(
        &cancelled,
        "published-attempt",
        "generation.published-attempt",
        "publication.published-attempt",
    );
    publish_empty_stage(&fixture, &published, "semantic-generation.published");

    let lookup_key = SemanticVectorPublishedGenerationKey {
        projection: published.key.projection.clone(),
        semantic_generation_id: published.semantic_generation_id.clone(),
    };
    let (control, probe) = operation("semantic-generation.lookup.restart");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let lookup = fixture
        .storage()
        .published_semantic_generation(&lookup_key, &context)
        .unwrap();
    assert!(matches!(
        &lookup,
        SemanticVectorPublishedGenerationLookup::Published { record, .. }
            if record.plan == published
    ));

    let retry = alternative_publication_plan(
        &published,
        "response-loss-retry",
        "generation.response-loss-retry",
        "publication.response-loss-retry",
    );
    let (control, probe) = operation("semantic-generation.begin.response-loss");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&retry, &context).unwrap(),
        SemanticVectorStageBeginOutcome::Published { record, .. }
            if record.plan == published
    ));

    let changed = SemanticVectorStagePlan::new(
        retry.key.projection.clone(),
        SemanticVectorBuildId::new("build.changed-semantic-plan").unwrap(),
        retry.semantic_generation_id.clone(),
        retry.base_generation.clone(),
        GraphPublicationKeyV1::new(
            retry.key.projection.clone(),
            GraphGenerationIdV1::new("generation.changed-semantic-plan").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publication.changed-semantic-plan").unwrap(),
        ),
        retry.source_scope.clone(),
        retry.code_scope_hash.clone(),
        retry.source_generation.clone(),
        retry.source_dependency.clone(),
        SemanticVectorReconstructionRecipe {
            source_manifest_digest: digest('f'),
            ..retry.recipe.clone()
        },
        1,
        Some(match lookup {
            SemanticVectorPublishedGenerationLookup::Published { verified_head, .. } => {
                *verified_head
            }
            SemanticVectorPublishedGenerationLookup::Missing => unreachable!(),
        }),
        retry.initial_checkpoint_digest.clone(),
        retry.writer_fence.clone(),
    )
    .unwrap();
    let (control, probe) = operation("semantic-generation.begin.changed-plan");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&changed, &context).unwrap(),
        SemanticVectorStageBeginOutcome::SemanticGenerationConflict { existing }
            if existing.plan == published
    ));
}

#[test]
fn historical_published_semantic_generation_remains_lookupable_after_new_head() {
    let fixture = Fixture::new();
    let empty_manifest = semantic_vector_chunk_manifest_digest(&[]).unwrap();
    let first = plan_with_count(&fixture, "historical-heads", empty_manifest.clone(), 0);
    publish_empty_stage(&fixture, &first, "historical-heads.first");
    let first_key = SemanticVectorPublishedGenerationKey {
        projection: first.key.projection.clone(),
        semantic_generation_id: first.semantic_generation_id.clone(),
    };
    let (control, probe) = operation("historical-heads.first.lookup");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let first_head = match fixture
        .storage()
        .published_semantic_generation(&first_key, &context)
        .unwrap()
    {
        SemanticVectorPublishedGenerationLookup::Published { verified_head, .. } => *verified_head,
        SemanticVectorPublishedGenerationLookup::Missing => panic!("first generation missing"),
    };
    let second = SemanticVectorStagePlan::new(
        first.key.projection.clone(),
        SemanticVectorBuildId::new("build.historical-heads.second").unwrap(),
        VectorGenerationIdV1::new(
            canonical_sha256(&("semantic-vector-test-generation", "historical-heads.second"))
                .unwrap(),
        ),
        Some(first.semantic_generation_id.clone()),
        GraphPublicationKeyV1::new(
            first.key.projection.clone(),
            GraphGenerationIdV1::new("generation.historical-heads.second").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publication.historical-heads.second").unwrap(),
        ),
        first.source_scope.clone(),
        first.code_scope_hash.clone(),
        first.source_generation.clone(),
        first.source_dependency.clone(),
        SemanticVectorReconstructionRecipe {
            expected_chunk_manifest_digest: empty_manifest,
            ..first.recipe.clone()
        },
        0,
        Some(first_head.clone()),
        first.initial_checkpoint_digest.clone(),
        first.writer_fence.clone(),
    )
    .unwrap();
    publish_empty_stage(&fixture, &second, "historical-heads.second");

    let (control, probe) = operation("historical-heads.first.lookup-after-second");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .published_semantic_generation(&first_key, &context)
            .unwrap(),
        SemanticVectorPublishedGenerationLookup::Published { record, .. }
            if record.plan == first
    ));
    let (control, probe) = operation("historical-heads.first.settle-response-loss");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .settle_published(
                &SemanticVectorStagePublishSettlement {
                    stage: first.key.clone(),
                    verified_head: first_head,
                },
                &first.writer_fence,
                &context,
            )
            .unwrap(),
        SemanticVectorStagePublishOutcome::ExactReplay(record)
            if record.plan == first
    ));
}

#[test]
fn historical_reader_snapshot_does_not_retain_concurrently_retired_generation() {
    let fixture = Fixture::new();
    let first = plan_with_count(
        &fixture,
        "historical-snapshot.retired",
        semantic_vector_chunk_manifest_digest(&[]).unwrap(),
        0,
    );
    publish_empty_stage(&fixture, &first, "historical-snapshot.first");
    let first_key = SemanticVectorPublishedGenerationKey {
        projection: first.key.projection.clone(),
        semantic_generation_id: first.semantic_generation_id.clone(),
    };
    let first_replay = publication_replay(&first);
    let retirement = SemanticVectorPublishedRetirement {
        stage: first.key.clone(),
        semantic_generation_id: first.semantic_generation_id.clone(),
        replay: GraphPublicationReplayRetirementV1::new(
            first_replay.key.clone(),
            first_replay.input_digest.clone(),
            first_replay.dependency_generation_closure_digest.clone(),
            first_replay.direct_dependency_generations.clone(),
            first_replay.expected_prior_head.clone(),
            first_replay.expected_recovered_digest.clone(),
            first_replay.canonical_replay_source_digest.clone(),
        )
        .unwrap(),
        writer_fence: first.writer_fence.clone(),
    };
    let (control, probe) = operation("historical-snapshot.reader");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let snapshot = super::super::support::begin_read_snapshot(
        &fixture.handle,
        &context,
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let historical = super::super::published::published_stage_for(&snapshot, &first_key)
        .unwrap()
        .expect("first publication is visible when the snapshot begins");
    let historical_head =
        super::super::published::published_stage_evidence_in_snapshot(&snapshot, &historical)
            .unwrap();
    let second = SemanticVectorStagePlan::new(
        first.key.projection.clone(),
        SemanticVectorBuildId::new("build.historical-snapshot.published").unwrap(),
        VectorGenerationIdV1::new(
            canonical_sha256(&(
                "semantic-vector-test-generation",
                "historical-snapshot.published",
            ))
            .unwrap(),
        ),
        None,
        GraphPublicationKeyV1::new(
            first.key.projection.clone(),
            GraphGenerationIdV1::new("generation.historical-snapshot.published").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publication.historical-snapshot.published")
                .unwrap(),
        ),
        first.source_scope.clone(),
        first.code_scope_hash.clone(),
        first.source_generation.clone(),
        first.source_dependency.clone(),
        first.recipe.clone(),
        0,
        Some(historical_head),
        first.initial_checkpoint_digest.clone(),
        first.writer_fence.clone(),
    )
    .unwrap();
    let second_key = SemanticVectorPublishedGenerationKey {
        projection: second.key.projection.clone(),
        semantic_generation_id: second.semantic_generation_id.clone(),
    };
    assert!(
        super::super::published::published_stage_for(&snapshot, &second_key)
            .unwrap()
            .is_none()
    );

    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_start = std::sync::Arc::clone(&start);
    let mut writer = fixture.storage();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            writer_start.wait();
            publish_empty_stage_with_storage(&mut writer, &second, "historical-snapshot.second");
            let (control, probe) = operation("historical-snapshot.retire");
            let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
            assert!(matches!(
                writer
                    .retire_published_generation(&retirement, &context)
                    .unwrap(),
                SemanticVectorPublishedRetirementOutcome::Retired(_)
            ));
        });
        start.wait();
        writer.join().expect("publication and retirement writer");
    });

    assert_eq!(&historical.record.plan, &first);
    assert_eq!(
        super::super::published::published_stage_evidence_in_snapshot(&snapshot, &historical)
            .unwrap()
            .key,
        first_replay.key
    );
    assert!(
        super::super::published::published_stage_for(&snapshot, &second_key)
            .unwrap()
            .is_none(),
        "the reader remains a consistent historical snapshot"
    );
    drop(snapshot);

    let (control, probe) = operation("historical-snapshot.first.after");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .published_semantic_generation(&first_key, &context)
            .unwrap(),
        SemanticVectorPublishedGenerationLookup::Missing
    );
    let (control, probe) = operation("historical-snapshot.second.after");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .published_semantic_generation(&second_key, &context)
            .unwrap(),
        SemanticVectorPublishedGenerationLookup::Published { record, .. }
            if record.plan.key.projection == second_key.projection
    ));
}

#[test]
fn retirement_tombstone_and_relational_descendants_commit_atomically() {
    let fixture = Fixture::new();
    let empty_manifest = semantic_vector_chunk_manifest_digest(&[]).unwrap();
    let first = plan_with_count(&fixture, "retirement.first", empty_manifest.clone(), 0);
    publish_empty_stage(&fixture, &first, "retirement.first");
    let first_replay = publication_replay(&first);
    let first_head = {
        let key = SemanticVectorPublishedGenerationKey {
            projection: first.key.projection.clone(),
            semantic_generation_id: first.semantic_generation_id.clone(),
        };
        let (control, probe) = operation("retirement.first.lookup");
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        match fixture
            .storage()
            .published_semantic_generation(&key, &context)
            .unwrap()
        {
            SemanticVectorPublishedGenerationLookup::Published { verified_head, .. } => {
                *verified_head
            }
            SemanticVectorPublishedGenerationLookup::Missing => panic!("first is missing"),
        }
    };
    let second = SemanticVectorStagePlan::new(
        first.key.projection.clone(),
        SemanticVectorBuildId::new("build.retirement.second").unwrap(),
        VectorGenerationIdV1::new(
            canonical_sha256(&("semantic-vector-test-generation", "retirement.second")).unwrap(),
        ),
        None,
        GraphPublicationKeyV1::new(
            first.key.projection.clone(),
            GraphGenerationIdV1::new("generation.retirement.second").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publication.retirement.second").unwrap(),
        ),
        first.source_scope.clone(),
        first.code_scope_hash.clone(),
        first.source_generation.clone(),
        first.source_dependency.clone(),
        SemanticVectorReconstructionRecipe {
            expected_chunk_manifest_digest: empty_manifest,
            ..first.recipe.clone()
        },
        0,
        Some(first_head),
        first.initial_checkpoint_digest.clone(),
        first.writer_fence.clone(),
    )
    .unwrap();
    publish_empty_stage(&fixture, &second, "retirement.second");
    let retirement = SemanticVectorPublishedRetirement {
        stage: first.key.clone(),
        semantic_generation_id: first.semantic_generation_id.clone(),
        replay: GraphPublicationReplayRetirementV1::new(
            first_replay.key.clone(),
            first_replay.input_digest.clone(),
            first_replay.dependency_generation_closure_digest.clone(),
            first_replay.direct_dependency_generations.clone(),
            first_replay.expected_prior_head.clone(),
            first_replay.expected_recovered_digest.clone(),
            first_replay.canonical_replay_source_digest.clone(),
        )
        .unwrap(),
        writer_fence: first.writer_fence.clone(),
    };
    let (control, probe) = operation("retirement.atomic");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .retire_published_generation(&retirement, &context)
            .unwrap(),
        SemanticVectorPublishedRetirementOutcome::Retired(_)
    ));
    let (control, probe) = operation("retirement.census");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let census = fixture
        .storage()
        .stage_census(
            &SemanticVectorStageCensusRequest::for_shard(
                first.key.projection.shard_id.clone(),
                None,
                256,
            )
            .unwrap(),
            &context,
        )
        .unwrap();
    assert!(
        census
            .records
            .iter()
            .all(|record| record.stage.plan.key != first.key)
    );
    let (control, probe) = operation("retirement.cleanup");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .pending_retirement_cleanup(&first.key.projection.shard_id, &context)
            .unwrap()
            .unwrap()
            .retirement,
        retirement
    );
}

#[test]
fn published_generation_referenced_as_pending_base_survives_retirement() {
    let fixture = Fixture::new();
    let empty_manifest = semantic_vector_chunk_manifest_digest(&[]).unwrap();
    let first = plan_with_count(&fixture, "retirement.live-base", empty_manifest.clone(), 0);
    publish_empty_stage(&fixture, &first, "retirement.live-base");
    let first_replay = publication_replay(&first);
    let first_head = {
        let key = SemanticVectorPublishedGenerationKey {
            projection: first.key.projection.clone(),
            semantic_generation_id: first.semantic_generation_id.clone(),
        };
        let (control, probe) = operation("retirement.live-base.lookup");
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        match fixture
            .storage()
            .published_semantic_generation(&key, &context)
            .unwrap()
        {
            SemanticVectorPublishedGenerationLookup::Published { verified_head, .. } => {
                *verified_head
            }
            SemanticVectorPublishedGenerationLookup::Missing => panic!("published base is missing"),
        }
    };
    let pending = SemanticVectorStagePlan::new(
        first.key.projection.clone(),
        SemanticVectorBuildId::new("build.retirement.live-base.pending").unwrap(),
        VectorGenerationIdV1::new(
            canonical_sha256(&(
                "semantic-vector-test-generation",
                "retirement.live-base.pending",
            ))
            .unwrap(),
        ),
        Some(first.semantic_generation_id.clone()),
        GraphPublicationKeyV1::new(
            first.key.projection.clone(),
            GraphGenerationIdV1::new("generation.retirement.live-base.pending").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publication.retirement.live-base.pending")
                .unwrap(),
        ),
        first.source_scope.clone(),
        first.code_scope_hash.clone(),
        first.source_generation.clone(),
        first.source_dependency.clone(),
        SemanticVectorReconstructionRecipe {
            expected_chunk_manifest_digest: empty_manifest,
            ..first.recipe.clone()
        },
        0,
        Some(first_head),
        first.initial_checkpoint_digest.clone(),
        first.writer_fence.clone(),
    )
    .unwrap();
    let (control, probe) = operation("retirement.live-base.pending.begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&pending, &context).unwrap(),
        SemanticVectorStageBeginOutcome::Begun(_)
    ));

    let retirement = SemanticVectorPublishedRetirement {
        stage: first.key.clone(),
        semantic_generation_id: first.semantic_generation_id.clone(),
        replay: GraphPublicationReplayRetirementV1::new(
            first_replay.key.clone(),
            first_replay.input_digest.clone(),
            first_replay.dependency_generation_closure_digest.clone(),
            first_replay.direct_dependency_generations.clone(),
            first_replay.expected_prior_head.clone(),
            first_replay.expected_recovered_digest.clone(),
            first_replay.canonical_replay_source_digest.clone(),
        )
        .unwrap(),
        writer_fence: first.writer_fence.clone(),
    };
    let (control, probe) = operation("retirement.live-base.retire");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .retire_published_generation(&retirement, &context)
            .unwrap(),
        SemanticVectorPublishedRetirementOutcome::Conflict
    );

    let key = SemanticVectorPublishedGenerationKey {
        projection: first.key.projection.clone(),
        semantic_generation_id: first.semantic_generation_id.clone(),
    };
    let (control, probe) = operation("retirement.live-base.survived");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .published_semantic_generation(&key, &context)
            .unwrap(),
        SemanticVectorPublishedGenerationLookup::Published { record, .. }
            if record.plan == first
    ));
}

#[test]
fn cancelled_retirement_removes_stage_descendants_and_replays_missing() {
    let fixture = Fixture::new();
    let plan = plan(
        &fixture,
        "cancelled-retirement",
        chunk_manifest("chunk.cancelled-retirement"),
    );
    let (control, probe) = operation("cancelled-retirement.begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();
    let (control, probe) = operation("cancelled-retirement.cancel");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .cancel_stage(&plan.key, &plan.writer_fence, &context)
        .unwrap();
    let request = SemanticVectorCancelledRetirement {
        stage: plan.key.clone(),
        writer_fence: plan.writer_fence.clone(),
    };
    for (suffix, expected) in [
        ("remove", SemanticVectorCancelledRetirementOutcome::Removed),
        (
            "replay",
            SemanticVectorCancelledRetirementOutcome::ExactMissing,
        ),
    ] {
        let (control, probe) = operation(&format!("cancelled-retirement.{suffix}"));
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        assert_eq!(
            fixture
                .storage()
                .remove_cancelled_generation(&request, &context)
                .unwrap(),
            expected
        );
    }
}

#[test]
fn project_census_is_bounded_and_advances_across_retired_worktree_rows() {
    let fixture = Fixture::new();
    for ordinal in 0..257 {
        let plan = plan(
            &fixture,
            &format!("bounded-census-{ordinal:03}"),
            chunk_manifest(&format!("chunk.bounded-census-{ordinal:03}")),
        );
        let (control, probe) = operation(&format!("bounded-census.{ordinal}.begin"));
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        fixture.storage().begin_stage(&plan, &context).unwrap();
        let (control, probe) = operation(&format!("bounded-census.{ordinal}.cancel"));
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        fixture
            .storage()
            .cancel_stage(&plan.key, &plan.writer_fence, &context)
            .unwrap();
    }
    let shard = fixture.binding.shard_id.clone();
    let (control, probe) = operation("bounded-census.first-page");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let first = fixture
        .storage()
        .stage_census(
            &SemanticVectorStageCensusRequest::for_shard(shard.clone(), None, 256).unwrap(),
            &context,
        )
        .unwrap();
    assert_eq!(first.records.len(), 256);
    let continuation = first.continuation.expect("first page must continue");
    let newcomer = plan(
        &fixture,
        "bounded-census-newcomer",
        chunk_manifest("chunk.bounded-census-newcomer"),
    );
    let (control, probe) = operation("bounded-census.newcomer.begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&newcomer, &context).unwrap();
    let (control, probe) = operation("bounded-census.second-page");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let drift = fixture
        .storage()
        .stage_census(
            &SemanticVectorStageCensusRequest::for_shard(shard.clone(), Some(continuation), 256)
                .unwrap(),
            &context,
        )
        .unwrap_err();
    assert!(matches!(
        drift,
        SemanticVectorStagingStoreError::CensusRevisionChanged { .. }
    ));
    let (control, probe) = operation("bounded-census.restart");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let restarted = fixture
        .storage()
        .stage_census(
            &SemanticVectorStageCensusRequest::for_shard(shard, None, 256).unwrap(),
            &context,
        )
        .unwrap();
    assert_eq!(restarted.records.len(), 256);
    assert!(restarted.continuation.is_some());
    assert!(restarted.complete_receipt.is_none());
}

#[test]
fn project_census_reaches_an_unmounted_worktree_projection_after_restart() {
    let fixture = Fixture::new();
    let current = plan(
        &fixture,
        "project-census.current",
        chunk_manifest("chunk.project-census.current"),
    );
    let retired_projection = GraphProjectionIdentityV1 {
        shard_id: current.key.projection.shard_id.clone(),
        namespace: current.key.projection.namespace.clone(),
        projection: GraphProjectionIdV1::new("semantic-vector.retired-worktree").unwrap(),
    };
    let retired = SemanticVectorStagePlan::new(
        retired_projection.clone(),
        SemanticVectorBuildId::new("build.project-census.retired").unwrap(),
        VectorGenerationIdV1::new(
            canonical_sha256(&("semantic-vector-test-generation", "project-census.retired"))
                .unwrap(),
        ),
        None,
        GraphPublicationKeyV1::new(
            retired_projection,
            GraphGenerationIdV1::new("generation.project-census.retired").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publication.project-census.retired").unwrap(),
        ),
        StoreShardIdV1::code(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
            ProjectId::new("project.fixture").unwrap(),
            RepositoryId::new("repository.fixture").unwrap(),
            CodeShardScopeV1::Worktree {
                worktree_id: WorktreeId::new("worktree.unmounted").unwrap(),
            },
        ),
        SemanticVectorCodeScopeHash::new("b".repeat(64)).unwrap(),
        current.source_generation.clone(),
        current.source_dependency.clone(),
        current.recipe.clone(),
        current.expected_chunk_count,
        None,
        current.initial_checkpoint_digest.clone(),
        current.writer_fence.clone(),
    )
    .unwrap();
    for (suffix, plan) in [("current", &current), ("retired", &retired)] {
        let (control, probe) = operation(&format!("project-census.{suffix}.begin"));
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        fixture.storage().begin_stage(plan, &context).unwrap();
        let (control, probe) = operation(&format!("project-census.{suffix}.cancel"));
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        fixture
            .storage()
            .cancel_stage(&plan.key, &plan.writer_fence, &context)
            .unwrap();
    }
    let (control, probe) = operation("project-census.restart");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let census = fixture
        .storage()
        .stage_census(
            &SemanticVectorStageCensusRequest::for_shard(
                fixture.binding.shard_id.clone(),
                None,
                256,
            )
            .unwrap(),
            &context,
        )
        .unwrap();
    assert_eq!(census.records.len(), 2);
    assert!(
        census
            .records
            .iter()
            .any(|record| record.stage.plan == retired)
    );
    assert_ne!(current.source_scope, retired.source_scope);
    let receipt = census
        .complete_receipt
        .expect("the restarted project census must be complete");
    assert_eq!(receipt.counts.cancelled, 2);
}

#[test]
fn exact_source_liveness_rejects_a_stale_project_census_revision() {
    let fixture = Fixture::new();
    let plan = plan(
        &fixture,
        "revision-bound-source-liveness",
        chunk_manifest("chunk.revision-bound-source-liveness"),
    );
    let (control, probe) = operation("revision-bound-source-liveness.begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();
    let (control, probe) = operation("revision-bound-source-liveness.census");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let receipt = fixture
        .storage()
        .stage_census(
            &SemanticVectorStageCensusRequest::for_shard(
                fixture.binding.shard_id.clone(),
                None,
                256,
            )
            .unwrap(),
            &context,
        )
        .unwrap()
        .complete_receipt
        .expect("single-page project census receipt");
    let (control, probe) = operation("revision-bound-source-liveness.exact");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(
        fixture
            .storage()
            .source_scope_has_live_reference(
                &fixture.binding.shard_id,
                &plan.source_scope,
                receipt.revision,
                &context,
            )
            .unwrap()
    );
    let (control, probe) = operation("revision-bound-source-liveness.cancel");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .cancel_stage(&plan.key, &plan.writer_fence, &context)
        .unwrap();
    let (control, probe) = operation("revision-bound-source-liveness.stale");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().source_scope_has_live_reference(
            &fixture.binding.shard_id,
            &plan.source_scope,
            receipt.revision,
            &context,
        ),
        Err(SemanticVectorStagingStoreError::CensusRevisionChanged { .. })
    ));
}

#[test]
fn source_scope_binding_survives_stage_retirement_until_exact_scope_collection() {
    let fixture = Fixture::new();
    let plan = plan(
        &fixture,
        "durable-source-scope-binding",
        chunk_manifest("chunk.durable-source-scope-binding"),
    );
    let (control, probe) = operation("durable-source-scope-binding.begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();
    let (control, probe) = operation("durable-source-scope-binding.cancel");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .cancel_stage(&plan.key, &plan.writer_fence, &context)
        .unwrap();
    let (control, probe) = operation("durable-source-scope-binding.retire");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .remove_cancelled_generation(
            &SemanticVectorCancelledRetirement {
                stage: plan.key.clone(),
                writer_fence: plan.writer_fence.clone(),
            },
            &context,
        )
        .unwrap();
    let (control, probe) = operation("durable-source-scope-binding.census");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let receipt = fixture
        .storage()
        .stage_census(
            &SemanticVectorStageCensusRequest::for_shard(
                fixture.binding.shard_id.clone(),
                None,
                256,
            )
            .unwrap(),
            &context,
        )
        .unwrap()
        .complete_receipt
        .expect("empty post-retirement census is complete");
    let (control, probe) = operation("durable-source-scope-binding.lookup");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .source_scope_binding(
                &fixture.binding.shard_id,
                &plan.code_scope_hash,
                receipt.revision,
                &context,
            )
            .unwrap(),
        SemanticVectorSourceScopeBindingLookup::Exact(plan.source_scope.clone())
    );
    let (control, probe) = operation("durable-source-scope-binding.remove");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(
        fixture
            .storage()
            .remove_source_scope_binding(
                &fixture.binding.shard_id,
                &plan.code_scope_hash,
                &plan.source_scope,
                receipt.revision,
                &context,
            )
            .unwrap()
    );
    let (control, probe) = operation("durable-source-scope-binding.stale");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().source_scope_binding(
            &fixture.binding.shard_id,
            &plan.code_scope_hash,
            receipt.revision,
            &context,
        ),
        Err(SemanticVectorStagingStoreError::CensusRevisionChanged { .. })
    ));
}

fn publish_empty_stage(fixture: &Fixture, plan: &SemanticVectorStagePlan, suffix: &str) {
    publish_empty_stage_with_storage(&mut fixture.storage(), plan, suffix);
}

fn publish_empty_stage_with_storage(
    storage: &mut SemanticVectorStagingExactSqlStorage,
    plan: &SemanticVectorStagePlan,
    suffix: &str,
) {
    let (control, probe) = operation(&format!("{suffix}.begin"));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        storage.begin_stage(plan, &context).unwrap(),
        SemanticVectorStageBeginOutcome::Begun(_)
    ));
    let receipt = control_receipt(&plan.key);
    let (control, probe) = operation(&format!("{suffix}.append"));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    storage
        .append_stage_batch(&receipt, &plan.writer_fence, &context)
        .unwrap();
    let settlement = SemanticVectorStageSettlement {
        batch: receipt.key.clone(),
        expected_receipt_digest: receipt.receipt_digest.clone(),
        terminal: SemanticVectorStageEffectTerminal::Applied {
            graph_batch_digest: digest('a'),
        },
    };
    let (control, probe) = operation(&format!("{suffix}.settle"));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    storage
        .settle_stage_batch(&settlement, &plan.writer_fence, &context)
        .unwrap();
    let replay = publication_replay(plan);
    let prepare = SemanticVectorStagePublicationPrepareRequest::new(
        plan.key.clone(),
        replay.clone(),
        receipt.checkpoint_digest,
    )
    .unwrap();
    let (control, probe) = operation(&format!("{suffix}.prepare"));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        storage
            .prepare_stage_publication(&prepare, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStagePublicationPrepareOutcome::ReadyToPublish(_)
    ));
    let (control, probe) = operation(&format!("{suffix}.cancel-ready-race"));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        storage
            .cancel_stage(&plan.key, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageCancelOutcome::ReadyToPublish(_)
    ));
    let head_request = GraphVerifiedHeadCompareAndSwapV1 {
        publication_key: replay.key.clone(),
        input_digest: replay.input_digest.clone(),
        dependency_generation_closure_digest: replay.dependency_generation_closure_digest.clone(),
        recovered_digest: replay.expected_recovered_digest.clone(),
        expected_prior_head: replay.expected_prior_head.clone(),
    };
    let (control, probe) = operation(&format!("{suffix}.head"));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let verified_head = match storage
        .compare_and_swap_verified_head(&head_request, &context)
        .unwrap()
    {
        GraphVerifiedHeadCasOutcomeV1::Advanced(head) => head,
        outcome => panic!("unexpected semantic generation head outcome: {outcome:?}"),
    };
    let (control, probe) = operation(&format!("{suffix}.publish"));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        storage
            .settle_published(
                &SemanticVectorStagePublishSettlement {
                    stage: plan.key.clone(),
                    verified_head,
                },
                &plan.writer_fence,
                &context,
            )
            .unwrap(),
        SemanticVectorStagePublishOutcome::Published(_)
    ));
    let (control, probe) = operation(&format!("{suffix}.cancel-published-race"));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        storage
            .cancel_stage(&plan.key, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageCancelOutcome::ReadyToPublish(record)
            if record.state == SemanticVectorStageState::Published
    ));
}

/// A store written before `4e57f2831` bound its semantic source scope to the
/// branch-labelled code shard. The producer now derives the checkout-scoped
/// shard, so the durable bijection sees the same code scope naming a different
/// source scope. That store must be told which pair it holds and which one was
/// asked for — an explicit rebuild path — never silently rebound onto the new
/// tuple.
#[test]
fn a_branch_bound_source_scope_refuses_the_checkout_scope_and_names_both_tuples() {
    let fixture = Fixture::new();
    let branch_scope = StoreShardIdV1::code(
        BrainId::new("brain.fixture").unwrap(),
        UserProfileId::new("profile.fixture").unwrap(),
        ProjectId::new("project.fixture").unwrap(),
        RepositoryId::new("repository.fixture").unwrap(),
        CodeShardScopeV1::Branch {
            worktree_id: WorktreeId::new("worktree.fixture").unwrap(),
            ref_id: tracedecay_domain::RefId::new("refs/heads/main").unwrap(),
        },
    );
    let legacy = plan_with_source_scope(
        &fixture,
        "branch-bound-legacy",
        chunk_manifest("chunk.branch-bound-legacy"),
        branch_scope.clone(),
    );
    let (control, probe) = operation("branch-bound.legacy-begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&legacy, &context).unwrap(),
        SemanticVectorStageBeginOutcome::Begun(_)
    ));

    // The producer after `4e57f2831` derives the worktree-scoped shard for the
    // same code scope hash. `plan` already uses `CodeShardScopeV1::Worktree`.
    let checkout = plan(
        &fixture,
        "branch-bound-checkout",
        chunk_manifest("chunk.branch-bound-checkout"),
    );
    assert_eq!(
        checkout.code_scope_hash, legacy.code_scope_hash,
        "this case only proves anything while both plans name one code scope"
    );
    assert_ne!(
        checkout.source_scope, legacy.source_scope,
        "the checkout-scoped source scope must differ from the branch-bound one"
    );
    let (control, probe) = operation("branch-bound.checkout-begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let refusal = fixture
        .storage()
        .begin_stage(&checkout, &context)
        .expect_err("a branch-bound durable binding must refuse the checkout scope");
    let SemanticVectorStagingStoreError::Corrupt(message) = refusal else {
        panic!("the conflicting binding must be typed corruption: {refusal:?}");
    };
    assert!(
        message.contains("conflicting durable source binding"),
        "{message}"
    );
    assert!(
        message.contains(checkout.code_scope_hash.as_str()),
        "the refusal must name the code scope both tuples share: {message}"
    );
    assert!(
        message.contains(serde_json::to_string(&checkout.source_scope).unwrap().as_str()),
        "the refusal must name the requested checkout source scope: {message}"
    );
    assert!(
        message.contains(serde_json::to_string(&branch_scope).unwrap().as_str()),
        "the refusal must name the retained branch-bound source scope: {message}"
    );

    // No silent rebinding: the durable row still names the branch scope.
    let (control, probe) = operation("branch-bound.census");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let receipt = fixture
        .storage()
        .stage_census(
            &SemanticVectorStageCensusRequest::for_shard(
                fixture.binding.shard_id.clone(),
                None,
                256,
            )
            .unwrap(),
            &context,
        )
        .unwrap()
        .complete_receipt
        .expect("bounded census is complete");
    let (control, probe) = operation("branch-bound.lookup");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .source_scope_binding(
                &fixture.binding.shard_id,
                &legacy.code_scope_hash,
                receipt.revision,
                &context,
            )
            .unwrap(),
        SemanticVectorSourceScopeBindingLookup::Exact(branch_scope),
        "the refused begin must not have rebound the durable row"
    );
}

fn plan_with_source_scope(
    fixture: &Fixture,
    name: &str,
    manifest: SemanticVectorChunkManifestDigest,
    source_scope: StoreShardIdV1,
) -> SemanticVectorStagePlan {
    let base = plan(fixture, name, manifest);
    SemanticVectorStagePlan::new(
        base.key.projection.clone(),
        base.key.build_id.clone(),
        base.semantic_generation_id.clone(),
        base.base_generation.clone(),
        base.publication_key.clone(),
        source_scope,
        base.code_scope_hash.clone(),
        base.source_generation.clone(),
        base.source_dependency.clone(),
        base.recipe.clone(),
        base.expected_chunk_count,
        base.expected_prior_verified_head.clone(),
        base.initial_checkpoint_digest.clone(),
        base.writer_fence.clone(),
    )
    .unwrap()
}

use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphDbRegistration, GraphDbRegistry,
    SemanticVectorRetentionAction, SemanticVectorRetentionCensus, SemanticVectorRetentionStep,
};
use tracedecay_store::{
    GraphPublicationOperationContextV1, GraphPublicationReplayLookupV1, GraphPublicationStoreV1,
    MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH, SemanticVectorCheckpointDigest,
    SemanticVectorPublicationAuthority, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorStageCancelOutcome,
    SemanticVectorStageResumeOutcome, SemanticVectorStageState, SemanticVectorStagingStore,
    SemanticVectorWriterFence,
};

#[path = "semantic_vector_staging_contract/fixture.rs"]
mod fixture;
#[path = "support/mod.rs"]
mod graph_support;

use fixture::{ContractFixture, NativeMismatch, settle_publication, with_context};
use graph_support::TestCancellation;

#[derive(Default)]
struct ApplyPause {
    calls: AtomicUsize,
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl ApplyPause {
    fn wait_until_reached(&self) {
        let mut state = self.state.lock().expect("apply pause lock");
        while !state.0 {
            state = self.changed.wait(state).expect("apply pause wait");
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("apply pause lock");
        state.1 = true;
        self.changed.notify_all();
    }
}

impl GraphCancellation for ApplyPause {
    fn is_cancelled(&self) -> bool {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 2 {
            let mut state = self.state.lock().expect("apply pause lock");
            state.0 = true;
            self.changed.notify_all();
            while !state.1 {
                state = self.changed.wait(state).expect("apply pause wait");
            }
        }
        false
    }
}

#[derive(Default)]
struct CancelOnSecondCheck(AtomicUsize);

impl GraphCancellation for CancelOnSecondCheck {
    fn is_cancelled(&self) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst) >= 1
    }
}

#[derive(Default)]
struct SwitchCancellation(AtomicBool);

impl GraphCancellation for SwitchCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn registration_with_cancellation(
    mut registration: GraphDbRegistration,
    cancellation: Arc<dyn GraphCancellation>,
) -> GraphDbRegistration {
    registration.cancellation = cancellation;
    registration
}

fn finalize_retention_step(
    registry: &GraphDbRegistry,
    registration: GraphDbRegistration,
    authority: &mut dyn SemanticVectorPublicationAuthority,
    context: &GraphPublicationOperationContextV1<'_>,
    writer_fence: &SemanticVectorWriterFence,
    step: SemanticVectorRetentionStep,
) -> Result<SemanticVectorRetentionCensus, GraphDbError> {
    match step {
        SemanticVectorRetentionStep::Census(census) => Ok(census),
        SemanticVectorRetentionStep::Reserved {
            mut census,
            reservation,
        } => {
            census.action = registry.finalize_semantic_vector_retirement(
                registration,
                authority,
                context,
                writer_fence,
                *reservation,
            )?;
            Ok(census)
        }
    }
}

#[test]
fn native_batch_admission_rejects_every_chunk_binding_mismatch() {
    let fixture = ContractFixture::new();
    for (name, mismatch) in [
        ("wrong-id", NativeMismatch::ChunkId),
        ("wrong-digest", NativeMismatch::ChunkDigest),
        ("wrong-operation", NativeMismatch::Operation),
        ("wrong-output", NativeMismatch::OutputDigest),
        ("wrong-metric", NativeMismatch::Metric),
        ("wrong-vector-values", NativeMismatch::VectorValues),
        (
            "wrong-projection-receipt",
            NativeMismatch::ProjectionReceipt,
        ),
        (
            "same-profile-foreign-projection",
            NativeMismatch::SameProfileProjection,
        ),
        ("extra-effect", NativeMismatch::ExtraEffect),
        ("missing-effect", NativeMismatch::MissingEffect),
    ] {
        let mut authority = fixture.authority();
        let plan = fixture.plan(name, name, None);
        let (batch, receipt) = fixture.batch_and_receipt_with_mismatch(&plan, 1.0, Some(mismatch));
        fixture.begin_and_append(&mut authority, &plan, &receipt, name);
        assert_eq!(
            fixture.apply(&mut authority, &receipt, batch, name),
            Err(GraphDbError::Conflict),
            "{name} must fail closed"
        );
        // The projection admits one pending stage at a time; release the
        // rejected attempt so the next mismatch case begins cleanly.
        let cancelled = with_context(&format!("{name}.cancel"), |context| {
            fixture.graph.registry.cancel_generation_stage(
                fixture.registration(),
                &mut authority,
                context,
                &plan.key,
            )
        })
        .unwrap();
        assert!(matches!(
            cancelled,
            SemanticVectorStageCancelOutcome::Cancelled(_)
        ));
    }
}

#[test]
fn paged_synthetic_corpus_publishes_after_eval_sized_admission() {
    // Dim-3 stand-in for paged admission. A 43-page (~21700) run applies, then
    // prepare_publication hits the 30s registration deadline while hashing the
    // recovered generation — keep this short and cover production width below.
    const PAGES: u64 = 4;
    let page = u64::try_from(MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH).unwrap();
    let chunk_count = page.checked_mul(PAGES).unwrap();
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let plan = fixture.plan_with_chunk_count("paged-corpus", "paged-corpus", chunk_count);
    let mut expected = plan.initial_checkpoint_digest.clone();
    for ordinal in 0..PAGES {
        let start = ordinal.checked_mul(page).unwrap();
        let next = unique_page_checkpoint(ordinal);
        let (batch, receipt) = fixture.page_batch_and_receipt(
            &plan,
            "paged-corpus",
            ordinal,
            start,
            page,
            expected.clone(),
            next.clone(),
            ordinal as f32,
        );
        if ordinal == 0 {
            fixture.begin_and_append(&mut authority, &plan, &receipt, "paged-corpus");
        } else {
            fixture.append(&mut authority, &plan, &receipt, "paged-corpus");
        }
        fixture
            .apply(
                &mut authority,
                &receipt,
                batch,
                &format!("paged-corpus.apply.{ordinal}"),
            )
            .unwrap_or_else(|error| {
                panic!("paged synthetic apply exhausted a named graph budget: {error}")
            });
        fixture.settle_batch(
            &mut authority,
            &receipt,
            &format!("paged-corpus.settle.{ordinal}"),
        );
        expected = next;
    }
    fixture
        .try_ready(&mut authority, &plan, "paged-corpus.ready")
        .unwrap_or_else(|error| {
            panic!("paged synthetic prepare exhausted a named graph budget: {error}")
        });
    let committed = fixture.publish(&mut authority, &plan, "paged-corpus.publish");
    settle_publication(
        &mut authority,
        &plan,
        &committed,
        "paged-corpus.publication-settle",
    );
}

/// TEMPORARY scaling probe (rc-semantic-timing lane): isolates the cost of
/// hashing the recovered generation. `prepare_publication_from_staged_native`
/// builds an EMPTY manifest and an EMPTY write batch, so the only work inside
/// it that scales with corpus size is
/// `recovered_generation_digest_from_database`, which walks every entity and
/// relation of the projection, serde_json-encodes each, and SHA256s the frames.
/// `prepare_publication_ms` below is therefore the recovered-generation hashing
/// cost plus O(1). Runs to real production width (43 pages / ~21.7k chunks at
/// 768-d) so the `EVALUATION_GRAPH_OPERATION_DEADLINE` ceiling can be sized
/// from a measurement instead of an extrapolation.
#[test]
#[ignore = "diagnostic probe, run explicitly"]
fn recovered_generation_digest_cost_scaling_probe() {
    let page = u64::try_from(MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH).unwrap();
    for pages in [4_u64, 8, 16, 43] {
        let chunk_count = page.checked_mul(pages).unwrap();
        let fixture = ContractFixture::new_with_embedding_dimensions(768);
        let mut authority = fixture.authority();
        let plan = fixture.plan_with_chunk_count("digest", "digest", chunk_count);
        let mut expected = plan.initial_checkpoint_digest.clone();
        let corpus_started = std::time::Instant::now();
        for ordinal in 0..pages {
            let start = ordinal.checked_mul(page).unwrap();
            let next = unique_page_checkpoint(ordinal);
            let (batch, receipt) = fixture.page_batch_and_receipt(
                &plan,
                "digest",
                ordinal,
                start,
                page,
                expected.clone(),
                next.clone(),
                ordinal as f32,
            );
            if ordinal == 0 {
                fixture.begin_and_append(&mut authority, &plan, &receipt, "digest");
            } else {
                fixture.append(&mut authority, &plan, &receipt, "digest");
            }
            fixture
                .apply(
                    &mut authority,
                    &receipt,
                    batch,
                    &format!("digest.apply.{ordinal}"),
                )
                .expect("digest probe apply");
            fixture.settle_batch(
                &mut authority,
                &receipt,
                &format!("digest.settle.{ordinal}"),
            );
            expected = next;
        }
        let corpus_ms = corpus_started.elapsed().as_millis();
        let ready_started = std::time::Instant::now();
        fixture
            .try_ready(&mut authority, &plan, "digest.ready")
            .expect("digest probe ready");
        let ready_ms = ready_started.elapsed().as_millis();
        let publish_started = std::time::Instant::now();
        let committed = fixture.publish(&mut authority, &plan, "digest.publish");
        settle_publication(
            &mut authority,
            &plan,
            &committed,
            "digest.publication-settle",
        );
        let publish_ms = publish_started.elapsed().as_millis();
        let case_ms = ready_ms.saturating_add(publish_ms);
        let per_chunk_us = (case_ms as f64) * 1000.0 / (chunk_count as f64);
        eprintln!(
            "[digest-probe] dims=768 pages={pages} chunks={chunk_count} \
             corpus_build_ms={corpus_ms} \
             prepare_publication_ms={ready_ms} publish_settle_ms={publish_ms} \
             case_total_ms={case_ms} per_chunk_us={per_chunk_us:.1} \
             pass_total_ms={pass_total}",
            pass_total = corpus_ms.saturating_add(case_ms)
        );
    }
}

/// TEMPORARY scaling probe (rc-semantic-timing lane): if per-page settle cost
/// is flat, total time is linear in pages; if each page rehashes the recovered
/// generation, total time grows with pages^2. Prints per-scale timings.
#[test]
#[ignore = "diagnostic probe, run explicitly"]
fn paged_corpus_settle_cost_scaling_probe() {
    let page = u64::try_from(MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH).unwrap();
    for (dims, pages) in [(3_u32, 4_u64), (3, 8), (768, 4), (768, 8)] {
        let chunk_count = page.checked_mul(pages).unwrap();
        let fixture = ContractFixture::new_with_embedding_dimensions(dims);
        let mut authority = fixture.authority();
        let plan = fixture.plan_with_chunk_count("probe", "probe", chunk_count);
        let mut expected = plan.initial_checkpoint_digest.clone();
        let started = std::time::Instant::now();
        let mut last_page_ms = 0_u128;
        for ordinal in 0..pages {
            let page_started = std::time::Instant::now();
            let start = ordinal.checked_mul(page).unwrap();
            let next = unique_page_checkpoint(ordinal);
            let (batch, receipt) = fixture.page_batch_and_receipt(
                &plan,
                "probe",
                ordinal,
                start,
                page,
                expected.clone(),
                next.clone(),
                ordinal as f32,
            );
            if ordinal == 0 {
                fixture.begin_and_append(&mut authority, &plan, &receipt, "probe");
            } else {
                fixture.append(&mut authority, &plan, &receipt, "probe");
            }
            fixture
                .apply(
                    &mut authority,
                    &receipt,
                    batch,
                    &format!("probe.apply.{ordinal}"),
                )
                .expect("probe apply");
            fixture.settle_batch(&mut authority, &receipt, &format!("probe.settle.{ordinal}"));
            expected = next;
            last_page_ms = page_started.elapsed().as_millis();
        }
        let commit_ms = started.elapsed().as_millis();
        let ready_started = std::time::Instant::now();
        fixture
            .try_ready(&mut authority, &plan, "probe.ready")
            .expect("probe ready");
        let ready_ms = ready_started.elapsed().as_millis();
        eprintln!(
            "[probe] dims={dims} pages={pages} chunks={chunk_count} commit_all_ms={commit_ms} last_page_ms={last_page_ms} prepare_publication_ms={ready_ms}"
        );
    }
}

#[test]
fn production_width_page_applies_and_publishes_through_named_budgets() {
    let page = u64::try_from(MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH).unwrap();
    let fixture = ContractFixture::new_with_embedding_dimensions(768);
    let mut authority = fixture.authority();
    let plan = fixture.plan_with_chunk_count("prod-width", "prod-width", page);
    let next = unique_page_checkpoint(0);
    let (batch, receipt) = fixture.page_batch_and_receipt(
        &plan,
        "prod-width",
        0,
        0,
        page,
        plan.initial_checkpoint_digest.clone(),
        next,
        0.125,
    );
    fixture.begin_and_append(&mut authority, &plan, &receipt, "prod-width");
    fixture
        .apply(&mut authority, &receipt, batch, "prod-width.apply")
        .unwrap_or_else(|error| {
            panic!("production-width apply exhausted a named graph budget: {error}")
        });
    fixture.settle_batch(&mut authority, &receipt, "prod-width.settle");
    fixture
        .try_ready(&mut authority, &plan, "prod-width.ready")
        .unwrap_or_else(|error| {
            panic!("production-width prepare exhausted a named graph budget: {error}")
        });
    let committed = fixture.publish(&mut authority, &plan, "prod-width.publish");
    settle_publication(
        &mut authority,
        &plan,
        &committed,
        "prod-width.publication-settle",
    );
}

fn unique_page_checkpoint(ordinal: u64) -> SemanticVectorCheckpointDigest {
    SemanticVectorCheckpointDigest::try_from(format!("sha256:{:064x}", 20_000 + ordinal)).unwrap()
}

#[test]
fn native_batch_admission_accepts_page_source_manifest() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let plan = fixture.plan("page-manifest", "page-manifest", None);
    let (batch, receipt) = fixture.batch_and_receipt_with_mismatch(
        &plan,
        1.0,
        Some(NativeMismatch::PageSourceManifest),
    );
    fixture.begin_and_append(&mut authority, &plan, &receipt, "page-manifest");
    fixture
        .apply(&mut authority, &receipt, batch, "page-manifest")
        .expect("page change-set digest is not the corpus watermark");
}

#[test]
fn receipt_then_cancel_fences_a_delayed_native_apply() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let plan = fixture.plan("receipt-cancel", "semantic-retry", None);
    let (batch, receipt) = fixture.batch_and_receipt(&plan, 1.0);
    fixture.begin_and_append(&mut authority, &plan, &receipt, "receipt-cancel");

    let cancelled = with_context("receipt-cancel.cancel", |context| {
        fixture.graph.registry.cancel_generation_stage(
            fixture.registration(),
            &mut authority,
            context,
            &plan.key,
        )
    })
    .unwrap();
    assert!(matches!(
        cancelled,
        SemanticVectorStageCancelOutcome::Cancelled(ref record)
            if record.state == SemanticVectorStageState::Cancelled
    ));
    assert_eq!(
        fixture.apply(
            &mut authority,
            &receipt,
            batch,
            "receipt-cancel.delayed-apply"
        ),
        Err(GraphDbError::Conflict)
    );
}

#[test]
fn native_apply_then_cancel_removes_before_settlement() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let plan = fixture.plan("apply-cancel", "semantic-apply-cancel", None);
    let (batch, receipt) = fixture.batch_and_receipt(&plan, 2.0);
    fixture.begin_and_append(&mut authority, &plan, &receipt, "apply-cancel");
    fixture
        .apply(&mut authority, &receipt, batch, "apply-cancel.native")
        .unwrap();

    with_context("apply-cancel.cancel", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .cancel_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &plan.key,
                )
                .unwrap(),
            SemanticVectorStageCancelOutcome::Cancelled(_)
        ));
    });
    let settlement = with_context("apply-cancel.delayed-settlement", |context| {
        fixture.graph.registry.settle_verified_generation_batch(
            fixture.registration(),
            &mut authority,
            context,
            &receipt.key,
            &receipt.receipt_digest,
        )
    });
    assert_eq!(settlement, Err(GraphDbError::Conflict));
    with_context("apply-cancel.resume", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .resume_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &plan.key,
                )
                .unwrap(),
            SemanticVectorStageResumeOutcome::Cancelled(_)
        ));
    });
}

#[test]
fn caller_cancel_after_native_apply_leaves_batch_settlement_replayable() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let plan = fixture.plan("apply-caller-cancel", "semantic-apply-caller-cancel", None);
    let (batch, receipt) = fixture.batch_and_receipt(&plan, 2.5);
    fixture.begin_and_append(&mut authority, &plan, &receipt, "apply-caller-cancel");
    fixture
        .apply(
            &mut authority,
            &receipt,
            batch,
            "apply-caller-cancel.native",
        )
        .unwrap();

    let cancellation = Arc::new(SwitchCancellation::default());
    cancellation.0.store(true, Ordering::SeqCst);
    let cancelled_registration = registration_with_cancellation(
        fixture.registration(),
        cancellation as Arc<dyn GraphCancellation>,
    );
    let interrupted = with_context("apply-caller-cancel.settle", |context| {
        fixture.graph.registry.settle_verified_generation_batch(
            cancelled_registration,
            &mut authority,
            context,
            &receipt.key,
            &receipt.receipt_digest,
        )
    });
    assert_eq!(interrupted, Err(GraphDbError::Cancelled));

    with_context("apply-caller-cancel.resume", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .resume_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &plan.key,
                )
                .unwrap(),
            SemanticVectorStageResumeOutcome::Pending(_)
        ));
    });
}

#[test]
fn published_native_commit_stays_replayable_until_stage_settlement() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let plan = fixture.plan("publish-unsettled", "semantic-publish-unsettled", None);
    let (batch, receipt) = fixture.batch_and_receipt(&plan, 2.75);
    fixture.begin_and_append(&mut authority, &plan, &receipt, "publish-unsettled");
    fixture
        .apply(&mut authority, &receipt, batch, "publish-unsettled.native")
        .unwrap();
    fixture.settle_batch(&mut authority, &receipt, "publish-unsettled.batch-settle");
    fixture.ready(&mut authority, &plan, "publish-unsettled.ready");
    let committed = fixture.publish(&mut authority, &plan, "publish-unsettled.commit");

    with_context("publish-unsettled.resume", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .resume_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &plan.key,
                )
                .unwrap(),
            SemanticVectorStageResumeOutcome::Ready(_)
        ));
    });
    let replay = fixture.publish(&mut authority, &plan, "publish-unsettled.replay");
    assert_eq!(replay.head, committed.head);
    settle_publication(&mut authority, &plan, &replay, "publish-unsettled.settle");
}

#[test]
fn cancelled_response_loss_retry_reconciles_native_cleanup() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let plan = fixture.plan("cancel-retry", "semantic-cancel-retry", None);
    let (batch, receipt) = fixture.batch_and_receipt(&plan, 3.0);
    fixture.begin_and_append(&mut authority, &plan, &receipt, "cancel-retry");
    fixture
        .apply(
            &mut authority,
            &receipt,
            batch.clone(),
            "cancel-retry.native",
        )
        .unwrap();

    with_context("cancel-retry.lost-response", |context| {
        assert!(matches!(
            authority
                .cancel_stage(&plan.key, &plan.writer_fence, context)
                .unwrap(),
            SemanticVectorStageCancelOutcome::Cancelled(_)
        ));
    });
    with_context("cancel-retry.reconcile", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .cancel_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &plan.key,
                )
                .unwrap(),
            SemanticVectorStageCancelOutcome::ExactReplay(_)
        ));
    });
    assert_eq!(
        fixture.apply(
            &mut authority,
            &receipt,
            batch,
            "cancel-retry.delayed-apply"
        ),
        Err(GraphDbError::Conflict)
    );
}

#[test]
fn retention_cleanup_failure_keeps_cancel_fence_until_retry() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let plan = fixture.plan(
        "retention-cancel-retry",
        "semantic-retention-cancel-retry",
        None,
    );
    let (batch, receipt) = fixture.batch_and_receipt(&plan, 4.0);
    fixture.begin_and_append(&mut authority, &plan, &receipt, "retention-cancel-retry");

    let pause = Arc::new(ApplyPause::default());
    let apply_registration = registration_with_cancellation(
        fixture.registration(),
        Arc::clone(&pause) as Arc<dyn GraphCancellation>,
    );
    let mut delayed_authority = authority.clone();
    let fixture_ref = &fixture;
    let receipt_key = receipt.key.clone();
    let receipt_digest = receipt.receipt_digest.clone();
    std::thread::scope(|scope| {
        let delayed_apply = scope.spawn(move || {
            with_context("retention-cancel-retry.delayed-apply", |context| {
                fixture_ref
                    .graph
                    .registry
                    .apply_verified_generation_batch(
                        apply_registration,
                        &mut delayed_authority,
                        context,
                        &receipt_key,
                        &receipt_digest,
                        batch,
                    )
                    .map(|_| ())
            })
        });
        pause.wait_until_reached();

        with_context("retention-cancel-retry.cancel", |context| {
            assert!(matches!(
                authority
                    .cancel_stage(&plan.key, &plan.writer_fence, context)
                    .unwrap(),
                SemanticVectorStageCancelOutcome::Cancelled(_)
            ));
        });
        let failed_cleanup = Arc::new(CancelOnSecondCheck::default());
        let failed_registration = registration_with_cancellation(
            fixture.registration(),
            failed_cleanup as Arc<dyn GraphCancellation>,
        );
        let reservation = with_context("retention-cancel-retry.reserve", |context| {
            fixture
                .graph
                .registry
                .reserve_one_semantic_vector_generation(
                    fixture.registration(),
                    &mut authority,
                    context,
                    None,
                    &plan.writer_fence,
                )
        })
        .unwrap();
        let failure = with_context("retention-cancel-retry.cleanup-failure", |context| {
            finalize_retention_step(
                &fixture.graph.registry,
                failed_registration,
                &mut authority,
                context,
                &plan.writer_fence,
                reservation,
            )
        });
        assert_eq!(failure, Err(GraphDbError::Cancelled));

        pause.release();
        assert_eq!(
            delayed_apply.join().expect("delayed apply thread"),
            Err(GraphDbError::Conflict)
        );
        assert_eq!(
            pause.calls.load(Ordering::SeqCst),
            2,
            "the retained fence rejects the delayed apply before native mutation checks"
        );
    });

    let retry = with_context("retention-cancel-retry.cleanup-retry", |context| {
        let step = fixture
            .graph
            .registry
            .reserve_one_semantic_vector_generation(
                fixture.registration(),
                &mut authority,
                context,
                None,
                &plan.writer_fence,
            )?;
        finalize_retention_step(
            &fixture.graph.registry,
            fixture.registration(),
            &mut authority,
            context,
            &plan.writer_fence,
            step,
        )
    })
    .unwrap();
    assert!(matches!(
        retry.action,
        SemanticVectorRetentionAction::CancelledRemoved(ref generation)
            if generation == &plan.semantic_generation_id
    ));
    with_context("retention-cancel-retry.stage-removed", |context| {
        assert!(authority.stage(&plan.key, context).unwrap().is_none());
    });
}

#[test]
fn ready_and_published_cancel_preserve_heads_and_historical_snapshot_bytes() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let first = fixture.plan("preserve-first", "semantic-preserve-first", None);
    let (first_batch, first_receipt) = fixture.batch_and_receipt(&first, 10.0);
    fixture.begin_and_append(&mut authority, &first, &first_receipt, "preserve-first");
    fixture
        .apply(
            &mut authority,
            &first_receipt,
            first_batch,
            "preserve-first.native",
        )
        .unwrap();
    fixture.settle_batch(&mut authority, &first_receipt, "preserve-first.settle");
    fixture.ready(&mut authority, &first, "preserve-first.ready");
    with_context("preserve-first.cancel-ready", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .cancel_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &first.key,
                )
                .unwrap(),
            SemanticVectorStageCancelOutcome::ReadyToPublish(_)
        ));
        assert!(matches!(
            authority.replay(&first.publication_key, context).unwrap(),
            GraphPublicationReplayLookupV1::Active(_)
        ));
    });
    let first_commit = fixture.publish(&mut authority, &first, "preserve-first.publish");
    settle_publication(
        &mut authority,
        &first,
        &first_commit,
        "preserve-first.publish-settle",
    );
    let reference = fixture.semantic_entity_reference(&first);
    let first_entity = first_commit
        .snapshot
        .entity(&reference, Arc::new(TestCancellation))
        .unwrap();
    assert!(first_entity.is_some());
    let first_entity_bytes = serde_json::to_vec(&first_entity).unwrap();
    with_context("preserve-first.cancel-published", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .cancel_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &first.key,
                )
                .unwrap(),
            SemanticVectorStageCancelOutcome::ReadyToPublish(_)
        ));
        assert_eq!(
            authority
                .verified_head(&first.key.projection, context)
                .unwrap(),
            Some(first_commit.head.clone())
        );
        assert!(matches!(
            fixture
                .graph
                .registry
                .published_semantic_generation(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &SemanticVectorPublishedGenerationKey {
                        projection: first.key.projection.clone(),
                        semantic_generation_id: first.semantic_generation_id.clone(),
                    },
                )
                .unwrap(),
            SemanticVectorPublishedGenerationLookup::Published { record, .. }
                if record.plan == first
        ));
    });

    let second = fixture.plan(
        "preserve-second",
        "semantic-preserve-second",
        Some(first_commit.head.clone()),
    );
    let (second_batch, second_receipt) = fixture.batch_and_receipt(&second, 20.0);
    fixture.begin_and_append(&mut authority, &second, &second_receipt, "preserve-second");
    fixture
        .apply(
            &mut authority,
            &second_receipt,
            second_batch,
            "preserve-second.native",
        )
        .unwrap();
    fixture.settle_batch(&mut authority, &second_receipt, "preserve-second.settle");
    fixture.ready(&mut authority, &second, "preserve-second.ready");
    with_context("preserve-second.cancel-ready", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .cancel_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &second.key,
                )
                .unwrap(),
            SemanticVectorStageCancelOutcome::ReadyToPublish(_)
        ));
        assert!(matches!(
            authority.replay(&second.publication_key, context).unwrap(),
            GraphPublicationReplayLookupV1::Active(_)
        ));
        assert_eq!(
            authority
                .verified_head(&second.key.projection, context)
                .unwrap(),
            Some(first_commit.head.clone())
        );
    });
    let second_commit = fixture.publish(&mut authority, &second, "preserve-second.publish");
    settle_publication(
        &mut authority,
        &second,
        &second_commit,
        "preserve-second.publish-settle",
    );
    assert_ne!(
        second_commit
            .snapshot
            .entity(&reference, Arc::new(TestCancellation))
            .unwrap(),
        first_entity
    );
    let second_reference = fixture.semantic_entity_reference(&second);
    let second_entity_bytes = serde_json::to_vec(
        &second_commit
            .snapshot
            .entity(&second_reference, Arc::new(TestCancellation))
            .unwrap(),
    )
    .unwrap();
    with_context("preserve-second.cancel-published", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .cancel_generation_stage(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &second.key,
                )
                .unwrap(),
            SemanticVectorStageCancelOutcome::ReadyToPublish(_)
        ));
        assert_eq!(
            authority
                .verified_head(&second.key.projection, context)
                .unwrap(),
            Some(second_commit.head.clone())
        );
    });

    let first_head = first_commit.head.clone();
    let second_head = second_commit.head.clone();
    drop(first_commit);
    drop(second_commit);
    assert!(fixture.graph.close().unwrap());
    fixture.graph.mount().unwrap();

    let mut restarted_authority = fixture.authority();
    with_context("preserve-first.lookup-after-restart", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .published_semantic_generation(
                    fixture.registration(),
                    &mut restarted_authority,
                    context,
                    &SemanticVectorPublishedGenerationKey {
                        projection: first.key.projection.clone(),
                        semantic_generation_id: first.semantic_generation_id.clone(),
                    },
                )
                .unwrap(),
            SemanticVectorPublishedGenerationLookup::Published {
                record,
                verified_head,
            } if record.plan == first && *verified_head == first_head
        ));
    });
    let historical = with_context("preserve-first.historical", |context| {
        fixture.graph.registry.verified_generation_snapshot(
            fixture.registration(),
            &mut restarted_authority,
            context,
            &first.publication_key,
        )
    })
    .unwrap();
    let historical_entity = historical
        .entity(&reference, Arc::new(TestCancellation))
        .unwrap();
    let recovered_second = with_context("preserve-second.historical-after-reopen", |context| {
        fixture.graph.registry.verified_generation_snapshot(
            fixture.registration(),
            &mut restarted_authority,
            context,
            &second.publication_key,
        )
    })
    .unwrap();
    let recovered_second_entity = recovered_second
        .entity(&second_reference, Arc::new(TestCancellation))
        .unwrap();
    assert_eq!(
        serde_json::to_vec(&historical_entity).unwrap(),
        first_entity_bytes
    );
    assert_eq!(
        serde_json::to_vec(&recovered_second_entity).unwrap(),
        second_entity_bytes
    );
    with_context("preserve-head-after-reopen", |context| {
        assert_eq!(
            restarted_authority
                .verified_head(&second.key.projection, context)
                .unwrap(),
            Some(second_head)
        );
    });
}

#[test]
fn exact_verified_generation_lease_blocks_retirement_until_activation_releases_it() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let first = fixture.plan("lease-first", "semantic-lease-first", None);
    let (first_batch, first_receipt) = fixture.batch_and_receipt(&first, 30.0);
    fixture.begin_and_append(&mut authority, &first, &first_receipt, "lease-first");
    fixture
        .apply(
            &mut authority,
            &first_receipt,
            first_batch,
            "lease-first.native",
        )
        .unwrap();
    fixture.settle_batch(&mut authority, &first_receipt, "lease-first.settle");
    fixture.ready(&mut authority, &first, "lease-first.ready");
    let first_commit = fixture.publish(&mut authority, &first, "lease-first.publish");
    settle_publication(
        &mut authority,
        &first,
        &first_commit,
        "lease-first.publish-settle",
    );
    // This is the exact VerifiedGraphSnapshot held by the production
    // GraphVectorGenerationStore activation lease through configuration CAS.
    let activation_lease = first_commit.snapshot.clone();
    let first_head = first_commit.head.clone();
    drop(first_commit);

    let second = fixture.plan("lease-second", "semantic-lease-second", Some(first_head));
    let (second_batch, second_receipt) = fixture.batch_and_receipt(&second, 40.0);
    fixture.begin_and_append(&mut authority, &second, &second_receipt, "lease-second");
    fixture
        .apply(
            &mut authority,
            &second_receipt,
            second_batch,
            "lease-second.native",
        )
        .unwrap();
    fixture.settle_batch(&mut authority, &second_receipt, "lease-second.settle");
    fixture.ready(&mut authority, &second, "lease-second.ready");
    let second_commit = fixture.publish(&mut authority, &second, "lease-second.publish");
    settle_publication(
        &mut authority,
        &second,
        &second_commit,
        "lease-second.publish-settle",
    );
    let newer_head = with_context("lease-retention.newer-head", |context| {
        let step = fixture
            .graph
            .registry
            .reserve_one_semantic_vector_generation(
                fixture.registration(),
                &mut authority,
                context,
                None,
                &first.writer_fence,
            )?;
        finalize_retention_step(
            &fixture.graph.registry,
            fixture.registration(),
            &mut authority,
            context,
            &first.writer_fence,
            step,
        )
    })
    .unwrap();
    assert_eq!(
        newer_head.action,
        SemanticVectorRetentionAction::Retained(first.semantic_generation_id.clone())
    );
    with_context("lease-retention.newer-head-still-published", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .published_semantic_generation(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &SemanticVectorPublishedGenerationKey {
                        projection: second.key.projection.clone(),
                        semantic_generation_id: second.semantic_generation_id.clone(),
                    },
                )
                .unwrap(),
            SemanticVectorPublishedGenerationLookup::Published { .. }
        ));
    });
    let retained = with_context("lease-retention.blocked", |context| {
        let step = fixture
            .graph
            .registry
            .reserve_one_semantic_vector_generation(
                fixture.registration(),
                &mut authority,
                context,
                None,
                &first.writer_fence,
            )?;
        finalize_retention_step(
            &fixture.graph.registry,
            fixture.registration(),
            &mut authority,
            context,
            &first.writer_fence,
            step,
        )
    })
    .unwrap();
    assert_eq!(
        retained.action,
        SemanticVectorRetentionAction::Retained(first.semantic_generation_id.clone())
    );
    with_context("lease-retention.still-published", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .published_semantic_generation(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &SemanticVectorPublishedGenerationKey {
                        projection: first.key.projection.clone(),
                        semantic_generation_id: first.semantic_generation_id.clone(),
                    },
                )
                .unwrap(),
            SemanticVectorPublishedGenerationLookup::Published { .. }
        ));
    });

    drop(activation_lease);
    let retired = with_context("lease-retention.released", |context| {
        let step = fixture
            .graph
            .registry
            .reserve_one_semantic_vector_generation(
                fixture.registration(),
                &mut authority,
                context,
                None,
                &first.writer_fence,
            )?;
        finalize_retention_step(
            &fixture.graph.registry,
            fixture.registration(),
            &mut authority,
            context,
            &first.writer_fence,
            step,
        )
    })
    .unwrap();
    assert_eq!(
        retired.action,
        SemanticVectorRetentionAction::Retired(first.semantic_generation_id.clone())
    );
    with_context("lease-retention.removed", |context| {
        assert!(matches!(
            fixture
                .graph
                .registry
                .published_semantic_generation(
                    fixture.registration(),
                    &mut authority,
                    context,
                    &SemanticVectorPublishedGenerationKey {
                        projection: first.key.projection.clone(),
                        semantic_generation_id: first.semantic_generation_id.clone(),
                    },
                )
                .unwrap(),
            SemanticVectorPublishedGenerationLookup::Missing
        ));
        assert!(matches!(
            authority.replay(&first.publication_key, context).unwrap(),
            GraphPublicationReplayLookupV1::Retired(_)
        ));
    });
    // Simulate a lost retirement response and authority reopen. The relational
    // tombstone journal deterministically converges native cleanup, then removes
    // its own journal on the exact replay.
    drop(authority);
    let mut restarted_authority = fixture.authority();
    for suffix in ["finalize-native", "finalize-journal"] {
        let finalized = with_context(&format!("lease-retention.{suffix}"), |context| {
            let step = fixture
                .graph
                .registry
                .reserve_one_semantic_vector_generation(
                    fixture.registration(),
                    &mut restarted_authority,
                    context,
                    None,
                    &first.writer_fence,
                )?;
            finalize_retention_step(
                &fixture.graph.registry,
                fixture.registration(),
                &mut restarted_authority,
                context,
                &first.writer_fence,
                step,
            )
        })
        .unwrap();
        assert_eq!(
            finalized.action,
            SemanticVectorRetentionAction::Finalized(first.semantic_generation_id.clone())
        );
    }
    with_context("lease-retention.cleanup-complete", |context| {
        assert!(
            restarted_authority
                .pending_retirement_cleanup(&first.key.projection.shard_id, context)
                .unwrap()
                .is_none()
        );
    });
    drop(second_commit);
}

#[test]
fn dropped_reservation_releases_its_fence_after_caller_cancellation() {
    let fixture = ContractFixture::new();
    let mut authority = fixture.authority();
    let first = fixture.plan("reservation-first", "semantic-reservation-first", None);
    let (first_batch, first_receipt) = fixture.batch_and_receipt(&first, 50.0);
    fixture.begin_and_append(&mut authority, &first, &first_receipt, "reservation-first");
    fixture
        .apply(
            &mut authority,
            &first_receipt,
            first_batch,
            "reservation-first.native",
        )
        .unwrap();
    fixture.settle_batch(&mut authority, &first_receipt, "reservation-first.settle");
    fixture.ready(&mut authority, &first, "reservation-first.ready");
    let first_commit = fixture.publish(&mut authority, &first, "reservation-first.publish");
    settle_publication(
        &mut authority,
        &first,
        &first_commit,
        "reservation-first.publish-settle",
    );
    let first_head = first_commit.head.clone();
    drop(first_commit);

    let second = fixture.plan(
        "reservation-second",
        "semantic-reservation-second",
        Some(first_head),
    );
    let (second_batch, second_receipt) = fixture.batch_and_receipt(&second, 60.0);
    fixture.begin_and_append(
        &mut authority,
        &second,
        &second_receipt,
        "reservation-second",
    );
    fixture
        .apply(
            &mut authority,
            &second_receipt,
            second_batch,
            "reservation-second.native",
        )
        .unwrap();
    fixture.settle_batch(&mut authority, &second_receipt, "reservation-second.settle");
    fixture.ready(&mut authority, &second, "reservation-second.ready");
    let second_commit = fixture.publish(&mut authority, &second, "reservation-second.publish");
    settle_publication(
        &mut authority,
        &second,
        &second_commit,
        "reservation-second.publish-settle",
    );

    let cancellation = Arc::new(SwitchCancellation::default());
    let cancelled_registration = registration_with_cancellation(
        fixture.registration(),
        Arc::clone(&cancellation) as Arc<dyn GraphCancellation>,
    );
    let reservation = with_context("reservation-drop.reserve", |context| {
        fixture
            .graph
            .registry
            .reserve_one_semantic_vector_generation(
                cancelled_registration,
                &mut authority,
                context,
                None,
                &first.writer_fence,
            )
    })
    .unwrap();
    let tracedecay_graph_db::SemanticVectorRetentionStep::Reserved {
        census,
        reservation,
    } = reservation
    else {
        panic!("first published generation must be reserved");
    };
    assert_eq!(reservation.census_revision(), census.revision);

    cancellation.0.store(true, Ordering::SeqCst);
    drop(reservation);

    let historical = with_context("reservation-drop.readable", |context| {
        fixture.graph.registry.verified_generation_snapshot(
            fixture.registration(),
            &mut authority,
            context,
            &first.publication_key,
        )
    })
    .expect("dropping a reservation must preserve prior-generation readability");
    drop(historical);

    let retry = with_context("reservation-drop.retry", |context| {
        fixture
            .graph
            .registry
            .reserve_one_semantic_vector_generation(
                fixture.registration(),
                &mut authority,
                context,
                None,
                &first.writer_fence,
            )
    })
    .unwrap();
    let tracedecay_graph_db::SemanticVectorRetentionStep::Reserved { reservation, .. } = retry
    else {
        panic!("released reservation must be retryable");
    };
    fixture
        .graph
        .registry
        .release_semantic_vector_retirement(*reservation)
        .expect("explicit release must not consult a cancelled request");
    drop(second_commit);
}

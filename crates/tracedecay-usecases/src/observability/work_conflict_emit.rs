//! Work-conflict predictions and linked outcomes from the mounted
//! native-integration owner.
//!
//! The preflight tree merge is the mechanical conflict oracle; its
//! disposition is recorded as an uncalibrated rule prediction bound to the
//! exact preview identity, and the terminal apply receipt is the independent
//! native-git adjudication linking back through the same deterministic
//! `prediction_ref`. Telemetry failure never changes the owner's result.

use tracedecay_application::{
    NativeIntegrationPreviewProjectionV1, NativeIntegrationReceiptProjectionV1,
    NativeIntegrationSurfaceResultV1,
};
use tracedecay_domain::{
    ConflictAdjudicatorV1, ConflictKindV1, ConflictOutcomeV1, ConflictPredictionV1,
    ConflictScoreKindV1, CoverageStateV1, ManifestDigest, NativeIntegrationPreviewDispositionV1,
    NativeIntegrationPreviewId, NativeIntegrationPreviewV1, NativeIntegrationTerminalOutcomeV1,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1,
    WorkConflictOutcomeLinkedV1, WorkConflictPredictionObservedV1, canonical_sha256,
};

use super::{
    BoundedObservabilityProducerV1, ExecutionOwnerFactInputV1, ObservabilityEmissionOutcomeV1,
    ObservabilityProducerIdentityV1, execution_owner_fact_envelope,
};

const PREDICTION_EVENT_KIND: &str = "work.conflict_prediction.observed.v1";
const OUTCOME_EVENT_KIND: &str = "work.conflict_outcome.linked.v1";
const PREDICTION_OPERATION: &str = "preflight_native_integration";
const OUTCOME_OPERATION: &str = "apply_native_integration";
/// The preflight tree merge is a deterministic rule oracle, not a calibrated
/// probability model; both revision strings say so rather than claiming a
/// calibration that does not exist.
const CONFLICT_DESCRIPTOR_REVISION: &str = "native-integration-preflight-tree-merge.rule.v1";
const CONFLICT_CALIBRATION_REVISION: &str = "uncalibrated.rule-oracle.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkConflictObservationUnavailableV1 {
    OwnerUnmounted,
    ProducerUnmounted,
    ProducerAdmissionUnavailable,
    /// The owner result does not adjudicate one mechanical merge relation:
    /// reads, refusals, approvals, worktree operations, already-integrated
    /// previews, and preflights whose tree merge never completed.
    NotAdjudicated,
    OwnerEvidenceInvalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkConflictObservationResultV1 {
    Enqueued {
        event_kind: &'static str,
    },
    DroppedAtCapacity {
        event_kind: &'static str,
    },
    Unavailable {
        reason: WorkConflictObservationUnavailableV1,
    },
}

/// Offers the work-conflict fact proved by one native-integration owner
/// result to the long-lived producer.
///
/// A preflight preview that completed its tree merge proves one mechanical
/// conflict prediction; a terminal apply receipt proves one independently
/// observed outcome linked to that prediction. Queue pressure is the
/// producer's typed drop state and never changes the owner's decided result.
pub fn record_work_conflict_observation(
    scope_ref: &str,
    producer: Option<&BoundedObservabilityProducerV1>,
    surface_operation: &str,
    owner_mounted: bool,
    result: &NativeIntegrationSurfaceResultV1,
    owner_preview: Option<&NativeIntegrationPreviewV1>,
) -> WorkConflictObservationResultV1 {
    if !owner_mounted {
        return unavailable(WorkConflictObservationUnavailableV1::OwnerUnmounted);
    }
    let Some(producer) = producer else {
        return unavailable(WorkConflictObservationUnavailableV1::ProducerUnmounted);
    };
    let identity = producer.identity();
    if identity.authorized_scope_ref != scope_ref {
        return unavailable(WorkConflictObservationUnavailableV1::OwnerEvidenceInvalid);
    }
    let (envelope, event_kind) = match work_conflict_envelope(
        identity,
        scope_ref,
        surface_operation,
        result,
        owner_preview,
    ) {
        Ok(Some(built)) => built,
        Ok(None) => {
            return unavailable(WorkConflictObservationUnavailableV1::NotAdjudicated);
        }
        Err(_) => {
            return unavailable(WorkConflictObservationUnavailableV1::OwnerEvidenceInvalid);
        }
    };
    match producer.try_emit_owner_fact(envelope) {
        Ok(ObservabilityEmissionOutcomeV1::Enqueued) => {
            WorkConflictObservationResultV1::Enqueued { event_kind }
        }
        Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity) => {
            WorkConflictObservationResultV1::DroppedAtCapacity { event_kind }
        }
        Err(_) => unavailable(WorkConflictObservationUnavailableV1::ProducerAdmissionUnavailable),
    }
}

const fn unavailable(
    reason: WorkConflictObservationUnavailableV1,
) -> WorkConflictObservationResultV1 {
    WorkConflictObservationResultV1::Unavailable { reason }
}

fn work_conflict_envelope(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    surface_operation: &str,
    result: &NativeIntegrationSurfaceResultV1,
    owner_preview: Option<&NativeIntegrationPreviewV1>,
) -> Result<Option<(ObservabilityEnvelopeV1, &'static str)>, &'static str> {
    match result {
        NativeIntegrationSurfaceResultV1::Preview(preview)
            if surface_operation == PREDICTION_OPERATION =>
        {
            Ok(prediction_envelope(identity, scope_ref, preview)?
                .map(|envelope| (envelope, PREDICTION_EVENT_KIND)))
        }
        NativeIntegrationSurfaceResultV1::Receipt(receipt)
            if surface_operation == OUTCOME_OPERATION =>
        {
            // A receipt exists only when native git ran the transaction to a
            // terminal state, and the daemon threads the exact durable
            // preview it applied. A missing or mismatched preview is invalid
            // owner evidence, not a reason to guess the prediction identity.
            let preview = owner_preview
                .filter(|preview| {
                    preview.validate().is_ok()
                        && preview.preview_id == receipt.status.preview_id
                        && preview.preview_digest == receipt.status.preview_digest
                })
                .ok_or("work_conflict_preview_binding")?;
            outcome_envelope(identity, scope_ref, receipt, preview)
                .map(|envelope| Some((envelope, OUTCOME_EVENT_KIND)))
        }
        _ => Ok(None),
    }
}

fn prediction_envelope(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    preview: &NativeIntegrationPreviewProjectionV1,
) -> Result<Option<ObservabilityEnvelopeV1>, &'static str> {
    let prediction = match &preview.disposition {
        NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_) => {
            ConflictPredictionV1::NoConflict
        }
        NativeIntegrationPreviewDispositionV1::NativeConflict { .. } => {
            ConflictPredictionV1::Conflict
        }
        // The mechanical oracle completed but truthfully defers the
        // integration verdict to semantic review.
        NativeIntegrationPreviewDispositionV1::SemanticReviewRequired { .. } => {
            ConflictPredictionV1::Abstained
        }
        // Already-integrated work has no pending merge relation to predict,
        // and a partial or unavailable preflight never completed its tree
        // merge; neither observed a prediction.
        NativeIntegrationPreviewDispositionV1::AlreadyIntegrated
        | NativeIntegrationPreviewDispositionV1::Partial { .. }
        | NativeIntegrationPreviewDispositionV1::Unavailable { .. } => return Ok(None),
    };
    let prediction_ref = prediction_ref(&preview.preview_id, &preview.preview_digest)?;
    let observation = WorkConflictPredictionObservedV1 {
        prediction_ref: prediction_ref.clone(),
        kind: ConflictKindV1::Mechanical,
        prediction,
        score_kind: ConflictScoreKindV1::Rule,
        descriptor_revision: CONFLICT_DESCRIPTOR_REVISION.to_owned(),
        calibration_revision: CONFLICT_CALIBRATION_REVISION.to_owned(),
        // The preflight evaluates exactly one frozen source-to-destination
        // merge relation, and it observed that whole relation.
        eligible_relation_count: 1,
        // The prediction stands exactly as long as the preview it is bound
        // to: an expired preview can never be applied, so its prediction can
        // never be adjudicated.
        expires_at_micros: preview.expires_at.0,
        coverage: CoverageStateV1::Known,
        local_anchor_refs: Vec::new(),
    };
    execution_owner_fact_envelope(
        identity,
        scope_ref,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &prediction_ref,
            operation: PREDICTION_OPERATION,
            event_time: preview.created_at,
            valid_from: Some(preview.created_at),
            valid_until: Some(preview.expires_at),
            terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
            coverage: CoverageStateV1::Known,
            payload: ObservabilityPayloadV1::WorkConflictPrediction(observation),
        },
    )
    .map(Some)
}

fn outcome_envelope(
    identity: &ObservabilityProducerIdentityV1,
    scope_ref: &str,
    receipt: &NativeIntegrationReceiptProjectionV1,
    preview: &NativeIntegrationPreviewV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // The prediction was observed at preview creation; a receipt completing
    // before its own preview existed is inconsistent owner evidence.
    let horizon_micros = receipt
        .completed_at
        .0
        .checked_sub(preview.created_at.0)
        .and_then(|elapsed| u64::try_from(elapsed).ok())
        .ok_or("work_conflict_outcome_horizon")?;
    let (outcome, adjudicator, coverage, terminal_result) = match receipt.terminal_outcome {
        // Native git integrated exactly the predicted relation and committed:
        // an independent no-conflict adjudication.
        NativeIntegrationTerminalOutcomeV1::Committed => (
            ConflictOutcomeV1::NoConflict,
            ConflictAdjudicatorV1::NativeGit,
            CoverageStateV1::Known,
            ObservabilityTerminalResultV1::Succeeded,
        ),
        // Native git aborted before mutating (drift or cancellation): the
        // predicted relation left observation without adjudication.
        NativeIntegrationTerminalOutcomeV1::AbortedNoChange => (
            ConflictOutcomeV1::Censored,
            ConflictAdjudicatorV1::None,
            CoverageStateV1::Known,
            ObservabilityTerminalResultV1::Unknown,
        ),
        // The mutation was undone; the precomputed candidate tree failed for
        // an environmental reason, which adjudicates neither conflict nor
        // no-conflict.
        NativeIntegrationTerminalOutcomeV1::RolledBack => (
            ConflictOutcomeV1::Unknown,
            ConflictAdjudicatorV1::None,
            CoverageStateV1::Known,
            ObservabilityTerminalResultV1::Failed,
        ),
        NativeIntegrationTerminalOutcomeV1::NeedsInspection => (
            ConflictOutcomeV1::Unknown,
            ConflictAdjudicatorV1::None,
            CoverageStateV1::Unknown,
            ObservabilityTerminalResultV1::Partial,
        ),
    };
    let prediction_ref = prediction_ref(&preview.preview_id, &preview.preview_digest)?;
    let observation = WorkConflictOutcomeLinkedV1 {
        prediction_ref: prediction_ref.clone(),
        kind: ConflictKindV1::Mechanical,
        outcome,
        adjudicator,
        horizon_micros,
        coverage,
        correction_revision: 0,
    };
    execution_owner_fact_envelope(
        identity,
        scope_ref,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &prediction_ref,
            operation: OUTCOME_OPERATION,
            event_time: receipt.completed_at,
            valid_from: Some(receipt.completed_at),
            valid_until: Some(receipt.completed_at),
            terminal_result: Some(terminal_result),
            coverage,
            payload: ObservabilityPayloadV1::WorkConflictOutcome(observation),
        },
    )
}

/// Deterministic prediction identity shared by the preflight prediction and
/// the apply-linked outcome: both sides hold the exact preview identity and
/// content digest, so both derive the same reference without exporting either
/// raw identifier.
fn prediction_ref(
    preview_id: &NativeIntegrationPreviewId,
    preview_digest: &ManifestDigest,
) -> Result<String, &'static str> {
    let digest = canonical_sha256(&(
        "tracedecay.work-conflict.prediction-ref.v1",
        preview_id.as_str(),
        preview_digest.as_str(),
    ))
    .map_err(|_| "work_conflict_prediction_ref")?;
    Ok(format!("work-conflict:{}", digest.as_str()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_application::{
        NativeIntegrationSnapshotProjectionV1, NativeIntegrationStatusProjectionV1,
        ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
    };
    use tracedecay_domain::{
        BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
        BranchStackRevisionV1, BranchStackSourceV1, CommitId, FrozenBranchStackSnapshotV1,
        GitHeadStateV1, GitObjectFormatV1, GitOidV1, GitOperationStateV1,
        MechanicalIntegrationModeV1, NativeIntegrationDirectionV1, NativeIntegrationPhaseV1,
        NativeIntegrationRepositorySnapshotV1, NativeIntegrationSelectionV1,
        NativeIntegrationTransactionId, ProjectId, RefId, RepositoryId, StackNodeId, UtcMicros,
        WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
    };
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;

    use crate::observability::RegisteredObservabilityPortV1;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn oid(byte: char) -> GitOidV1 {
        GitOidV1::new(byte.to_string().repeat(40)).unwrap()
    }

    fn identity(scope_ref: &str) -> ObservabilityProducerIdentityV1 {
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope_ref.to_owned(),
            process_boot_id: "boot:work-conflict-test".to_owned(),
            producer_revision: "work-conflict-test.v1".to_owned(),
            configuration_revision: "work-conflict-test-config.v1".to_owned(),
            policy_revision: "work-conflict-test-policy.v1".to_owned(),
        }
    }

    fn sealed_preview(
        disposition: NativeIntegrationPreviewDispositionV1,
    ) -> NativeIntegrationPreviewV1 {
        let project_id = ProjectId::new("private-project").unwrap();
        let repository_id = RepositoryId::new("private-repository").unwrap();
        let source_node = StackNodeId::new("node.work-conflict.source").unwrap();
        let destination_node = StackNodeId::new("node.work-conflict.destination").unwrap();
        let source_ref = RefId::new("refs/heads/private-source-ref").unwrap();
        let destination_ref = RefId::new("refs/heads/private-target-ref").unwrap();
        let destination_worktree = WorktreeId::new("worktree.work-conflict.destination").unwrap();
        let revision = BranchStackRevisionV1::new(
            BranchStackId::new("stack.work-conflict").unwrap(),
            BranchStackRevisionId::new("revision.work-conflict").unwrap(),
            WorktreeInventorySnapshotId::new("inventory.work-conflict").unwrap(),
            WorktreeInventoryEpoch::new(1).unwrap(),
            BranchStackSourceV1::ExplicitDeclaration,
            vec![
                BranchStackNodeV1 {
                    node_id: source_node.clone(),
                    project_id: project_id.clone(),
                    repository_id: repository_id.clone(),
                    reference: source_ref.clone(),
                    tip: CommitId::new("1".repeat(40)).unwrap(),
                    worktree_id: Some(WorktreeId::new("worktree.work-conflict.source").unwrap()),
                },
                BranchStackNodeV1 {
                    node_id: destination_node.clone(),
                    project_id: project_id.clone(),
                    repository_id: repository_id.clone(),
                    reference: destination_ref.clone(),
                    tip: CommitId::new("2".repeat(40)).unwrap(),
                    worktree_id: Some(destination_worktree.clone()),
                },
            ],
            vec![BranchStackEdgeV1 {
                dependency: source_node.clone(),
                dependent: destination_node.clone(),
            }],
        )
        .unwrap();
        let selection = NativeIntegrationSelectionV1::DeclaredStackEdge(
            FrozenBranchStackSnapshotV1::new(
                revision,
                source_node,
                destination_node,
                NativeIntegrationDirectionV1::PropagateDependencyToDependent,
                UtcMicros(10),
            )
            .unwrap(),
        );
        let repository_snapshot = NativeIntegrationRepositorySnapshotV1 {
            project_id,
            repository_id,
            source_worktree_id: selection.source_worktree_id().unwrap().cloned(),
            destination_worktree_id: Some(destination_worktree),
            source_ref,
            destination_ref: destination_ref.clone(),
            source_tip: oid('1'),
            destination_tip: oid('2'),
            source_tree: oid('3'),
            destination_tree: oid('4'),
            merge_base: oid('5'),
            dependency_commits: vec![oid('1')],
            destination_head: GitHeadStateV1::Attached {
                branch: destination_ref.as_str().to_owned(),
                commit: oid('2'),
            },
            refs_digest: digest('b'),
            index_digest: digest('c'),
            worktree_digest: digest('d'),
            attributes_digest: digest('e'),
            operation_state: GitOperationStateV1::None,
            clean: true,
            object_format: GitObjectFormatV1::Sha1,
            adapter_revision: "gix.work-conflict.v1".to_owned(),
            captured_at: UtcMicros(11),
            digest: digest('0'),
        }
        .seal()
        .unwrap();
        let eligible = matches!(
            disposition,
            NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
        );
        NativeIntegrationPreviewV1 {
            preview_id: NativeIntegrationPreviewId::new("preview.work-conflict.fixture").unwrap(),
            selection,
            repository_snapshot,
            grant_digest: digest('f'),
            policy_digest: digest('1'),
            graph_revision_digest: digest('2'),
            test_revision_digest: digest('3'),
            schema_revision_digest: digest('4'),
            migration_revision_digest: digest('5'),
            disposition,
            candidate_tree: eligible.then(|| oid('6')),
            ordered_commits: vec![oid('1')],
            created_at: UtcMicros(12),
            expires_at: UtcMicros(1_000),
            preview_digest: digest('0'),
        }
        .seal()
        .unwrap()
    }

    fn eligible_preview() -> NativeIntegrationPreviewV1 {
        sealed_preview(
            NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
                MechanicalIntegrationModeV1::FastForward,
            ),
        )
    }

    fn preview_result(preview: &NativeIntegrationPreviewV1) -> NativeIntegrationSurfaceResultV1 {
        NativeIntegrationSurfaceResultV1::Preview(
            NativeIntegrationPreviewProjectionV1::project(preview).unwrap(),
        )
    }

    fn projection_result(
        disposition: NativeIntegrationPreviewDispositionV1,
    ) -> NativeIntegrationSurfaceResultV1 {
        NativeIntegrationSurfaceResultV1::Preview(NativeIntegrationPreviewProjectionV1 {
            preview_id: NativeIntegrationPreviewId::new("preview.work-conflict.projection")
                .unwrap(),
            preview_digest: digest('a'),
            selection: NativeIntegrationSnapshotProjectionV1 {
                selection_digest: digest('b'),
                project_id: ProjectId::new("private-project").unwrap(),
                repository_id: RepositoryId::new("private-repository").unwrap(),
                source_ref: RefId::new("private-source-ref").unwrap(),
                destination_ref: RefId::new("private-target-ref").unwrap(),
                inventory_epoch: WorktreeInventoryEpoch::new(1).unwrap(),
                frozen_at: UtcMicros(10),
            },
            disposition,
            ordered_commit_count: 1,
            created_at: UtcMicros(12),
            expires_at: UtcMicros(1_000),
        })
    }

    fn receipt_result(
        preview: &NativeIntegrationPreviewV1,
        terminal_outcome: NativeIntegrationTerminalOutcomeV1,
        completed_at: UtcMicros,
    ) -> NativeIntegrationSurfaceResultV1 {
        NativeIntegrationSurfaceResultV1::Receipt(NativeIntegrationReceiptProjectionV1 {
            status: NativeIntegrationStatusProjectionV1 {
                transaction_id: NativeIntegrationTransactionId::new(
                    "transaction.work-conflict.fixture",
                )
                .unwrap(),
                preview_id: preview.preview_id.clone(),
                preview_digest: preview.preview_digest.clone(),
                repository_id: preview.repository_snapshot.repository_id.clone(),
                destination_ref: preview.repository_snapshot.destination_ref.clone(),
                phase: NativeIntegrationPhaseV1::Terminal,
                phase_revision: 5,
                cancellation_requested: false,
                terminal_outcome: Some(terminal_outcome),
                updated_at: completed_at,
            },
            terminal_outcome,
            final_ref_tip: "private-final-object".to_owned(),
            final_tree: "private-final-tree".to_owned(),
            completed_at,
            receipt_digest: digest('9'),
        })
    }

    #[tokio::test]
    async fn preflight_prediction_and_apply_outcome_persist_one_linked_pair() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project dir");
        let project_id = ProjectId::new("project.work-conflict.durable").unwrap();
        let scope_ref = project_id.as_str().to_owned();
        let runtime = RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered runtime");
        let database = runtime.project_database_arc().expect("project database");
        let producer =
            BoundedObservabilityProducerV1::start(database.clone(), identity(&scope_ref), 64)
                .expect("producer");

        let preview = eligible_preview();
        assert_eq!(
            record_work_conflict_observation(
                &scope_ref,
                Some(&producer),
                PREDICTION_OPERATION,
                true,
                &preview_result(&preview),
                Some(&preview),
            ),
            WorkConflictObservationResultV1::Enqueued {
                event_kind: PREDICTION_EVENT_KIND,
            }
        );
        assert_eq!(
            record_work_conflict_observation(
                &scope_ref,
                Some(&producer),
                OUTCOME_OPERATION,
                true,
                &receipt_result(
                    &preview,
                    NativeIntegrationTerminalOutcomeV1::Committed,
                    UtcMicros(20),
                ),
                Some(&preview),
            ),
            WorkConflictObservationResultV1::Enqueued {
                event_kind: OUTCOME_EVENT_KIND,
            }
        );
        // A denied scope writes nothing.
        assert_eq!(
            record_work_conflict_observation(
                "project.work-conflict.foreign",
                Some(&producer),
                PREDICTION_OPERATION,
                true,
                &preview_result(&preview),
                Some(&preview),
            ),
            WorkConflictObservationResultV1::Unavailable {
                reason: WorkConflictObservationUnavailableV1::OwnerEvidenceInvalid,
            }
        );
        producer.shutdown().await.expect("flush producer");
        drop(producer);

        let port = RegisteredObservabilityPortV1::new(database.as_ref());
        let page = port
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: scope_ref.clone(),
                event_kinds: vec![
                    PREDICTION_EVENT_KIND.to_owned(),
                    OUTCOME_EVENT_KIND.to_owned(),
                ],
                horizon: ObservabilityHorizonV1 {
                    since_micros: 0,
                    until_micros: 10_000,
                },
                after_watermark: None,
                limit: 64,
            })
            .await
            .expect("durable conflict query");
        assert_eq!(page.events.len(), 2, "exactly one linked pair persists");

        let prediction = page
            .events
            .iter()
            .find(|event| event.event_kind == PREDICTION_EVENT_KIND)
            .expect("persisted prediction");
        let outcome = page
            .events
            .iter()
            .find(|event| event.event_kind == OUTCOME_EVENT_KIND)
            .expect("persisted outcome");
        let ObservabilityPayloadV1::WorkConflictPrediction(prediction_payload) =
            &prediction.payload
        else {
            panic!("wrong prediction payload family");
        };
        let ObservabilityPayloadV1::WorkConflictOutcome(outcome_payload) = &outcome.payload else {
            panic!("wrong outcome payload family");
        };
        assert_eq!(
            prediction_payload.prediction_ref, outcome_payload.prediction_ref,
            "outcome links the exact prediction identity"
        );
        assert_eq!(
            prediction.trace_id, outcome.trace_id,
            "prediction and outcome share one owner trace"
        );
        assert_eq!(prediction_payload.kind, ConflictKindV1::Mechanical);
        assert_eq!(
            prediction_payload.prediction,
            ConflictPredictionV1::NoConflict
        );
        assert_eq!(prediction_payload.score_kind, ConflictScoreKindV1::Rule);
        assert_eq!(prediction_payload.eligible_relation_count, 1);
        assert_eq!(
            prediction_payload.expires_at_micros, preview.expires_at.0,
            "prediction expiry is the preview's own expiry"
        );
        assert_eq!(outcome_payload.kind, ConflictKindV1::Mechanical);
        assert_eq!(outcome_payload.outcome, ConflictOutcomeV1::NoConflict);
        assert_eq!(
            outcome_payload.adjudicator,
            ConflictAdjudicatorV1::NativeGit
        );
        assert_eq!(
            outcome_payload.horizon_micros, 8,
            "horizon is apply completion minus preview creation"
        );
        assert_eq!(outcome_payload.correction_revision, 0);

        let wire = serde_json::to_string(&page.events).expect("serialize persisted pair");
        for prohibited in [
            "private-project",
            "private-repository",
            "private-source-ref",
            "private-target-ref",
            "private-final-object",
            "private-final-tree",
            "preview.work-conflict.fixture",
        ] {
            assert!(!wire.contains(prohibited), "leaked {prohibited}");
        }
    }

    #[test]
    fn conflict_preview_predicts_conflict_and_semantic_review_abstains() {
        let identity = identity("project.scope");
        let (envelope, event_kind) = work_conflict_envelope(
            &identity,
            "project.scope",
            PREDICTION_OPERATION,
            &projection_result(NativeIntegrationPreviewDispositionV1::NativeConflict {
                conflict_digest: digest('8'),
            }),
            None,
        )
        .expect("conflict prediction")
        .expect("adjudicated disposition");
        assert_eq!(event_kind, PREDICTION_EVENT_KIND);
        let ObservabilityPayloadV1::WorkConflictPrediction(payload) = &envelope.payload else {
            panic!("wrong payload family");
        };
        assert_eq!(payload.prediction, ConflictPredictionV1::Conflict);
        assert_eq!(payload.descriptor_revision, CONFLICT_DESCRIPTOR_REVISION);
        assert_eq!(payload.calibration_revision, CONFLICT_CALIBRATION_REVISION);

        let (envelope, _) = work_conflict_envelope(
            &identity,
            "project.scope",
            PREDICTION_OPERATION,
            &projection_result(
                NativeIntegrationPreviewDispositionV1::SemanticReviewRequired {
                    evidence_digest: digest('7'),
                },
            ),
            None,
        )
        .expect("semantic-review prediction")
        .expect("abstaining disposition");
        let ObservabilityPayloadV1::WorkConflictPrediction(payload) = &envelope.payload else {
            panic!("wrong payload family");
        };
        assert_eq!(payload.prediction, ConflictPredictionV1::Abstained);
    }

    #[test]
    fn unadjudicated_dispositions_reads_and_wrong_operations_emit_nothing() {
        let identity = identity("project.scope");
        for disposition in [
            NativeIntegrationPreviewDispositionV1::AlreadyIntegrated,
            NativeIntegrationPreviewDispositionV1::Partial {
                reason: tracedecay_domain::NativeIntegrationUnavailabilityV1::PartialEvidence,
            },
            NativeIntegrationPreviewDispositionV1::Unavailable {
                reason: tracedecay_domain::NativeIntegrationUnavailabilityV1::Denied,
            },
        ] {
            assert!(
                work_conflict_envelope(
                    &identity,
                    "project.scope",
                    PREDICTION_OPERATION,
                    &projection_result(disposition),
                    None,
                )
                .expect("typed non-adjudication")
                .is_none()
            );
        }
        // A preview reached under any operation but preflight proves nothing.
        assert!(
            work_conflict_envelope(
                &identity,
                "project.scope",
                "native_integration_status",
                &projection_result(
                    NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
                        MechanicalIntegrationModeV1::FastForward,
                    ),
                ),
                None,
            )
            .expect("wrong operation")
            .is_none()
        );
        // Refused applies never reach a receipt; the unavailable result
        // truthfully adjudicates nothing.
        assert!(
            work_conflict_envelope(
                &identity,
                "project.scope",
                OUTCOME_OPERATION,
                &NativeIntegrationSurfaceResultV1::unavailable(
                    tracedecay_application::NativeIntegrationSurfaceUnavailableV1::Denied,
                ),
                None,
            )
            .expect("refused apply")
            .is_none()
        );
    }

    #[test]
    fn aborted_rolled_back_and_uninspectable_applies_never_claim_adjudication() {
        let identity = identity("project.scope");
        let preview = eligible_preview();
        let cases = [
            (
                NativeIntegrationTerminalOutcomeV1::AbortedNoChange,
                ConflictOutcomeV1::Censored,
                CoverageStateV1::Known,
            ),
            (
                NativeIntegrationTerminalOutcomeV1::RolledBack,
                ConflictOutcomeV1::Unknown,
                CoverageStateV1::Known,
            ),
            (
                NativeIntegrationTerminalOutcomeV1::NeedsInspection,
                ConflictOutcomeV1::Unknown,
                CoverageStateV1::Unknown,
            ),
        ];
        for (terminal_outcome, expected_outcome, expected_coverage) in cases {
            let (envelope, _) = work_conflict_envelope(
                &identity,
                "project.scope",
                OUTCOME_OPERATION,
                &receipt_result(&preview, terminal_outcome, UtcMicros(20)),
                Some(&preview),
            )
            .expect("terminal receipt")
            .expect("linked outcome");
            let ObservabilityPayloadV1::WorkConflictOutcome(payload) = &envelope.payload else {
                panic!("wrong payload family");
            };
            assert_eq!(payload.outcome, expected_outcome);
            assert_eq!(payload.adjudicator, ConflictAdjudicatorV1::None);
            assert_eq!(payload.coverage, expected_coverage);
        }
    }

    #[test]
    fn missing_or_mismatched_owner_evidence_is_a_typed_refusal_never_a_panic() {
        let identity = identity("project.scope");
        let preview = eligible_preview();
        let receipt = receipt_result(
            &preview,
            NativeIntegrationTerminalOutcomeV1::Committed,
            UtcMicros(20),
        );
        // A receipt without its durable preview cannot name a prediction.
        assert!(
            work_conflict_envelope(
                &identity,
                "project.scope",
                OUTCOME_OPERATION,
                &receipt,
                None
            )
            .is_err()
        );
        // A preview that does not match the receipt identity is foreign
        // evidence, not a linkable prediction.
        let mut foreign = eligible_preview();
        foreign.preview_id =
            NativeIntegrationPreviewId::new("preview.work-conflict.foreign").unwrap();
        let foreign = foreign.seal().unwrap();
        assert!(
            work_conflict_envelope(
                &identity,
                "project.scope",
                OUTCOME_OPERATION,
                &receipt,
                Some(&foreign),
            )
            .is_err()
        );
        // A receipt completing before its preview existed is inconsistent.
        assert!(
            work_conflict_envelope(
                &identity,
                "project.scope",
                OUTCOME_OPERATION,
                &receipt_result(
                    &preview,
                    NativeIntegrationTerminalOutcomeV1::Committed,
                    UtcMicros(11),
                ),
                Some(&preview),
            )
            .is_err()
        );
    }

    #[test]
    fn absent_producer_or_unmounted_owner_is_typed_unavailable() {
        let preview = eligible_preview();
        assert_eq!(
            record_work_conflict_observation(
                "project.scope",
                None,
                PREDICTION_OPERATION,
                true,
                &preview_result(&preview),
                Some(&preview),
            ),
            WorkConflictObservationResultV1::Unavailable {
                reason: WorkConflictObservationUnavailableV1::ProducerUnmounted,
            }
        );
        assert_eq!(
            record_work_conflict_observation(
                "project.scope",
                None,
                PREDICTION_OPERATION,
                false,
                &preview_result(&preview),
                Some(&preview),
            ),
            WorkConflictObservationResultV1::Unavailable {
                reason: WorkConflictObservationUnavailableV1::OwnerUnmounted,
            }
        );
    }
}

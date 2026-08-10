//! Exact native-integration evidence translated into stack transitions.
//!
//! The daemon stack runtime owns recipient selection, authorization, durable
//! watermarks, delivery, and expansion. This module only classifies a sealed
//! declared-stack preview or terminal receipt and derives replay-stable signal
//! identity from that canonical evidence.

use tracedecay_application::ResolvedScope;
use tracedecay_domain::{
    BranchStackRevisionV1, ManifestDigest, NativeIntegrationPreviewDispositionV1,
    NativeIntegrationPreviewV1, NativeIntegrationReceiptV1, NativeIntegrationSelectionV1,
    NativeIntegrationTerminalOutcomeV1, UtcMicros,
};
use tracedecay_usecases::stack_coordinator::{
    StackCoordinatorErrorV1, StackSignalDraftV1, StackSignalKindV1, StackSignalV1,
};

/// Produces the one truthful transition represented by a sealed preflight.
/// Independent-branch previews and non-transition dispositions remain silent.
pub(crate) fn signal_from_preflight(
    scope: &ResolvedScope,
    preview: &NativeIntegrationPreviewV1,
) -> Result<Option<StackSignalV1>, StackCoordinatorErrorV1> {
    preview.validate().map_err(invalid)?;
    let Some(revision) = declared_revision_for_scope(scope, preview)? else {
        return Ok(None);
    };
    let kind = match &preview.disposition {
        NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_) => {
            StackSignalKindV1::DependencyReady
        }
        NativeIntegrationPreviewDispositionV1::NativeConflict { .. } => {
            StackSignalKindV1::ActualConflict
        }
        NativeIntegrationPreviewDispositionV1::AlreadyIntegrated
        | NativeIntegrationPreviewDispositionV1::SemanticReviewRequired { .. }
        | NativeIntegrationPreviewDispositionV1::Partial { .. }
        | NativeIntegrationPreviewDispositionV1::Unavailable { .. } => return Ok(None),
    };
    build_signal(
        scope,
        revision,
        kind,
        preview.preview_digest.clone(),
        preview.created_at,
    )
    .map(Some)
}

/// Produces the one material transition represented by a sealed terminal
/// receipt. No-change and rolled-back terminals never claim stack progress.
pub(crate) fn signal_from_receipt(
    scope: &ResolvedScope,
    preview: &NativeIntegrationPreviewV1,
    receipt: &NativeIntegrationReceiptV1,
) -> Result<Option<StackSignalV1>, StackCoordinatorErrorV1> {
    preview.validate().map_err(invalid)?;
    receipt.validate().map_err(invalid)?;
    let Some(revision) = declared_revision_for_scope(scope, preview)? else {
        return Ok(None);
    };
    if !matches!(
        &preview.disposition,
        NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
    ) || receipt.status.preview_id != preview.preview_id
        || receipt.status.preview_digest != preview.preview_digest
        || receipt.status.repository_id != scope.repository_id
        || scope.reference.as_ref() != Some(&receipt.status.destination_ref)
        || receipt.status.expected_destination_tip != preview.repository_snapshot.destination_tip
        || receipt.completed_at != receipt.status.updated_at
        || receipt.completed_at < preview.created_at
    {
        return Err(StackCoordinatorErrorV1::Stale);
    }
    let kind = match receipt.status.terminal_outcome {
        Some(NativeIntegrationTerminalOutcomeV1::Committed) => {
            if receipt.status.candidate_tip.as_ref() != Some(&receipt.final_ref_tip) {
                return Err(StackCoordinatorErrorV1::Stale);
            }
            StackSignalKindV1::IntegrationCommitted
        }
        Some(NativeIntegrationTerminalOutcomeV1::NeedsInspection) => {
            StackSignalKindV1::IntegrationNeedsInspection
        }
        Some(
            NativeIntegrationTerminalOutcomeV1::AbortedNoChange
            | NativeIntegrationTerminalOutcomeV1::RolledBack,
        ) => return Ok(None),
        None => {
            return Err(StackCoordinatorErrorV1::Invalid(
                "missing terminal outcome".into(),
            ));
        }
    };
    build_signal(
        scope,
        revision,
        kind,
        receipt.receipt_digest.clone(),
        receipt.completed_at,
    )
    .map(Some)
}

fn declared_revision_for_scope<'a>(
    scope: &ResolvedScope,
    preview: &'a NativeIntegrationPreviewV1,
) -> Result<Option<&'a BranchStackRevisionV1>, StackCoordinatorErrorV1> {
    scope.validate().map_err(invalid)?;
    let NativeIntegrationSelectionV1::DeclaredStackEdge(selection) = &preview.selection else {
        return Ok(None);
    };
    let destination = selection.destination().map_err(invalid)?;
    if destination.project_id != scope.project_id
        || destination.repository_id != scope.repository_id
        || scope.reference.as_ref() != Some(&destination.reference)
        || preview.repository_snapshot.project_id != scope.project_id
        || preview.repository_snapshot.repository_id != scope.repository_id
        || preview.repository_snapshot.destination_ref != destination.reference
        || preview.repository_snapshot.destination_worktree_id.as_ref() != Some(&scope.worktree_id)
        || preview.selection.source_tip().map_err(invalid)?
            != preview.repository_snapshot.source_tip
        || preview.selection.destination_tip().map_err(invalid)?
            != preview.repository_snapshot.destination_tip
        || destination
            .worktree_id
            .as_ref()
            .is_some_and(|worktree| worktree != &scope.worktree_id)
    {
        return Err(StackCoordinatorErrorV1::Stale);
    }
    Ok(Some(&selection.revision))
}

fn build_signal(
    scope: &ResolvedScope,
    revision: &BranchStackRevisionV1,
    kind: StackSignalKindV1,
    state_digest: ManifestDigest,
    observed_at: UtcMicros,
) -> Result<StackSignalV1, StackCoordinatorErrorV1> {
    StackSignalV1::seal(
        scope,
        StackSignalDraftV1 {
            stack_revision_id: revision.revision_id.clone(),
            stack_revision_digest: revision.digest.clone(),
            kind,
            state_digest,
            github_stack_digest: None,
            observed_at,
        },
    )
}

fn invalid(error: impl std::fmt::Display) -> StackCoordinatorErrorV1 {
    StackCoordinatorErrorV1::Invalid(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
        BranchStackSourceV1, CommitId, FrozenBranchStackSnapshotV1, GitHeadStateV1,
        GitObjectFormatV1, GitOidV1, GitOperationStateV1, MechanicalIntegrationModeV1,
        NativeIntegrationApprovalId, NativeIntegrationDirectionV1, NativeIntegrationPhaseV1,
        NativeIntegrationPreviewId, NativeIntegrationRepositorySnapshotV1,
        NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1, ProjectId, RefId,
        RepositoryId, StackNodeId, WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
    };

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn oid(byte: char) -> GitOidV1 {
        GitOidV1::new(byte.to_string().repeat(40)).expect("oid")
    }

    fn scope(worktree: &str) -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new("project.stack-producer").expect("project"),
            RepositoryId::new("repository.stack-producer").expect("repository"),
            WorktreeId::new(worktree).expect("worktree"),
            Some(RefId::new("refs/heads/dependent").expect("ref")),
        )
        .expect("scope")
    }

    fn preview(
        disposition: NativeIntegrationPreviewDispositionV1,
    ) -> (ResolvedScope, NativeIntegrationPreviewV1) {
        let scope = scope("worktree.stack-producer.dependent");
        let source_node = StackNodeId::new("node.stack-producer.source").expect("source node");
        let destination_node =
            StackNodeId::new("node.stack-producer.destination").expect("destination node");
        let source_ref = RefId::new("refs/heads/dependency").expect("source ref");
        let destination_ref = scope.reference.clone().expect("destination ref");
        let revision = BranchStackRevisionV1::new(
            BranchStackId::new("stack.stack-producer").expect("stack"),
            BranchStackRevisionId::new("revision.stack-producer").expect("revision"),
            WorktreeInventorySnapshotId::new("inventory.stack-producer").expect("inventory"),
            WorktreeInventoryEpoch::new(1).expect("epoch"),
            BranchStackSourceV1::ExplicitDeclaration,
            vec![
                BranchStackNodeV1 {
                    node_id: source_node.clone(),
                    project_id: scope.project_id.clone(),
                    repository_id: scope.repository_id.clone(),
                    reference: source_ref.clone(),
                    tip: CommitId::new("1".repeat(40)).expect("source tip"),
                    worktree_id: Some(
                        WorktreeId::new("worktree.stack-producer.source").expect("source worktree"),
                    ),
                },
                BranchStackNodeV1 {
                    node_id: destination_node.clone(),
                    project_id: scope.project_id.clone(),
                    repository_id: scope.repository_id.clone(),
                    reference: destination_ref.clone(),
                    tip: CommitId::new("2".repeat(40)).expect("destination tip"),
                    worktree_id: Some(scope.worktree_id.clone()),
                },
            ],
            vec![BranchStackEdgeV1 {
                dependency: source_node.clone(),
                dependent: destination_node.clone(),
            }],
        )
        .expect("revision");
        let selection = NativeIntegrationSelectionV1::DeclaredStackEdge(
            FrozenBranchStackSnapshotV1::new(
                revision,
                source_node,
                destination_node,
                NativeIntegrationDirectionV1::PropagateDependencyToDependent,
                UtcMicros(10),
            )
            .expect("selection"),
        );
        let repository_snapshot = NativeIntegrationRepositorySnapshotV1 {
            project_id: scope.project_id.clone(),
            repository_id: scope.repository_id.clone(),
            source_worktree_id: selection
                .source_worktree_id()
                .expect("source worktree")
                .cloned(),
            destination_worktree_id: Some(scope.worktree_id.clone()),
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
            adapter_revision: "gix.stack-producer.v1".to_owned(),
            captured_at: UtcMicros(11),
            digest: digest('0'),
        }
        .seal()
        .expect("repository snapshot");
        let eligible = matches!(
            disposition,
            NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
        );
        let preview = NativeIntegrationPreviewV1 {
            preview_id: NativeIntegrationPreviewId::new("preview.stack-producer").expect("preview"),
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
        .expect("sealed preview");
        (scope, preview)
    }

    fn receipt(
        preview: &NativeIntegrationPreviewV1,
        outcome: NativeIntegrationTerminalOutcomeV1,
    ) -> NativeIntegrationReceiptV1 {
        NativeIntegrationReceiptV1 {
            status: NativeIntegrationTransactionStatusV1 {
                transaction_id: NativeIntegrationTransactionId::new("transaction.stack-producer")
                    .expect("transaction"),
                preview_id: preview.preview_id.clone(),
                preview_digest: preview.preview_digest.clone(),
                approval_id: NativeIntegrationApprovalId::new("approval.stack-producer")
                    .expect("approval"),
                repository_id: preview.repository_snapshot.repository_id.clone(),
                destination_ref: preview.repository_snapshot.destination_ref.clone(),
                expected_destination_tip: preview.repository_snapshot.destination_tip.clone(),
                candidate_tip: Some(oid('7')),
                phase: NativeIntegrationPhaseV1::Terminal,
                phase_revision: 4,
                cancellation_requested: false,
                terminal_outcome: Some(outcome),
                updated_at: UtcMicros(20),
            },
            final_ref_tip: oid('7'),
            final_tree: oid('8'),
            final_index_digest: digest('6'),
            final_worktree_digest: digest('7'),
            completed_at: UtcMicros(20),
            receipt_digest: digest('0'),
        }
        .seal()
        .expect("sealed receipt")
    }

    #[test]
    fn declared_preflight_signals_are_truthful_and_replay_stable() {
        let (scope, eligible) = preview(
            NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
                MechanicalIntegrationModeV1::FastForward,
            ),
        );
        let first = signal_from_preflight(&scope, &eligible)
            .expect("signal result")
            .expect("dependency-ready signal");
        let replay = signal_from_preflight(&scope, &eligible)
            .expect("replay result")
            .expect("replay signal");
        assert_eq!(first, replay);
        assert_eq!(first.kind, StackSignalKindV1::DependencyReady);
        assert_eq!(first.state_digest, eligible.preview_digest);

        let (_, conflict) = preview(NativeIntegrationPreviewDispositionV1::NativeConflict {
            conflict_digest: digest('8'),
        });
        assert_eq!(
            signal_from_preflight(&scope, &conflict)
                .expect("conflict result")
                .expect("conflict signal")
                .kind,
            StackSignalKindV1::ActualConflict
        );
    }

    #[test]
    fn terminal_receipts_emit_only_material_stack_outcomes() {
        let (scope, preview) = preview(
            NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
                MechanicalIntegrationModeV1::FastForward,
            ),
        );
        let committed = receipt(&preview, NativeIntegrationTerminalOutcomeV1::Committed);
        let signal = signal_from_receipt(&scope, &preview, &committed)
            .expect("committed result")
            .expect("committed signal");
        assert_eq!(signal.kind, StackSignalKindV1::IntegrationCommitted);
        assert_eq!(signal.state_digest, committed.receipt_digest);

        let mut mismatched_commit = committed.clone();
        mismatched_commit.final_ref_tip = oid('9');
        mismatched_commit = mismatched_commit.seal().expect("mismatched receipt");
        assert_eq!(
            signal_from_receipt(&scope, &preview, &mismatched_commit),
            Err(StackCoordinatorErrorV1::Stale)
        );

        let inspection = receipt(
            &preview,
            NativeIntegrationTerminalOutcomeV1::NeedsInspection,
        );
        assert_eq!(
            signal_from_receipt(&scope, &preview, &inspection)
                .expect("inspection result")
                .expect("inspection signal")
                .kind,
            StackSignalKindV1::IntegrationNeedsInspection
        );
        let no_change = receipt(
            &preview,
            NativeIntegrationTerminalOutcomeV1::AbortedNoChange,
        );
        assert!(
            signal_from_receipt(&scope, &preview, &no_change)
                .expect("no-change result")
                .is_none()
        );
    }

    #[test]
    fn cross_worktree_scope_is_stale() {
        let (_, preview) = preview(
            NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
                MechanicalIntegrationModeV1::FastForward,
            ),
        );
        assert_eq!(
            signal_from_preflight(&scope("worktree.stack-producer.foreign"), &preview),
            Err(StackCoordinatorErrorV1::Stale)
        );
    }
}

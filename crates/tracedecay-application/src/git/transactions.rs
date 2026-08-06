//! Transport-neutral application contracts for PR11 Git index transactions.
//!
//! This module owns request admission, authority binding, receipt projection,
//! and idempotency identity. Native Git execution, serialization, durable
//! journals, and recovery remain injected ports owned by the daemon/store
//! adapters. No request carries Git arguments, paths, refs, or command text.

use serde::Serialize;
use thiserror::Error;
use tracedecay_domain::{
    GitIndexCommitIntentV1, GitIndexIdempotencyKey, GitIndexPreviewId, GitIndexPreviewV1,
    GitIndexReceiptOutcomeV1, GitIndexTransactionId, GitIndexTransactionOperationV1,
    GitIndexTransactionReceiptV1, ManifestDigest, RepositoryId, RepositoryStateSnapshotV1,
    RetrievalAnchorId, UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, EffectClass, UseCaseId};

use crate::{
    ApplicationContractError, AuthorityReceipt, EffectId, EffectReceipt, EffectResult,
    EffectTermination, IdempotencyKey, OperationReceipt, OperationTermination, PreviewId,
    PreviewResult, ReconciliationState, RequestAdmission, RequestContext,
};

const GIT_INDEX_PREVIEW_REQUEST_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.git-index-preview-request.v1";
const GIT_INDEX_APPLY_REQUEST_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.git-index-apply-request.v1";

/// One capability/use-case binding for a closed Git index operation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GitIndexOperationBindingV1 {
    pub capability_id: CapabilityId,
    pub use_case_id: UseCaseId,
    pub operation: GitIndexTransactionOperationV1,
}

impl GitIndexOperationBindingV1 {
    pub fn for_operation(
        operation: GitIndexTransactionOperationV1,
    ) -> Result<Self, ApplicationContractError> {
        let (capability, use_case) = git_index_operation_ids(operation);
        Ok(Self {
            capability_id: CapabilityId::new(capability)?,
            use_case_id: UseCaseId::new(use_case)?,
            operation,
        })
    }

    fn validate(&self) -> Result<(), ApplicationContractError> {
        let (capability, use_case) = git_index_operation_ids(self.operation);
        if self.capability_id != CapabilityId::new(capability)?
            || self.use_case_id != UseCaseId::new(use_case)?
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index transaction operation binding",
            });
        }
        Ok(())
    }
}

pub(crate) const fn git_index_operation_ids(
    operation: GitIndexTransactionOperationV1,
) -> (&'static str, &'static str) {
    match operation {
        GitIndexTransactionOperationV1::StageHunks => {
            ("capability.git.stage-hunks", "use-case.git.stage-hunks")
        }
        GitIndexTransactionOperationV1::UnstageHunks => {
            ("capability.git.unstage-hunks", "use-case.git.unstage-hunks")
        }
        GitIndexTransactionOperationV1::CommitIndex => {
            ("capability.git.commit-index", "use-case.git.commit-index")
        }
    }
}

/// Map each irreversible operation to its distinct catalog effect class.
pub const fn git_index_effect_class(operation: GitIndexTransactionOperationV1) -> EffectClass {
    match operation {
        GitIndexTransactionOperationV1::StageHunks => EffectClass::GitIndexStage,
        GitIndexTransactionOperationV1::UnstageHunks => EffectClass::GitIndexUnstage,
        GitIndexTransactionOperationV1::CommitIndex => EffectClass::GitIndexCommit,
    }
}

/// Sink evidence that must be carried into an admitted effect receipt.
///
/// The policy digest must match the `AuthorityReceipt`; configuration, catalog,
/// and privacy digests remain explicit because the application does not own
/// their source of truth.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GitIndexEffectProofV1 {
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub catalog_digest: ManifestDigest,
    pub privacy_digest: ManifestDigest,
    pub external_proof: Option<RetrievalAnchorId>,
}

impl GitIndexEffectProofV1 {
    pub fn validate_for(
        &self,
        authority: &AuthorityReceipt,
    ) -> Result<(), ApplicationContractError> {
        self.policy_digest.validate()?;
        self.configuration_digest.validate()?;
        self.catalog_digest.validate()?;
        self.privacy_digest.validate()?;
        self.external_proof
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)?;
        if self.policy_digest != authority.policy.digest {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index effect proof policy digest",
            });
        }
        Ok(())
    }
}

/// Immutable application request for an index-mutation preview.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GitIndexPreviewRequestV1 {
    pub context: RequestContext,
    pub authority: AuthorityReceipt,
    pub binding: GitIndexOperationBindingV1,
    /// The daemon-issued opaque preview identity that every selected
    /// `HunkRefV1` must already carry.
    pub preview_id: GitIndexPreviewId,
    /// Exact query read-only repository authority snapshot. The daemon captures
    /// current native state independently and requires byte-for-byte typed
    /// equality before it mints an applicable mutation preview.
    pub repository_snapshot: RepositoryStateSnapshotV1,
    /// A native adapter must re-mint and revalidate these exact references
    /// while constructing its immutable preview; it cannot relocate a hunk.
    pub selected_hunks: Vec<tracedecay_domain::HunkRefV1>,
    pub commit_intent: Option<GitIndexCommitIntentV1>,
    pub observed_at: UtcMicros,
}

impl GitIndexPreviewRequestV1 {
    pub fn input_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        self.validate()?;
        Ok(canonical_sha256(&(
            GIT_INDEX_PREVIEW_REQUEST_DIGEST_DOMAIN_V1,
            self,
        ))?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_admission(
            &self.context,
            &self.authority,
            &self.binding,
            self.observed_at,
        )?;
        self.preview_id.validate()?;
        self.repository_snapshot.validate()?;
        let snapshot_digest =
            GitIndexPreviewV1::repository_snapshot_digest(&self.repository_snapshot)?;
        if self.context.scope().project_id != self.repository_snapshot.project_id
            || self.context.scope().repository_id != self.repository_snapshot.repository_id
            || self.repository_snapshot.worktree_id.as_ref()
                != Some(&self.context.scope().worktree_id)
            || !scope_reference_matches_snapshot(
                self.context.scope().reference.as_ref(),
                &self.repository_snapshot,
            )
            || (self.binding.operation == GitIndexTransactionOperationV1::CommitIndex
                && self.context.scope().reference.is_none())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index preview repository scope",
            });
        }
        for hunk in &self.selected_hunks {
            hunk.validate()?;
            if self.binding.operation.hunk_direction() != Some(hunk.direction)
                || hunk.preview_id != self.preview_id.as_str()
                || hunk.snapshot_digest != snapshot_digest
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "git index preview hunk binding",
                });
            }
        }
        match self.binding.operation {
            GitIndexTransactionOperationV1::CommitIndex => {
                if !self.selected_hunks.is_empty() {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "git index commit preview hunk selection",
                    });
                }
                self.commit_intent
                    .as_ref()
                    .ok_or(ApplicationContractError::Inconsistent {
                        field: "git index commit preview intent",
                    })?
                    .validate()?;
            }
            GitIndexTransactionOperationV1::StageHunks
            | GitIndexTransactionOperationV1::UnstageHunks => {
                if self.selected_hunks.is_empty() || self.commit_intent.is_some() {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "git index hunk preview input",
                    });
                }
            }
        }
        Ok(())
    }
}

/// Immutable application request for a preview-bound index mutation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GitIndexApplyRequestV1 {
    pub context: RequestContext,
    pub authority: AuthorityReceipt,
    pub binding: GitIndexOperationBindingV1,
    pub preview_id: GitIndexPreviewId,
    pub preview_digest: ManifestDigest,
    pub idempotency_key: IdempotencyKey,
    pub proof: GitIndexEffectProofV1,
    pub observed_at: UtcMicros,
}

impl GitIndexApplyRequestV1 {
    pub fn input_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        self.validate()?;
        Ok(canonical_sha256(&(
            GIT_INDEX_APPLY_REQUEST_DIGEST_DOMAIN_V1,
            self.context.actor(),
            self.context.scope(),
            &self.binding,
            &self.preview_id,
            &self.preview_digest,
            &self.idempotency_key,
            &self.proof.external_proof,
        ))?)
    }

    pub fn native_idempotency_key(
        &self,
    ) -> Result<GitIndexIdempotencyKey, ApplicationContractError> {
        Ok(GitIndexIdempotencyKey::new(
            self.idempotency_key.as_str().to_owned(),
        )?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_admission(
            &self.context,
            &self.authority,
            &self.binding,
            self.observed_at,
        )?;
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        self.native_idempotency_key()?;
        self.proof.validate_for(&self.authority)
    }

    /// Validate every request field that selects or authorizes an immutable
    /// preview before a daemon/native adapter may mutate repository state.
    pub fn validate_for_preview(
        &self,
        preview: &GitIndexPreviewV1,
    ) -> Result<(), ApplicationContractError> {
        self.validate()?;
        preview.validate()?;
        let scope = self.context.scope();
        if self.preview_id != preview.preview_id
            || self.preview_digest != preview.preview_digest
            || self.binding.operation != preview.operation
            || scope.project_id != preview.repository_snapshot.project_id
            || scope.repository_id != preview.repository_snapshot.repository_id
            || preview.repository_snapshot.worktree_id.as_ref() != Some(&scope.worktree_id)
            || !scope_reference_matches_snapshot(
                scope.reference.as_ref(),
                &preview.repository_snapshot,
            )
            || preview.is_expired_at(self.observed_at)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index apply preview binding",
            });
        }
        Ok(())
    }
}

/// Native port output for a completed preview pass. Unsupported state is a
/// truthful completed preview, not a transport error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitIndexPreviewPortResultV1 {
    pub preview: GitIndexPreviewV1,
    pub execution: OperationReceipt,
}

impl GitIndexPreviewPortResultV1 {
    pub fn validate_for(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<(), ApplicationContractError> {
        self.preview.validate()?;
        self.execution.validate()?;
        if self.preview.operation != request.binding.operation
            || self.preview.preview_id != request.preview_id
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index preview operation",
            });
        }
        if self.preview.repository_snapshot != request.repository_snapshot {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index preview repository snapshot binding",
            });
        }
        let requested_commit_intent_digest = request
            .commit_intent
            .as_ref()
            .map(GitIndexCommitIntentV1::compute_digest)
            .transpose()?;
        if self.preview.commit_intent_digest != requested_commit_intent_digest {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index preview commit intent binding",
            });
        }
        if self.preview.disposition.is_applicable() {
            let mut requested_hunks: Vec<_> = request
                .selected_hunks
                .iter()
                .map(tracedecay_domain::HunkRefV1::compute_digest)
                .collect::<Result<_, _>>()?;
            requested_hunks.sort_unstable();
            if requested_hunks != self.preview.selected_hunk_digests()? {
                return Err(ApplicationContractError::Inconsistent {
                    field: "git index preview selected hunk binding",
                });
            }
        }
        Ok(())
    }
}

/// Native port output for an admitted mutation or its idempotent replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitIndexApplyPortResultV1 {
    pub effect_id: EffectId,
    pub idempotency_key: IdempotencyKey,
    pub preview_digest: ManifestDigest,
    pub receipt: GitIndexTransactionReceiptV1,
    pub execution: OperationReceipt,
    pub reconciliation: ReconciliationState,
}

impl GitIndexApplyPortResultV1 {
    pub fn validate_for(
        &self,
        request: &GitIndexApplyRequestV1,
    ) -> Result<(), ApplicationContractError> {
        self.preview_digest.validate()?;
        self.receipt.validate()?;
        self.execution.validate()?;
        if self.idempotency_key != request.idempotency_key
            || self.preview_digest != request.preview_digest
            || self.receipt.preview_id != request.preview_id
            || self.receipt.operation != request.binding.operation
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index apply port binding",
            });
        }
        let expected_termination =
            effect_termination_for_result(self.receipt.outcome, self.execution.termination).ok_or(
                ApplicationContractError::Inconsistent {
                    field: "git index apply terminal outcome",
                },
            )?;
        if self.execution.termination != expected_operation_termination(expected_termination)
            || (expected_termination == EffectTermination::EffectUnknown
                && self.reconciliation != ReconciliationState::Pending)
            || (expected_termination != EffectTermination::EffectUnknown
                && self.reconciliation != ReconciliationState::Reconciled)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "git index apply terminal reconciliation",
            });
        }
        Ok(())
    }
}

/// Recovery is daemon-internal and never replays a native mutation. The
/// returned receipt must prove a terminal native state or `NeedsInspection`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitIndexRecoveryRequestV1 {
    pub repository_id: RepositoryId,
    pub transaction_id: GitIndexTransactionId,
    pub observed_at: UtcMicros,
}

/// Closed daemon/native boundary. Implementations own per-repository
/// serialization, journal fsync, fixed native Git invocation, and startup
/// recovery. No method admits arbitrary Git arguments or a free-form path.
pub trait GitIndexTransactionPort {
    fn preview(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError>;

    fn apply(
        &self,
        request: &GitIndexApplyRequestV1,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError>;

    fn recover(
        &self,
        request: &GitIndexRecoveryRequestV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexTransactionPortError>;
}

/// Stable, transport-neutral failure taxonomy for the daemon/native port.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GitIndexTransactionPortError {
    #[error("git index transaction daemon is unavailable")]
    DaemonUnavailable,
    #[error("git index transaction preview is stale, unknown, malformed, or expired")]
    StalePreview,
    #[error("git index transaction preview input expired")]
    ExpiredPreview,
    #[error("git index transaction is unsupported in the current repository state")]
    Unsupported,
    #[error("git index transaction policy proof did not admit this exact effect")]
    PolicyDenied,
    #[error("git index transaction idempotency key conflicts with a prior input")]
    IdempotencyConflict,
    #[error("git index transaction recovery is required before another mutation")]
    RecoveryRequired,
    #[error("git index transaction recovery requires user inspection")]
    NeedsInspection,
    #[error("native Git transaction failed without a success receipt")]
    NativeFailure,
}

/// Application service that projects the daemon's immutable previews and
/// durable transaction receipts through the approved effect/receipt envelope.
pub struct GitIndexTransactionService<P> {
    port: P,
}

impl<P> GitIndexTransactionService<P>
where
    P: GitIndexTransactionPort,
{
    pub fn new(port: P) -> Self {
        Self { port }
    }

    pub fn preview(
        &self,
        request: GitIndexPreviewRequestV1,
    ) -> Result<PreviewResult<GitIndexPreviewV1>, GitIndexTransactionApplicationError> {
        request.validate()?;
        let result = self.port.preview(&request)?;
        result.validate_for(&request)?;
        let preview = result.preview;
        Ok(PreviewResult::new(
            PreviewId::new(preview.preview_id.as_str().to_owned())?,
            preview.preview_digest.clone(),
            git_index_effect_class(request.binding.operation),
            request.authority,
            preview.repository_snapshot_digest.clone(),
            result.execution,
            Some(preview),
        )?)
    }

    pub fn apply(
        &self,
        request: GitIndexApplyRequestV1,
    ) -> Result<EffectResult<GitIndexTransactionReceiptV1>, GitIndexTransactionApplicationError>
    {
        request.validate()?;
        let result = self.port.apply(&request)?;
        result.validate_for(&request)?;
        let input_digest = request.input_digest()?;
        let expected_state = result.receipt.old_snapshot_digest.clone();
        let committed_state = (result.receipt.outcome == GitIndexReceiptOutcomeV1::Committed)
            .then(|| result.receipt.final_snapshot_digest.clone());
        let effect_termination =
            effect_termination_for_result(result.receipt.outcome, result.execution.termination)
                .ok_or(ApplicationContractError::Inconsistent {
                    field: "git index apply terminal outcome",
                })?;
        let receipt = EffectReceipt {
            operation: request.binding.use_case_id.clone(),
            request_id: request.context.request_id().clone(),
            actor: request.context.actor().clone(),
            scope: request.context.scope().clone(),
            effect_class: git_index_effect_class(request.binding.operation),
            idempotency_key: request.idempotency_key.clone(),
            input_digest,
            expected_state: expected_state.clone(),
            policy_digest: request.proof.policy_digest,
            configuration_digest: request.proof.configuration_digest,
            catalog_digest: request.proof.catalog_digest,
            privacy_digest: request.proof.privacy_digest,
            outcome: effect_termination,
            committed_state,
            external_proof: request.proof.external_proof,
        };
        Ok(EffectResult::new(
            result.effect_id,
            git_index_effect_class(request.binding.operation),
            result.idempotency_key,
            request.authority,
            expected_state,
            result.execution,
            result.reconciliation,
            receipt,
            Some(result.receipt),
        )?)
    }

    pub fn recover(
        &self,
        request: GitIndexRecoveryRequestV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexTransactionApplicationError> {
        request
            .repository_id
            .validate()
            .map_err(ApplicationContractError::from)?;
        request
            .transaction_id
            .validate()
            .map_err(ApplicationContractError::from)?;
        let receipt = self.port.recover(&request)?;
        receipt.validate().map_err(ApplicationContractError::from)?;
        Ok(receipt)
    }
}

#[derive(Debug, Error)]
pub enum GitIndexTransactionApplicationError {
    #[error(transparent)]
    Contract(#[from] ApplicationContractError),
    #[error(transparent)]
    Port(#[from] GitIndexTransactionPortError),
}

fn validate_admission(
    context: &RequestContext,
    authority: &AuthorityReceipt,
    binding: &GitIndexOperationBindingV1,
    observed_at: UtcMicros,
) -> Result<(), ApplicationContractError> {
    context.validate()?;
    authority.validate_for(context.scope())?;
    binding.validate()?;
    if context.admission_at(observed_at) != RequestAdmission::Admitted {
        return Err(ApplicationContractError::Inconsistent {
            field: "git index transaction admission",
        });
    }
    if !context.allows(&binding.capability_id, &binding.use_case_id) {
        return Err(ApplicationContractError::Inconsistent {
            field: "git index transaction capability binding",
        });
    }
    Ok(())
}

pub(super) fn scope_reference_matches_snapshot(
    reference: Option<&tracedecay_domain::RefId>,
    snapshot: &RepositoryStateSnapshotV1,
) -> bool {
    match (reference, &snapshot.head) {
        (Some(reference), tracedecay_domain::GitHeadStateV1::Attached { branch, .. }) => {
            reference.as_str() == branch
        }
        (Some(reference), tracedecay_domain::GitHeadStateV1::Unborn { branch }) => {
            reference.as_str() == branch
        }
        (None, tracedecay_domain::GitHeadStateV1::Detached { .. }) => true,
        (None, _) | (Some(_), tracedecay_domain::GitHeadStateV1::Detached { .. }) => false,
    }
}

const fn expected_operation_termination(termination: EffectTermination) -> OperationTermination {
    match termination {
        EffectTermination::Completed => OperationTermination::Completed,
        EffectTermination::Cancelled => OperationTermination::Cancelled,
        EffectTermination::TimedOut => OperationTermination::TimedOut,
        EffectTermination::Failed => OperationTermination::Failed,
        EffectTermination::Partial => OperationTermination::Partial,
        EffectTermination::EffectUnknown => OperationTermination::EffectUnknown,
    }
}

const fn effect_termination_for_result(
    outcome: GitIndexReceiptOutcomeV1,
    execution: OperationTermination,
) -> Option<EffectTermination> {
    match (outcome, execution) {
        (GitIndexReceiptOutcomeV1::Committed, OperationTermination::Completed) => {
            Some(EffectTermination::Completed)
        }
        (GitIndexReceiptOutcomeV1::AbortedNoChange, OperationTermination::Failed) => {
            Some(EffectTermination::Failed)
        }
        (GitIndexReceiptOutcomeV1::AbortedNoChange, OperationTermination::Cancelled) => {
            Some(EffectTermination::Cancelled)
        }
        (GitIndexReceiptOutcomeV1::AbortedNoChange, OperationTermination::TimedOut) => {
            Some(EffectTermination::TimedOut)
        }
        (GitIndexReceiptOutcomeV1::NeedsInspection, OperationTermination::EffectUnknown) => {
            Some(EffectTermination::EffectUnknown)
        }
        _ => None,
    }
}

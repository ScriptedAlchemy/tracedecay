use std::path::Path;

use tracedecay_application::{NativeIntegrationPortError, NativeIntegrationPreflightRequestV1};
use tracedecay_domain::{
    GitOidV1, GitOperationStateV1, ManifestDigest, MechanicalIntegrationModeV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewV1,
    NativeIntegrationRepositorySnapshotV1, NativeIntegrationSelectionV1,
    NativeIntegrationUnavailabilityV1, ProjectId, RepositoryId, canonical_sha256,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git_repository::{
    GitNativeIntegrationMode, GitNativePreflightDisposition, GitNativeUnsupportedReason,
    GitRepositoryAuthority,
};
use tracedecay_store::NativeIntegrationRecordV1;

use super::{
    NativeApplyEffectV1, NativeIntegrationMechanics, NativeIntegrationProbeV1, domain_error,
    native_error,
};

const ADAPTER_REVISION: &str = "gix-native-integration-v1";

/// One enrolled repository adapter. The root is supplied only at trusted
/// composition; requests cannot replace it.
pub struct GixNativeIntegrationAdapter {
    project_id: ProjectId,
    repository_id: RepositoryId,
    repository: GitRepositoryAuthority,
}

impl GixNativeIntegrationAdapter {
    pub fn open(
        project_id: ProjectId,
        repository_id: RepositoryId,
        enrolled_repository_root: &Path,
    ) -> Result<Self, NativeIntegrationPortError> {
        project_id.validate().map_err(domain_error)?;
        repository_id.validate().map_err(domain_error)?;
        let repository =
            GitRepositoryAuthority::discover(enrolled_repository_root).map_err(native_error)?;
        Ok(Self {
            project_id,
            repository_id,
            repository,
        })
    }

    fn live_probe(
        &self,
        record: &NativeIntegrationRecordV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        let preview = &record.preview;
        let live_tip = self
            .repository
            .exact_reference_tip(preview.repository_snapshot.destination_ref.as_str())
            .map_err(native_error)?;
        let live_tree = self.commit_tree(&live_tip)?;
        let (index_digest, worktree_digest) = self.live_status_digests()?;
        if index_digest != preview.repository_snapshot.index_digest
            || worktree_digest != preview.repository_snapshot.worktree_digest
        {
            return Ok(NativeIntegrationProbeV1::Diverged {
                tip: live_tip,
                tree: live_tree,
                index_digest,
                worktree_digest,
            });
        }
        if live_tip == preview.repository_snapshot.destination_tip
            && live_tree == preview.repository_snapshot.destination_tree
        {
            return Ok(NativeIntegrationProbeV1::OldState {
                tip: live_tip,
                tree: live_tree,
                index_digest,
                worktree_digest,
            });
        }
        if preview.candidate_tree.as_ref() == Some(&live_tree) {
            return Ok(NativeIntegrationProbeV1::CommittedState {
                tip: live_tip,
                tree: live_tree,
                index_digest,
                worktree_digest,
            });
        }
        Ok(NativeIntegrationProbeV1::Diverged {
            tip: live_tip,
            tree: live_tree,
            index_digest,
            worktree_digest,
        })
    }

    fn commit_tree(&self, commit: &GitOidV1) -> Result<GitOidV1, NativeIntegrationPortError> {
        self.repository.commit_tree(commit).map_err(native_error)
    }

    fn live_status_digests(
        &self,
    ) -> Result<(ManifestDigest, ManifestDigest), NativeIntegrationPortError> {
        let status = self.repository.status().map_err(native_error)?;
        let digest = canonical_sha256(&(
            &status.head,
            status.operation,
            &status.entries,
            &status.degradations,
        ))
        .map_err(domain_error)?;
        Ok((digest.clone(), digest))
    }
}

impl NativeIntegrationMechanics for GixNativeIntegrationAdapter {
    fn preflight(
        &self,
        selection: &NativeIntegrationSelectionV1,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<NativeIntegrationPreviewV1, NativeIntegrationPortError> {
        selection.validate().map_err(domain_error)?;
        if selection.project_id().map_err(domain_error)? != &self.project_id
            || selection.repository_id().map_err(domain_error)? != &self.repository_id
        {
            return Err(NativeIntegrationPortError::Denied);
        }
        let source_ref = selection.source_ref().map_err(domain_error)?;
        let destination_ref = selection.destination_ref().map_err(domain_error)?;
        let source_tip = selection.source_tip().map_err(domain_error)?;
        let destination_tip = selection.destination_tip().map_err(domain_error)?;
        let mode = request
            .preferred_mode
            .unwrap_or(MechanicalIntegrationModeV1::TwoParentMerge);
        let native_mode = native_mode(mode);
        let native = self
            .repository
            .preflight_native_integration(
                source_ref.as_str(),
                destination_ref.as_str(),
                &source_tip,
                &destination_tip,
                native_mode,
                cancellation,
            )
            .map_err(native_error)?;
        let status = self.repository.status().map_err(native_error)?;
        let references = self.repository.references().map_err(native_error)?;
        let refs_digest = canonical_sha256(
            &references
                .iter()
                .map(|reference| {
                    (
                        reference.name.as_str(),
                        reference.target.as_ref(),
                        reference.symbolic_target.as_deref(),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .map_err(domain_error)?;
        let status_digest = canonical_sha256(&(
            &status.head,
            status.operation,
            &status.entries,
            &status.degradations,
        ))
        .map_err(domain_error)?;
        let attributes_digest = canonical_sha256(&(
            ADAPTER_REVISION,
            native.candidate_tree.as_ref(),
            &refs_digest,
            &status_digest,
        ))
        .map_err(domain_error)?;
        let clean = status
            .entries
            .iter()
            .all(|entry| matches!(entry, tracedecay_domain::GitStatusEntryV1::Ignored { .. }));
        let repository_snapshot = NativeIntegrationRepositorySnapshotV1 {
            project_id: self.project_id.clone(),
            repository_id: self.repository_id.clone(),
            source_worktree_id: selection
                .source_worktree_id()
                .map_err(domain_error)?
                .cloned(),
            destination_worktree_id: selection
                .destination_worktree_id()
                .map_err(domain_error)?
                .cloned(),
            source_ref: source_ref.clone(),
            destination_ref: destination_ref.clone(),
            source_tip: native.source_tip.clone(),
            destination_tip: native.destination_tip.clone(),
            source_tree: native.source_tree.clone(),
            destination_tree: native.destination_tree.clone(),
            merge_base: native.merge_base.clone(),
            dependency_commits: native.ordered_commits.clone(),
            destination_head: status.head,
            refs_digest,
            index_digest: status_digest.clone(),
            worktree_digest: status_digest,
            attributes_digest,
            operation_state: status.operation,
            clean,
            object_format: self.repository.object_format().map_err(native_error)?,
            adapter_revision: ADAPTER_REVISION.to_owned(),
            captured_at: request.observed_at,
            digest: placeholder_digest()?,
        }
        .seal()
        .map_err(domain_error)?;

        let destination_occupied = selection
            .destination_worktree_id()
            .map_err(domain_error)?
            .is_some();
        let disposition = if !repository_snapshot.clean
            || repository_snapshot.operation_state != GitOperationStateV1::None
        {
            NativeIntegrationPreviewDispositionV1::Unavailable {
                reason: NativeIntegrationUnavailabilityV1::NativeStateUnavailable,
            }
        } else if destination_occupied {
            NativeIntegrationPreviewDispositionV1::Partial {
                reason: NativeIntegrationUnavailabilityV1::DestinationOccupied,
            }
        } else {
            match native.disposition {
                GitNativePreflightDisposition::Eligible => {
                    NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(mode)
                }
                GitNativePreflightDisposition::AlreadyIntegrated => {
                    NativeIntegrationPreviewDispositionV1::AlreadyIntegrated
                }
                GitNativePreflightDisposition::Conflict => {
                    NativeIntegrationPreviewDispositionV1::NativeConflict {
                        conflict_digest: canonical_sha256(&(
                            &repository_snapshot.digest,
                            mode,
                            "native-conflict",
                        ))
                        .map_err(domain_error)?,
                    }
                }
                GitNativePreflightDisposition::Unsupported(reason) => {
                    NativeIntegrationPreviewDispositionV1::Unavailable {
                        reason: match reason {
                            GitNativeUnsupportedReason::SigningRequired => {
                                NativeIntegrationUnavailabilityV1::SigningRequired
                            }
                            GitNativeUnsupportedReason::HooksConfigured => {
                                NativeIntegrationUnavailabilityV1::UnsupportedHooks
                            }
                            GitNativeUnsupportedReason::NonFastForward
                            | GitNativeUnsupportedReason::RootCommit
                            | GitNativeUnsupportedReason::MergeCommit => {
                                NativeIntegrationUnavailabilityV1::NativeStateUnavailable
                            }
                        },
                    }
                }
            }
        };
        let candidate_tree = matches!(
            disposition,
            NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
        )
        .then(|| native.candidate_tree)
        .flatten();
        NativeIntegrationPreviewV1 {
            preview_id: request.preview_id.clone(),
            selection: selection.clone(),
            repository_snapshot,
            grant_digest: request.topology.grant_digest.clone(),
            policy_digest: request.topology.policy_digest.clone(),
            graph_revision_digest: request.evidence.graph_revision_digest.clone(),
            test_revision_digest: request.evidence.test_revision_digest.clone(),
            schema_revision_digest: request.evidence.schema_revision_digest.clone(),
            migration_revision_digest: request.evidence.migration_revision_digest.clone(),
            disposition,
            candidate_tree,
            ordered_commits: native.ordered_commits,
            created_at: request.observed_at,
            expires_at: request.preview_expires_at,
            preview_digest: placeholder_digest()?,
        }
        .seal()
        .map_err(domain_error)
    }

    fn apply(
        &self,
        preview: &NativeIntegrationPreviewV1,
        cancellation: &CancellationToken,
    ) -> Result<NativeApplyEffectV1, NativeIntegrationPortError> {
        preview.validate().map_err(domain_error)?;
        if preview
            .repository_snapshot
            .destination_worktree_id
            .is_some()
        {
            return Ok(NativeApplyEffectV1::FailedNoChange);
        }
        let NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(mode) =
            preview.disposition
        else {
            return Ok(NativeApplyEffectV1::FailedNoChange);
        };
        let Some(candidate_tree) = &preview.candidate_tree else {
            return Ok(NativeApplyEffectV1::FailedNoChange);
        };
        match self.repository.apply_native_integration(
            preview.repository_snapshot.source_ref.as_str(),
            preview.repository_snapshot.destination_ref.as_str(),
            &preview.repository_snapshot.source_tip,
            &preview.repository_snapshot.destination_tip,
            candidate_tree,
            native_mode(mode),
            cancellation,
        ) {
            Ok(outcome) => {
                let (final_index_digest, final_worktree_digest) = self.live_status_digests()?;
                if final_index_digest != preview.repository_snapshot.index_digest
                    || final_worktree_digest != preview.repository_snapshot.worktree_digest
                {
                    return Ok(NativeApplyEffectV1::UnknownAfterCommitPoint {
                        candidate_tip: Some(outcome.new_tip),
                    });
                }
                Ok(NativeApplyEffectV1::Committed {
                    new_tip: outcome.new_tip,
                    final_tree: outcome.final_tree,
                    final_index_digest,
                    final_worktree_digest,
                })
            }
            Err(_error) => {
                let probe = self.probe_from_preview(preview)?;
                if matches!(probe, NativeIntegrationProbeV1::OldState { .. }) {
                    Ok(NativeApplyEffectV1::FailedNoChange)
                } else {
                    Ok(NativeApplyEffectV1::UnknownAfterCommitPoint {
                        candidate_tip: match probe {
                            NativeIntegrationProbeV1::CommittedState { tip, .. } => Some(tip),
                            _ => None,
                        },
                    })
                }
            }
        }
    }

    fn probe(
        &self,
        record: &NativeIntegrationRecordV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        self.live_probe(record)
    }

    fn rollback(
        &self,
        record: &NativeIntegrationRecordV1,
        committed_tip: &GitOidV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        self.repository
            .rollback_native_integration(
                record.preview.repository_snapshot.destination_ref.as_str(),
                committed_tip,
                &record.preview.repository_snapshot.destination_tip,
            )
            .map_err(native_error)?;
        self.live_probe(record)
    }
}

impl GixNativeIntegrationAdapter {
    fn probe_from_preview(
        &self,
        preview: &NativeIntegrationPreviewV1,
    ) -> Result<NativeIntegrationProbeV1, NativeIntegrationPortError> {
        let live_tip = self
            .repository
            .exact_reference_tip(preview.repository_snapshot.destination_ref.as_str())
            .map_err(native_error)?;
        let live_tree = self.commit_tree(&live_tip)?;
        let (index_digest, worktree_digest) = self.live_status_digests()?;
        if index_digest != preview.repository_snapshot.index_digest
            || worktree_digest != preview.repository_snapshot.worktree_digest
        {
            return Ok(NativeIntegrationProbeV1::Diverged {
                tip: live_tip,
                tree: live_tree,
                index_digest,
                worktree_digest,
            });
        }
        if live_tip == preview.repository_snapshot.destination_tip
            && live_tree == preview.repository_snapshot.destination_tree
        {
            return Ok(NativeIntegrationProbeV1::OldState {
                tip: live_tip,
                tree: live_tree,
                index_digest,
                worktree_digest,
            });
        }
        if preview.candidate_tree.as_ref() == Some(&live_tree) {
            return Ok(NativeIntegrationProbeV1::CommittedState {
                tip: live_tip,
                tree: live_tree,
                index_digest,
                worktree_digest,
            });
        }
        Ok(NativeIntegrationProbeV1::Diverged {
            tip: live_tip,
            tree: live_tree,
            index_digest,
            worktree_digest,
        })
    }
}

const fn native_mode(mode: MechanicalIntegrationModeV1) -> GitNativeIntegrationMode {
    match mode {
        MechanicalIntegrationModeV1::FastForward => GitNativeIntegrationMode::FastForward,
        MechanicalIntegrationModeV1::TwoParentMerge => GitNativeIntegrationMode::TwoParentMerge,
        MechanicalIntegrationModeV1::CherryPickExactCommits => {
            GitNativeIntegrationMode::CherryPickExactCommits
        }
    }
}

fn placeholder_digest() -> Result<ManifestDigest, NativeIntegrationPortError> {
    canonical_sha256(&"pending native integration value").map_err(domain_error)
}

//! Durable, content-free acquisition queue records for canonical sources.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{
    DomainError, LocatorDigest, ManifestDigest, ProviderId, SourceBindingIdentityV1,
    SourceBindingV1, SourceDefinitionV1, SourceEventAdmissionReceiptV1, SourceEventKeyV1,
    SourceRefreshCauseV1, SourceRefreshReceiptV1, SourceWholeRootStageV1, UtcMicros,
    canonical_sha256,
};

pub const MAX_SOURCE_ACQUISITION_ATTEMPTS_V1: u32 = 16;
pub const MAX_SOURCE_ACQUISITION_RECEIPTS_V1: usize = 1_024;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SourceAcquisitionQueueContractErrorV1 {
    #[error("external-source acquisition queue domain value is invalid")]
    Domain,
    #[error("external-source acquisition queue state is inconsistent")]
    Inconsistent,
}

impl From<DomainError> for SourceAcquisitionQueueContractErrorV1 {
    fn from(_error: DomainError) -> Self {
        Self::Domain
    }
}

pub type SourceAcquisitionQueueResultV1<T> = Result<T, SourceAcquisitionQueueContractErrorV1>;

/// Exact, content-free provider request authority persisted with a scheduled
/// acquisition. A worker must reconstruct its provider read from this value;
/// mutable hook or project-open state is never a substitute.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum SourceAcquisitionRequestV1 {
    GitHubReview {
        provider: ProviderId,
        configured_source: LocatorDigest,
        scope: FeedbackScopeV1,
        operation: GitHubReviewReadOperationV1,
        pull_request_id: GitHubPullRequestIdV1,
        request_digest: ManifestDigest,
    },
}

impl SourceAcquisitionRequestV1 {
    pub fn github_review(
        provider: ProviderId,
        configured_source: LocatorDigest,
        scope: FeedbackScopeV1,
        operation: GitHubReviewReadOperationV1,
        pull_request_id: GitHubPullRequestIdV1,
    ) -> SourceAcquisitionQueueResultV1<Self> {
        let request_digest = canonical_sha256(&(
            "tracedecay.external-source.github-review-request.v1",
            &provider,
            &configured_source,
            &scope,
            operation,
            &pull_request_id,
        ))?;
        let request = Self::GitHubReview {
            provider,
            configured_source,
            scope,
            operation,
            pull_request_id,
            request_digest,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn provider(&self) -> &ProviderId {
        match self {
            Self::GitHubReview { provider, .. } => provider,
        }
    }

    pub fn configured_source(&self) -> &LocatorDigest {
        match self {
            Self::GitHubReview {
                configured_source, ..
            } => configured_source,
        }
    }

    pub fn request_digest(&self) -> &ManifestDigest {
        match self {
            Self::GitHubReview { request_digest, .. } => request_digest,
        }
    }

    /// Binding locator for this exact request, not merely its repository
    /// configuration. Distinct refs, heads, worktrees, or pull requests
    /// therefore cannot share a queue row or event receipt.
    pub fn binding_native_root(&self) -> SourceAcquisitionQueueResultV1<LocatorDigest> {
        self.validate()?;
        LocatorDigest::new(
            canonical_sha256(&(
                "tracedecay.external-source.request-binding.v1",
                self.configured_source(),
                self.request_digest(),
            ))?
            .as_str(),
        )
        .map_err(SourceAcquisitionQueueContractErrorV1::from)
    }

    pub fn validate(&self) -> SourceAcquisitionQueueResultV1<()> {
        match self {
            Self::GitHubReview {
                provider,
                configured_source,
                scope,
                operation,
                pull_request_id,
                request_digest,
            } => {
                provider.validate()?;
                configured_source.validate()?;
                scope.validate()?;
                pull_request_id.validate()?;
                request_digest.validate()?;
                if !operation.is_read_only()
                    || canonical_sha256(&(
                        "tracedecay.external-source.github-review-request.v1",
                        provider,
                        configured_source,
                        scope,
                        operation,
                        pull_request_id,
                    ))? != *request_digest
                {
                    return Err(SourceAcquisitionQueueContractErrorV1::Inconsistent);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceScheduledRefetchV1 {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    request: SourceAcquisitionRequestV1,
    event_receipt: SourceEventAdmissionReceiptV1,
    whole_root_stage: Option<SourceWholeRootStageV1>,
    attempt: u32,
    not_before: UtcMicros,
}

impl SourceScheduledRefetchV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        request: SourceAcquisitionRequestV1,
        event_receipt: SourceEventAdmissionReceiptV1,
        whole_root_stage: Option<SourceWholeRootStageV1>,
        attempt: u32,
        not_before: UtcMicros,
    ) -> SourceAcquisitionQueueResultV1<Self> {
        let scheduled = Self {
            definition,
            binding,
            request,
            event_receipt,
            whole_root_stage,
            attempt,
            not_before,
        };
        scheduled.validate()?;
        Ok(scheduled)
    }

    pub fn definition(&self) -> &SourceDefinitionV1 {
        &self.definition
    }

    pub fn binding(&self) -> &SourceBindingV1 {
        &self.binding
    }

    pub fn request(&self) -> &SourceAcquisitionRequestV1 {
        &self.request
    }

    pub fn event_receipt(&self) -> &SourceEventAdmissionReceiptV1 {
        &self.event_receipt
    }

    pub fn refresh(&self) -> &SourceRefreshReceiptV1 {
        self.event_receipt.original_refresh()
    }

    pub fn whole_root_stage(&self) -> Option<&SourceWholeRootStageV1> {
        self.whole_root_stage.as_ref()
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn not_before(&self) -> UtcMicros {
        self.not_before
    }

    pub fn with_retry(
        &self,
        attempt: u32,
        not_before: UtcMicros,
    ) -> SourceAcquisitionQueueResultV1<Self> {
        Self::new(
            self.definition.clone(),
            self.binding.clone(),
            self.request.clone(),
            self.event_receipt.clone(),
            self.whole_root_stage.clone(),
            attempt,
            not_before,
        )
    }

    pub fn with_whole_root_stage(
        &self,
        stage: Option<SourceWholeRootStageV1>,
        not_before: UtcMicros,
    ) -> SourceAcquisitionQueueResultV1<Self> {
        Self::new(
            self.definition.clone(),
            self.binding.clone(),
            self.request.clone(),
            self.event_receipt.clone(),
            stage,
            0,
            not_before,
        )
    }

    pub fn validate(&self) -> SourceAcquisitionQueueResultV1<()> {
        self.definition.validate()?;
        self.binding.validate_against(&self.definition)?;
        self.request.validate()?;
        self.event_receipt.validate()?;
        self.whole_root_stage
            .as_ref()
            .map_or(Ok(()), SourceWholeRootStageV1::validate)?;
        let identity = self.binding.immutable_identity()?;
        if self.request.provider() != &self.definition.provider
            || self.request.binding_native_root()? != self.binding.native_root
            || self.event_receipt.binding() != &identity
            || self.event_receipt.original_refresh().cause() != SourceRefreshCauseV1::Event
            || self.attempt > MAX_SOURCE_ACQUISITION_ATTEMPTS_V1
        {
            return Err(SourceAcquisitionQueueContractErrorV1::Inconsistent);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAcquisitionQueueStateV1 {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    active: Option<SourceScheduledRefetchV1>,
    successor: Option<SourceScheduledRefetchV1>,
    receipts: BTreeMap<SourceEventKeyV1, SourceEventAdmissionReceiptV1>,
    state_digest: ManifestDigest,
}

impl SourceAcquisitionQueueStateV1 {
    pub fn new(
        definition: SourceDefinitionV1,
        binding: SourceBindingV1,
        active: Option<SourceScheduledRefetchV1>,
        successor: Option<SourceScheduledRefetchV1>,
        receipts: BTreeMap<SourceEventKeyV1, SourceEventAdmissionReceiptV1>,
    ) -> SourceAcquisitionQueueResultV1<Self> {
        let state_digest = canonical_sha256(&(
            "tracedecay.external-source.acquisition-queue.v1",
            &definition,
            &binding,
            &active,
            &successor,
            &receipts,
        ))?;
        let state = Self {
            definition,
            binding,
            active,
            successor,
            receipts,
            state_digest,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn definition(&self) -> &SourceDefinitionV1 {
        &self.definition
    }

    pub fn binding(&self) -> &SourceBindingV1 {
        &self.binding
    }

    pub fn state_digest(&self) -> &ManifestDigest {
        &self.state_digest
    }

    pub fn binding_identity(&self) -> SourceAcquisitionQueueResultV1<SourceBindingIdentityV1> {
        self.binding
            .immutable_identity()
            .map_err(SourceAcquisitionQueueContractErrorV1::from)
    }

    pub fn active(&self) -> Option<&SourceScheduledRefetchV1> {
        self.active.as_ref()
    }

    pub fn successor(&self) -> Option<&SourceScheduledRefetchV1> {
        self.successor.as_ref()
    }

    pub fn receipt(&self, event: &SourceEventKeyV1) -> Option<&SourceEventAdmissionReceiptV1> {
        self.receipts.get(event)
    }

    pub fn receipts(&self) -> &BTreeMap<SourceEventKeyV1, SourceEventAdmissionReceiptV1> {
        &self.receipts
    }

    pub fn is_ready(&self, now: UtcMicros) -> bool {
        self.active
            .as_ref()
            .is_some_and(|task| task.not_before().0 <= now.0)
    }

    pub fn with_schedule(
        &self,
        active: Option<SourceScheduledRefetchV1>,
        successor: Option<SourceScheduledRefetchV1>,
    ) -> SourceAcquisitionQueueResultV1<Self> {
        Self::new(
            self.definition.clone(),
            self.binding.clone(),
            active,
            successor,
            self.receipts.clone(),
        )
    }

    pub fn validate(&self) -> SourceAcquisitionQueueResultV1<()> {
        self.definition.validate()?;
        self.binding.validate_against(&self.definition)?;
        let identity = self.binding_identity()?;
        for scheduled in [self.active.as_ref(), self.successor.as_ref()]
            .into_iter()
            .flatten()
        {
            scheduled.validate()?;
            if scheduled.binding().immutable_identity().ok().as_ref() != Some(&identity)
                || scheduled.definition() != &self.definition
            {
                return Err(SourceAcquisitionQueueContractErrorV1::Inconsistent);
            }
        }
        if self.successor.is_some() && self.active.is_none() {
            return Err(SourceAcquisitionQueueContractErrorV1::Inconsistent);
        }
        if self.receipts.len() > MAX_SOURCE_ACQUISITION_RECEIPTS_V1 {
            return Err(SourceAcquisitionQueueContractErrorV1::Inconsistent);
        }
        for (event_key, receipt) in &self.receipts {
            receipt.validate()?;
            if receipt.binding() != &identity || receipt.event_key() != event_key {
                return Err(SourceAcquisitionQueueContractErrorV1::Inconsistent);
            }
        }
        let expected = canonical_sha256(&(
            "tracedecay.external-source.acquisition-queue.v1",
            &self.definition,
            &self.binding,
            &self.active,
            &self.successor,
            &self.receipts,
        ))?;
        if expected != self.state_digest {
            return Err(SourceAcquisitionQueueContractErrorV1::Inconsistent);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAcquisitionQueueCasV1 {
    binding: SourceBindingIdentityV1,
    expected_state_digest: Option<ManifestDigest>,
    next: SourceAcquisitionQueueStateV1,
}

impl SourceAcquisitionQueueCasV1 {
    pub fn new(
        binding: SourceBindingIdentityV1,
        expected_state_digest: Option<ManifestDigest>,
        next: SourceAcquisitionQueueStateV1,
    ) -> SourceAcquisitionQueueResultV1<Self> {
        let command = Self {
            binding,
            expected_state_digest,
            next,
        };
        command.validate()?;
        Ok(command)
    }

    pub fn binding(&self) -> &SourceBindingIdentityV1 {
        &self.binding
    }

    pub fn expected_state_digest(&self) -> Option<&ManifestDigest> {
        self.expected_state_digest.as_ref()
    }

    pub fn next(&self) -> &SourceAcquisitionQueueStateV1 {
        &self.next
    }

    pub fn validate(&self) -> SourceAcquisitionQueueResultV1<()> {
        self.binding.validate()?;
        self.expected_state_digest
            .as_ref()
            .map_or(Ok(()), ManifestDigest::validate)?;
        self.next.validate()?;
        if self.next.binding_identity()? != self.binding {
            return Err(SourceAcquisitionQueueContractErrorV1::Inconsistent);
        }
        Ok(())
    }
}

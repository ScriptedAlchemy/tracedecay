use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ActorId, BrainId, ManifestDigest, UserProfileId, UtcMicros, WorkGraphVersionV1,
    WorkProductEventSequenceV1, WorkProductSourceWatermarkV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::{CancellationContext, Deadline, RequestContext, RequestId};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProductApplicationErrorV1 {
    #[error("Work operation is not authorized")]
    NotAuthorized,
    #[error("Work operation was cancelled")]
    Cancelled,
    #[error("Work operation timed out")]
    TimedOut,
    #[error("Work resource was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work graph version changed")]
    VersionConflict,
    #[error("Work policy, configuration, or catalog revision changed")]
    RevisionConflict,
    #[error("Work idempotency key was reused with different input")]
    IdempotencyConflict,
    #[error("Work request is invalid")]
    InvalidRequest,
    /// The selection covers only part of the owner's journal, so there is no
    /// head to prepare a mutation against.
    ///
    /// A read may answer over the covered slice and disclose the rest, because
    /// a truthful partial reading is still a reading. A mutation may not: it
    /// pins the head it read as its expected version, and under partial
    /// coverage that head is the covered slice's head, not the journal's. A
    /// change prepared against it would be reasoning from a graph that is not
    /// current, and the append would fail its compare-and-swap for a reason
    /// that names the wrong cause. The remedy is in the message because it is
    /// actionable: widen the selection to the relation scopes the excluded
    /// events were admitted under.
    #[error(
        "Work selection covers only part of the owner's journal, so no mutation can be \
         prepared against it; widen the selection to the relation scopes the excluded \
         events were admitted under"
    )]
    SelectionCoverageIncomplete,
    #[error("Work event authority is unavailable")]
    EventAuthorityUnavailable,
    #[error("Verified Work graph authority is unavailable")]
    GraphAuthorityUnavailable,
    #[error("Work evidence authority is unavailable")]
    EvidenceAuthorityUnavailable,
    #[error("Work evidence continuation is stale")]
    EvidenceContinuationStale,
    #[error("Work proposal authority is unavailable")]
    ProposalAuthorityUnavailable,
}

pub use tracedecay_domain::WorkProductAuthorizedRelationScopeV1 as WorkRelationScopeV1;
pub use tracedecay_domain::WorkProductSelectionScopeV1;

/// Owner identity resolved by the registered profile authority. It is never
/// accepted from a Work request.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedWorkProductScopeV1 {
    owner_brain_id: BrainId,
    owner_profile_id: UserProfileId,
    selection: WorkProductSelectionScopeV1,
}

impl AuthorizedWorkProductScopeV1 {
    pub fn new(
        owner_brain_id: BrainId,
        owner_profile_id: UserProfileId,
        selection: WorkProductSelectionScopeV1,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        owner_brain_id
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        owner_profile_id
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        selection
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        Ok(Self {
            owner_brain_id,
            owner_profile_id,
            selection,
        })
    }

    pub const fn owner_brain_id(&self) -> &BrainId {
        &self.owner_brain_id
    }

    pub const fn owner_profile_id(&self) -> &UserProfileId {
        &self.owner_profile_id
    }

    pub const fn selection(&self) -> &WorkProductSelectionScopeV1 {
        &self.selection
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProductOwnerAuthorizationErrorV1 {
    #[error("Work profile owner or relation scope is not authorized")]
    NotAuthorized,
    #[error("Registered Work profile owner authority is unavailable")]
    Unavailable,
}

/// Resolves the registered profile owner and authorizes every selected
/// project/repository relation against the request context.
pub trait WorkProductOwnerAuthorizationPortV1: Send + Sync {
    fn authorize_scope(
        &self,
        context: &RequestContext,
        selection: &WorkProductSelectionScopeV1,
        observed_at: UtcMicros,
    ) -> Result<AuthorizedWorkProductScopeV1, WorkProductOwnerAuthorizationErrorV1>;
}

impl<A> WorkProductOwnerAuthorizationPortV1 for &A
where
    A: WorkProductOwnerAuthorizationPortV1 + ?Sized,
{
    fn authorize_scope(
        &self,
        context: &RequestContext,
        selection: &WorkProductSelectionScopeV1,
        observed_at: UtcMicros,
    ) -> Result<AuthorizedWorkProductScopeV1, WorkProductOwnerAuthorizationErrorV1> {
        (**self).authorize_scope(context, selection, observed_at)
    }
}

/// Canonical catalog binding metadata injected by composition.
///
/// This module deliberately owns no operation enum or local binding registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkProductBindingV1 {
    capability_id: CapabilityId,
    use_case_id: UseCaseId,
}

impl WorkProductBindingV1 {
    pub const fn new(capability_id: CapabilityId, use_case_id: UseCaseId) -> Self {
        Self {
            capability_id,
            use_case_id,
        }
    }

    pub const fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub const fn use_case_id(&self) -> &UseCaseId {
        &self.use_case_id
    }
}

/// One exact verified graph snapshot identity.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedWorkGraphVersionV1 {
    graph_version: WorkGraphVersionV1,
    event_sequence: WorkProductEventSequenceV1,
    source_watermark: WorkProductSourceWatermarkV1,
    recovered_graph_digest: ManifestDigest,
}

impl VerifiedWorkGraphVersionV1 {
    pub fn new(
        graph_version: WorkGraphVersionV1,
        event_sequence: WorkProductEventSequenceV1,
        source_watermark: WorkProductSourceWatermarkV1,
        recovered_graph_digest: ManifestDigest,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        recovered_graph_digest
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        Ok(Self {
            graph_version,
            event_sequence,
            source_watermark,
            recovered_graph_digest,
        })
    }

    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub const fn source_watermark(&self) -> &WorkProductSourceWatermarkV1 {
        &self.source_watermark
    }

    pub const fn event_sequence(&self) -> WorkProductEventSequenceV1 {
        self.event_sequence
    }

    pub const fn recovered_graph_digest(&self) -> &ManifestDigest {
        &self.recovered_graph_digest
    }
}

/// Admission state forwarded to each Work port. This keeps cancellation and
/// deadline identities intact without leaking a transport or database type.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductPortContextV1 {
    actor: ActorId,
    request_id: RequestId,
    deadline: Deadline,
    cancellation: CancellationContext,
    authorized_scope: AuthorizedWorkProductScopeV1,
    observed_at: UtcMicros,
}

impl WorkProductPortContextV1 {
    pub(crate) fn from_request(
        context: &RequestContext,
        authorized_scope: AuthorizedWorkProductScopeV1,
        observed_at: UtcMicros,
    ) -> Self {
        Self {
            actor: context.actor().clone(),
            request_id: context.request_id().clone(),
            deadline: context.deadline().clone(),
            cancellation: context.cancellation().clone(),
            authorized_scope,
            observed_at,
        }
    }

    pub const fn actor(&self) -> &ActorId {
        &self.actor
    }

    pub const fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub const fn deadline(&self) -> &Deadline {
        &self.deadline
    }

    pub const fn cancellation(&self) -> &CancellationContext {
        &self.cancellation
    }

    pub const fn authorized_scope(&self) -> &AuthorizedWorkProductScopeV1 {
        &self.authorized_scope
    }

    pub const fn observed_at(&self) -> UtcMicros {
        self.observed_at
    }
}

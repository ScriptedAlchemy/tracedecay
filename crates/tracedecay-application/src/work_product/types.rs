use std::collections::BTreeSet;

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
    #[error("Work idempotency key was reused with different input")]
    IdempotencyConflict,
    #[error("Work request is invalid")]
    InvalidRequest,
    #[error("Work event authority is unavailable")]
    EventAuthorityUnavailable,
    #[error("Verified Work graph authority is unavailable")]
    GraphAuthorityUnavailable,
    #[error("Work evidence authority is unavailable")]
    EvidenceAuthorityUnavailable,
    #[error("Work proposal authority is unavailable")]
    ProposalAuthorityUnavailable,
    #[error("Work graph publication is pending reconciliation")]
    ReconciliationRequired,
}

pub use tracedecay_domain::WorkProductAuthorizedRelationScopeV1 as WorkRelationScopeV1;

/// The relation subset selected for one request. `ProfileOwnedNoGit` is an
/// explicit no-Git selection, not an empty set that bypasses authorization.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum WorkProductSelectionScopeV1 {
    ProfileOwnedNoGit,
    Relations {
        relation_scopes: BTreeSet<WorkRelationScopeV1>,
    },
}

impl WorkProductSelectionScopeV1 {
    pub fn relations(
        relation_scopes: BTreeSet<WorkRelationScopeV1>,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        if relation_scopes.is_empty() {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        Ok(Self::Relations { relation_scopes })
    }

    pub const fn relation_scopes(&self) -> Option<&BTreeSet<WorkRelationScopeV1>> {
        match self {
            Self::ProfileOwnedNoGit => None,
            Self::Relations { relation_scopes } => Some(relation_scopes),
        }
    }

    pub fn validate(&self) -> Result<(), WorkProductApplicationErrorV1> {
        if matches!(
            self,
            Self::Relations { relation_scopes } if relation_scopes.is_empty()
        ) {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        Ok(())
    }
}

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
        selection.validate()?;
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

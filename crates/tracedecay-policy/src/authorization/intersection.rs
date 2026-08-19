use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_domain::ManifestDigest;

use super::input::{
    DisclosureClassV1, PrivacyConstraintSetV1, PrivacyConstraintV1, SinkKindV1,
    SourceAuthorizationInputV1, SourceOwnerV1,
};

/// The non-expanding effective authority used only by a successful decision.
///
/// Every collection is narrowed to the requested subset. The policy crate
/// cannot turn this value into an effect; application must sink-recheck it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveSourceGrantV1 {
    pub owner: SourceOwnerV1,
    pub resources: BTreeSet<super::input::ResourceIdV1>,
    pub operations: BTreeSet<super::input::TypedOperationV1>,
    pub sinks: BTreeSet<SinkKindV1>,
    pub disclosure_ceiling: DisclosureClassV1,
    pub constraints: PrivacyConstraintSetV1,
    pub budgets: super::input::BudgetSetV1,
    pub source_grant_digest: ManifestDigest,
    pub requester_grant_digest: ManifestDigest,
}

impl EffectiveSourceGrantV1 {
    pub fn permits_requested_access(&self, input: &SourceAuthorizationInputV1) -> bool {
        self.owner == input.resolved_owner_scope.owner
            && self.resources.contains(&input.requested_access.resource)
            && self.operations.contains(&input.requested_access.operation)
            && self.sinks.contains(&input.requested_access.sink)
            && input.requested_access.disclosure <= self.disclosure_ceiling
            && self.budgets.contains(&input.requested_access.budget)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntersectionFailureV1 {
    OwnerMismatch,
    RequesterSubjectMismatch,
    ResourceNotGranted,
    OperationNotGranted,
    SinkNotGranted,
    DisclosureTooBroad,
    BudgetExceeded,
    MandatoryLocalPrivacyBlocksEgress,
    SanitizedOnlyBlocksDisclosure,
    NoModelContext,
    NoRetention,
    NoTelemetry,
    NoExport,
    SinkUnavailable,
}

pub(crate) fn intersect_source_authority(
    input: &SourceAuthorizationInputV1,
) -> Result<EffectiveSourceGrantV1, IntersectionFailureV1> {
    let source_grant = &input.source_grant;
    let requester_grant = &input.requester_grant;
    let binding_owner = input.binding.binding.owner();
    let resolved_owner = &input.resolved_owner_scope.owner;

    if &binding_owner != resolved_owner
        || &source_grant.owner != resolved_owner
        || &requester_grant.owner != resolved_owner
    {
        return Err(IntersectionFailureV1::OwnerMismatch);
    }
    if source_grant.subject != input.requester || requester_grant.subject != input.requester {
        return Err(IntersectionFailureV1::RequesterSubjectMismatch);
    }

    let resource_allowed = source_grant
        .resources
        .contains(&input.requested_access.resource)
        && requester_grant
            .resources
            .contains(&input.requested_access.resource);
    if !resource_allowed {
        return Err(IntersectionFailureV1::ResourceNotGranted);
    }
    let operation_allowed = source_grant
        .operations
        .contains(&input.requested_access.operation)
        && requester_grant
            .operations
            .contains(&input.requested_access.operation);
    if !operation_allowed {
        return Err(IntersectionFailureV1::OperationNotGranted);
    }
    let sink_allowed = source_grant.sinks.contains(&input.requested_access.sink)
        && requester_grant.sinks.contains(&input.requested_access.sink);
    if !sink_allowed {
        return Err(IntersectionFailureV1::SinkNotGranted);
    }
    if !input.sink_policy.available {
        return Err(IntersectionFailureV1::SinkUnavailable);
    }

    let disclosure_ceiling = source_grant
        .disclosure_ceiling
        .min(requester_grant.disclosure_ceiling)
        .min(input.source_policy.disclosure_ceiling)
        .min(input.sink_policy.disclosure_ceiling);
    if input.requested_access.disclosure > disclosure_ceiling {
        return Err(IntersectionFailureV1::DisclosureTooBroad);
    }

    let budgets = source_grant.budgets.pointwise_min(&requester_grant.budgets);
    if !budgets.contains(&input.requested_access.budget) {
        return Err(IntersectionFailureV1::BudgetExceeded);
    }

    let constraints = source_grant
        .constraints
        .iter()
        .chain(requester_grant.constraints.iter())
        .chain(input.source_policy.mandatory_privacy.iter())
        .chain(input.sink_policy.mandatory_privacy.iter())
        .copied()
        .collect::<PrivacyConstraintSetV1>();

    if constraints.contains(&PrivacyConstraintV1::LocalOnly)
        && input.requested_access.sink.is_egress()
    {
        return Err(IntersectionFailureV1::MandatoryLocalPrivacyBlocksEgress);
    }
    if constraints.contains(&PrivacyConstraintV1::SanitizedOnly)
        && input.requested_access.disclosure > DisclosureClassV1::SanitizedContent
    {
        return Err(IntersectionFailureV1::SanitizedOnlyBlocksDisclosure);
    }
    if constraints.contains(&PrivacyConstraintV1::NoModelContext)
        && input.requested_access.sink == SinkKindV1::ModelContext
    {
        return Err(IntersectionFailureV1::NoModelContext);
    }
    if constraints.contains(&PrivacyConstraintV1::NoRetention)
        && matches!(
            input.requested_access.sink,
            SinkKindV1::CanonicalStore | SinkKindV1::LocalDurableStore
        )
    {
        return Err(IntersectionFailureV1::NoRetention);
    }
    if constraints.contains(&PrivacyConstraintV1::NoTelemetry)
        && input.requested_access.sink == SinkKindV1::Telemetry
    {
        return Err(IntersectionFailureV1::NoTelemetry);
    }
    if constraints.contains(&PrivacyConstraintV1::NoExport)
        && input.requested_access.sink == SinkKindV1::Export
    {
        return Err(IntersectionFailureV1::NoExport);
    }

    Ok(EffectiveSourceGrantV1 {
        owner: input.resolved_owner_scope.owner.clone(),
        resources: BTreeSet::from([input.requested_access.resource.clone()]),
        operations: BTreeSet::from([input.requested_access.operation]),
        sinks: BTreeSet::from([input.requested_access.sink]),
        disclosure_ceiling: input.requested_access.disclosure,
        constraints,
        budgets: input.requested_access.budget.clone(),
        source_grant_digest: source_grant.digest.clone(),
        requester_grant_digest: requester_grant.digest.clone(),
    })
}

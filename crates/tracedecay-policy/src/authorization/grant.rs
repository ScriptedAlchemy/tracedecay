use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{ActorId, ManifestDigest, UtcMicros};

use super::input::{
    BudgetSetV1, DisclosureClassV1, GrantIdV1, PrivacyConstraintSetV1, ResourceIdV1, SinkKindV1,
    SourceOwnerV1, TypedOperationV1,
};

/// Explicit external grant record state. Policy cannot issue, renew, revoke,
/// widen, or reinterpret a grant; it only consumes this immutable input.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GrantStateV1 {
    Active,
    Revoked,
    Stale,
    Ambiguous,
}

/// Immutable authorization input issued outside this crate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGrantV1 {
    pub grant_id: GrantIdV1,
    pub issuer: ActorId,
    pub subject: ActorId,
    pub owner: SourceOwnerV1,
    pub resources: BTreeSet<ResourceIdV1>,
    pub operations: BTreeSet<TypedOperationV1>,
    pub sinks: BTreeSet<SinkKindV1>,
    pub disclosure_ceiling: DisclosureClassV1,
    pub constraints: PrivacyConstraintSetV1,
    pub budgets: BudgetSetV1,
    pub revision: u64,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub digest: ManifestDigest,
    pub state: GrantStateV1,
}

impl CapabilityGrantV1 {
    pub fn is_valid(&self) -> bool {
        self.grant_id.is_valid()
            && self.issuer.validate().is_ok()
            && self.subject.validate().is_ok()
            && self.owner.is_valid()
            && !self.resources.is_empty()
            && self.resources.iter().all(ResourceIdV1::is_valid)
            && !self.operations.is_empty()
            && !self.sinks.is_empty()
            && self.revision > 0
            && self.issued_at < self.expires_at
            && self.digest.validate().is_ok()
    }

    pub(crate) fn state_at(&self, evaluated_at: UtcMicros) -> GrantStateAtV1 {
        match self.state {
            GrantStateV1::Revoked => GrantStateAtV1::Revoked,
            GrantStateV1::Stale => GrantStateAtV1::Stale,
            GrantStateV1::Ambiguous => GrantStateAtV1::Ambiguous,
            GrantStateV1::Active if evaluated_at < self.issued_at => GrantStateAtV1::NotYetIssued,
            GrantStateV1::Active if evaluated_at >= self.expires_at => GrantStateAtV1::Expired,
            GrantStateV1::Active => GrantStateAtV1::Active,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrantStateAtV1 {
    Active,
    Revoked,
    Stale,
    Ambiguous,
    NotYetIssued,
    Expired,
}

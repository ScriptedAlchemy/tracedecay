//! Advisory concurrent-work proximity contracts.
//!
//! These types describe overlap evidence only. They grant no lease, lock,
//! scheduler admission, work assignment, or agent-continuation authority.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::code_intelligence::identity::{FileOccurrenceId, SourceSpan, SymbolOccurrenceId};
use crate::research::{DomainError, RetrievalAnchorId, UtcMicros};

use super::FeedbackScopeV1;

pub const PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1: &str = "feedback.proximity.risk_threshold";

crate::canonical_text::validated_string_newtype!(
    plain,
    DomainError,
    super::validate_label;
    ProximityContributionIdV1 => "proximity contribution id",
    ProximityObservationIdV1 => "proximity observation id",
);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProximityWarningClassV1 {
    SameFile,
    OverlappingRange,
    SameSymbol,
    SamePackage,
    SameCrate,
    Neighborhood,
    SharedCaller,
    SharedDependency,
    SharedTest,
}

impl ProximityWarningClassV1 {
    /// Same-file, overlapping-range, and same-symbol encounters are exact
    /// evidence of concurrent edits. Every other class is a relationship hint.
    pub const fn is_exact_overlap(self) -> bool {
        matches!(
            self,
            Self::SameFile | Self::OverlappingRange | Self::SameSymbol
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProximityRelationPathKindV1 {
    DirectCaller,
    TransitiveCaller,
    DirectDependency,
    TransitiveDependency,
    AffectedTest,
    PackageMembership,
    CrateMembership,
    NeighborhoodMembership,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProximityCoverageV1 {
    Complete,
    Partial,
    Stale,
    Unavailable,
    Denied,
    Private,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProximityInclusionV1 {
    Included,
    BelowThreshold,
    SuppressedDuplicate,
    Stale,
    Denied,
    Private,
}

/// A privacy-scoped code address. It identifies the coarse changed-code shape
/// but carries no other actor, session, or private-source content.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProximityAddressV1 {
    pub scope: FeedbackScopeV1,
    pub file: FileOccurrenceId,
    pub span: Option<SourceSpan>,
    pub symbol: Option<SymbolOccurrenceId>,
}

impl ProximityAddressV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.scope.validate()?;
        self.file.validate()?;
        self.span.as_ref().map_or(Ok(()), SourceSpan::validate)?;
        self.symbol
            .as_ref()
            .map_or(Ok(()), SymbolOccurrenceId::validate)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProximityRelationPathV1 {
    pub kind: ProximityRelationPathKindV1,
    pub retrieval_anchor_id: Option<RetrievalAnchorId>,
}

impl ProximityRelationPathV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.retrieval_anchor_id
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)
    }
}

/// Reference-only provenance for one proximity candidate. `BelowThreshold`
/// marks a relationship hint the effective policy excluded; it is a successful
/// zero-candidate result. No inclusion emits a lock, schedules work, or
/// continues an agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProximityContributionV1 {
    pub contribution_id: ProximityContributionIdV1,
    pub warning_class: ProximityWarningClassV1,
    pub source_observation_ids: Vec<ProximityObservationIdV1>,
    pub retrieval_anchor_ids: Vec<RetrievalAnchorId>,
    pub address: Option<ProximityAddressV1>,
    pub relation_paths: Vec<ProximityRelationPathV1>,
    pub observed_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub coverage: ProximityCoverageV1,
    pub inclusion: ProximityInclusionV1,
}

impl ProximityContributionV1 {
    /// Expired contributions are never valid presentation or dedupe input for
    /// a later request. The source authority decides whether a fresh value can
    /// be produced; this contract merely makes stale reuse unrepresentable.
    pub const fn is_expired_at(&self, observed_at: UtcMicros) -> bool {
        observed_at.0 >= self.expires_at.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.contribution_id.validate()?;
        let concealed = matches!(
            self.inclusion,
            ProximityInclusionV1::Denied | ProximityInclusionV1::Private
        );
        for observation_id in &self.source_observation_ids {
            observation_id.validate()?;
        }
        for anchor_id in &self.retrieval_anchor_ids {
            anchor_id.validate()?;
        }
        self.address
            .as_ref()
            .map_or(Ok(()), ProximityAddressV1::validate)?;
        for path in &self.relation_paths {
            path.validate()?;
        }
        let coverage_matches = matches!(
            (self.inclusion, self.coverage),
            (
                ProximityInclusionV1::Included
                    | ProximityInclusionV1::BelowThreshold
                    | ProximityInclusionV1::SuppressedDuplicate,
                ProximityCoverageV1::Complete | ProximityCoverageV1::Partial
            ) | (ProximityInclusionV1::Stale, ProximityCoverageV1::Stale)
                | (ProximityInclusionV1::Denied, ProximityCoverageV1::Denied)
                | (ProximityInclusionV1::Private, ProximityCoverageV1::Private)
        );
        if !coverage_matches {
            return Err(DomainError::NonCanonical {
                field: "proximity inclusion coverage",
            });
        }

        if self.inclusion == ProximityInclusionV1::BelowThreshold
            && self.warning_class.is_exact_overlap()
        {
            return Err(DomainError::NonCanonical {
                field: "exact proximity below threshold",
            });
        }
        if concealed {
            if !self.source_observation_ids.is_empty()
                || !self.retrieval_anchor_ids.is_empty()
                || self.address.is_some()
                || !self.relation_paths.is_empty()
            {
                return Err(DomainError::NonCanonical {
                    field: "concealed proximity evidence",
                });
            }
        } else if self.source_observation_ids.is_empty()
            || self.retrieval_anchor_ids.is_empty()
            || self.address.is_none()
        {
            return Err(DomainError::NonCanonical {
                field: "proximity evidence",
            });
        }

        if !concealed {
            let Some(address) = self.address.as_ref() else {
                return Err(DomainError::NonCanonical {
                    field: "proximity evidence",
                });
            };
            let has_relation = |kind| self.relation_paths.iter().any(|path| path.kind == kind);
            let exact_shape = match self.warning_class {
                ProximityWarningClassV1::SameFile => true,
                ProximityWarningClassV1::OverlappingRange => address.span.is_some(),
                ProximityWarningClassV1::SameSymbol => address.symbol.is_some(),
                ProximityWarningClassV1::SamePackage => {
                    has_relation(ProximityRelationPathKindV1::PackageMembership)
                }
                ProximityWarningClassV1::SameCrate => {
                    has_relation(ProximityRelationPathKindV1::CrateMembership)
                }
                ProximityWarningClassV1::Neighborhood => {
                    has_relation(ProximityRelationPathKindV1::NeighborhoodMembership)
                }
                ProximityWarningClassV1::SharedCaller => {
                    has_relation(ProximityRelationPathKindV1::DirectCaller)
                        || has_relation(ProximityRelationPathKindV1::TransitiveCaller)
                }
                ProximityWarningClassV1::SharedDependency => {
                    has_relation(ProximityRelationPathKindV1::DirectDependency)
                        || has_relation(ProximityRelationPathKindV1::TransitiveDependency)
                }
                ProximityWarningClassV1::SharedTest => {
                    has_relation(ProximityRelationPathKindV1::AffectedTest)
                }
            };
            if !exact_shape {
                return Err(DomainError::NonCanonical {
                    field: "proximity warning evidence shape",
                });
            }
        }

        if self.expires_at.0 <= self.observed_at.0 {
            return Err(DomainError::NonCanonical {
                field: "proximity expiry",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concealed_private_contribution() -> ProximityContributionV1 {
        ProximityContributionV1 {
            contribution_id: ProximityContributionIdV1::new("contribution.private").unwrap(),
            warning_class: ProximityWarningClassV1::Neighborhood,
            source_observation_ids: Vec::new(),
            retrieval_anchor_ids: Vec::new(),
            address: None,
            relation_paths: Vec::new(),
            observed_at: UtcMicros(1),
            expires_at: UtcMicros(2),
            coverage: ProximityCoverageV1::Private,
            inclusion: ProximityInclusionV1::Private,
        }
    }

    #[test]
    fn private_proximity_exposes_no_evidence() {
        let contribution = concealed_private_contribution();
        contribution.validate().unwrap();
        assert!(!contribution.is_expired_at(UtcMicros(1)));
        assert!(contribution.is_expired_at(UtcMicros(2)));

        let mut leaking = contribution;
        leaking.relation_paths.push(ProximityRelationPathV1 {
            kind: ProximityRelationPathKindV1::DirectCaller,
            retrieval_anchor_id: None,
        });
        assert!(leaking.validate().is_err());
    }

    #[test]
    fn concealed_proximity_requires_matching_coverage() {
        let mut contribution = concealed_private_contribution();
        contribution.inclusion = ProximityInclusionV1::Denied;
        assert_eq!(
            contribution.validate().unwrap_err().to_string(),
            "proximity inclusion coverage is not canonical"
        );
        contribution.coverage = ProximityCoverageV1::Denied;
        assert!(contribution.validate().is_ok());
    }

    #[test]
    fn exact_overlap_is_never_below_threshold() {
        let mut contribution = concealed_private_contribution();
        contribution.warning_class = ProximityWarningClassV1::SameFile;
        contribution.inclusion = ProximityInclusionV1::BelowThreshold;
        contribution.coverage = ProximityCoverageV1::Complete;
        assert_eq!(
            contribution.validate().unwrap_err().to_string(),
            "exact proximity below threshold is not canonical"
        );
        assert!(ProximityWarningClassV1::SameSymbol.is_exact_overlap());
        assert!(!ProximityWarningClassV1::SharedDependency.is_exact_overlap());
    }
}

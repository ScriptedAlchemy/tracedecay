//! Advisory concurrent-work proximity contracts.
//!
//! These types describe overlap evidence only. They grant no lease, lock,
//! scheduler admission, work assignment, or agent-continuation authority.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::code_intelligence::identity::{FileOccurrenceId, SourceSpan, SymbolOccurrenceId};
use crate::research::{DomainError, ManifestDigest, RetrievalAnchorId, UtcMicros};

use super::FeedbackScopeV1;

pub const PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1: &str = "feedback.proximity.risk_threshold";

crate::canonical_text::validated_string_newtype!(
    plain,
    DomainError,
    super::validate_label;
    ProximityContributionIdV1 => "proximity contribution id",
    ProximityWarningIdV1 => "proximity warning id",
    ProximityObservationIdV1 => "proximity observation id",
);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProximityTierV1 {
    /// Exact same-file/range/symbol conflicts emit without a risk threshold.
    Immediate,
    /// Package/crate/neighborhood relations require configured risk gating.
    Configured,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    IncompatibleBranchWorktree,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProximityRelationStrengthV1 {
    Direct,
    Transitive,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProximityBranchWorktreeIncompatibilityV1 {
    Compatible,
    BranchDiverged,
    WorktreeDiverged,
    Incompatible,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProximityCoverageV1 {
    Complete,
    Partial,
    Stale,
    Unavailable,
    Denied,
    Private,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

/// Explicit threshold inputs. Scores use basis points to preserve equality and
/// persistence semantics without encoding a local scoring implementation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProximityRiskInputsV1 {
    pub overlap_size: u32,
    pub blast_radius_size: u32,
    pub relation_strength: ProximityRelationStrengthV1,
    pub branch_worktree_incompatibility: ProximityBranchWorktreeIncompatibilityV1,
    pub freshness_decay_basis_points: u16,
}

impl ProximityRiskInputsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.freshness_decay_basis_points > 10_000 {
            return Err(DomainError::NonCanonical {
                field: "proximity freshness decay basis points",
            });
        }
        Ok(())
    }
}

/// Reference-only provenance for one proximity candidate. `BelowThreshold` is
/// a successful zero-candidate result; no tier emits a lock, schedules work,
/// or continues an agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProximityContributionV1 {
    pub contribution_id: ProximityContributionIdV1,
    pub warning_id: ProximityWarningIdV1,
    pub warning_class: ProximityWarningClassV1,
    pub source_observation_ids: Vec<ProximityObservationIdV1>,
    pub retrieval_anchor_ids: Vec<RetrievalAnchorId>,
    pub address: Option<ProximityAddressV1>,
    pub relation_paths: Vec<ProximityRelationPathV1>,
    pub risk_inputs: Option<ProximityRiskInputsV1>,
    pub tier: ProximityTierV1,
    pub threshold_value_basis_points: Option<u16>,
    pub threshold_revision: Option<ManifestDigest>,
    pub raw_risk_basis_points: Option<u16>,
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

    /// Records presentation suppression without discarding the evidence,
    /// threshold provenance, or expiry that produced the duplicate warning.
    pub fn suppressed_duplicate(mut self) -> Result<Self, DomainError> {
        self.validate()?;
        if self.inclusion != ProximityInclusionV1::Included {
            return Err(DomainError::NonCanonical {
                field: "proximity duplicate suppression input",
            });
        }
        self.inclusion = ProximityInclusionV1::SuppressedDuplicate;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.contribution_id.validate()?;
        self.warning_id.validate()?;
        let immediate_class = matches!(
            self.warning_class,
            ProximityWarningClassV1::SameFile
                | ProximityWarningClassV1::OverlappingRange
                | ProximityWarningClassV1::SameSymbol
        );
        match (self.tier, immediate_class) {
            (ProximityTierV1::Immediate, true) | (ProximityTierV1::Configured, false) => {}
            (ProximityTierV1::Immediate, false) => {
                return Err(DomainError::NonCanonical {
                    field: "immediate proximity warning class",
                });
            }
            (ProximityTierV1::Configured, true) => {
                return Err(DomainError::NonCanonical {
                    field: "configured proximity warning class",
                });
            }
        }
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
        self.risk_inputs
            .as_ref()
            .map_or(Ok(()), ProximityRiskInputsV1::validate)?;
        if self
            .raw_risk_basis_points
            .is_some_and(|value| value > 10_000)
        {
            return Err(DomainError::NonCanonical {
                field: "proximity raw risk basis points",
            });
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

        if concealed {
            if self.threshold_value_basis_points.is_some() || self.threshold_revision.is_some() {
                return Err(DomainError::NonCanonical {
                    field: "concealed proximity threshold",
                });
            }
        } else {
            match (
                self.tier,
                self.threshold_value_basis_points,
                self.threshold_revision.as_ref(),
            ) {
                (ProximityTierV1::Immediate, None, None) => {}
                (ProximityTierV1::Immediate, _, _) => {
                    return Err(DomainError::NonCanonical {
                        field: "immediate proximity threshold",
                    });
                }
                (ProximityTierV1::Configured, Some(value), Some(revision)) => {
                    if value > 10_000 {
                        return Err(DomainError::NonCanonical {
                            field: "configured proximity threshold basis points",
                        });
                    }
                    revision.validate()?;
                }
                (ProximityTierV1::Configured, _, _) => {
                    return Err(DomainError::NonCanonical {
                        field: "configured proximity threshold",
                    });
                }
            }
        }

        if self.inclusion == ProximityInclusionV1::BelowThreshold
            && self.tier != ProximityTierV1::Configured
        {
            return Err(DomainError::NonCanonical {
                field: "immediate proximity below threshold",
            });
        }
        if concealed {
            if !self.source_observation_ids.is_empty()
                || !self.retrieval_anchor_ids.is_empty()
                || self.address.is_some()
                || !self.relation_paths.is_empty()
                || self.risk_inputs.is_some()
                || self.raw_risk_basis_points.is_some()
            {
                return Err(DomainError::NonCanonical {
                    field: "concealed proximity evidence",
                });
            }
        } else if self.source_observation_ids.is_empty()
            || self.retrieval_anchor_ids.is_empty()
            || self.address.is_none()
            || self.risk_inputs.is_none()
            || self.raw_risk_basis_points.is_none()
        {
            return Err(DomainError::NonCanonical {
                field: "proximity evidence",
            });
        }

        if !concealed {
            let address = self.address.as_ref().expect("validated above");
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
                ProximityWarningClassV1::IncompatibleBranchWorktree => {
                    self.risk_inputs.as_ref().is_some_and(|inputs| {
                        inputs.branch_worktree_incompatibility
                            != ProximityBranchWorktreeIncompatibilityV1::Compatible
                    })
                }
            };
            if !exact_shape {
                return Err(DomainError::NonCanonical {
                    field: "proximity warning evidence shape",
                });
            }
        }

        if let (ProximityTierV1::Configured, Some(threshold), Some(raw_risk)) = (
            self.tier,
            self.threshold_value_basis_points,
            self.raw_risk_basis_points,
        ) {
            let threshold_relation_is_valid = match self.inclusion {
                ProximityInclusionV1::BelowThreshold => raw_risk < threshold,
                ProximityInclusionV1::Included => raw_risk >= threshold,
                _ => true,
            };
            if !threshold_relation_is_valid {
                return Err(DomainError::NonCanonical {
                    field: "proximity threshold inclusion",
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
            warning_id: ProximityWarningIdV1::new("warning.private").unwrap(),
            warning_class: ProximityWarningClassV1::Neighborhood,
            source_observation_ids: Vec::new(),
            retrieval_anchor_ids: Vec::new(),
            address: None,
            relation_paths: Vec::new(),
            risk_inputs: None,
            tier: ProximityTierV1::Configured,
            threshold_value_basis_points: None,
            threshold_revision: None,
            raw_risk_basis_points: None,
            observed_at: UtcMicros(1),
            expires_at: UtcMicros(2),
            coverage: ProximityCoverageV1::Private,
            inclusion: ProximityInclusionV1::Private,
        }
    }

    #[test]
    fn private_proximity_exposes_no_evidence_or_threshold_inputs() {
        let contribution = concealed_private_contribution();
        contribution.validate().unwrap();
        assert!(!contribution.is_expired_at(UtcMicros(1)));
        assert!(contribution.is_expired_at(UtcMicros(2)));

        let mut leaking = contribution;
        leaking.raw_risk_basis_points = Some(9_000);
        assert!(leaking.validate().is_err());
    }

    #[test]
    fn concealed_proximity_requires_matching_coverage() {
        let mut contribution = concealed_private_contribution();
        contribution.inclusion = ProximityInclusionV1::Denied;
        assert!(contribution.validate().is_err());
        contribution.coverage = ProximityCoverageV1::Denied;
        assert!(contribution.validate().is_ok());
    }
}

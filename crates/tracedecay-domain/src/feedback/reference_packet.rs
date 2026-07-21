//! PR13 reference-only feedback packet contracts.
//!
//! This packet retains identifiers, anchors, state, and bounded display
//! framing only. It intentionally has no `TaskId`, work-item, task-link, or
//! workflow field: PR17 owns separately authorized task-link composition.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::{
    DomainError, ManifestDigest, RetrievalAnchorId, UtcMicros, canonical_sha256,
};

use super::ci_localization::CiFailureLocalizationStateV1;
use super::evidence_packet::FeedbackPacketId;
use super::github_review::{GitHubReviewIngressProviderOutcomeV1, GitHubReviewLifecycleV1};
use super::proximity::{ProximityContributionV1, ProximityInclusionV1};
use super::{
    FeedbackCycleId, FeedbackCycleResultV1, FeedbackDurabilityV1, FeedbackFindingId,
    FeedbackResultId, FeedbackScopeV1, ProviderEvaluationStateV1,
};

pub const FEEDBACK_REFERENCE_PACKET_SCHEMA_VERSION_V1: u16 = 1;

macro_rules! feedback_reference_id {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                super::validate_label(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                super::validate_label(&self.0, $field)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

feedback_reference_id!(
    FeedbackReferenceSourceRecordIdV1,
    "feedback reference source record id"
);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackReferenceFindingKindV1 {
    PostEditDiagnostic,
    GitHubReview,
    CiLocalization,
    Proximity,
}

/// The source outcome is typed by source family and remains distinct from the
/// persisted finding's lifecycle or delivery presentation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackReferenceSourceStateV1 {
    PostEditDiagnostic(ProviderEvaluationStateV1),
    GitHubReview {
        lifecycle: GitHubReviewLifecycleV1,
        provider_outcome: GitHubReviewIngressProviderOutcomeV1,
    },
    CiLocalization(CiFailureLocalizationStateV1),
    Proximity(ProximityInclusionV1),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackReferenceCoverageV1 {
    Complete,
    Partial,
    Stale,
    Unavailable,
    Denied,
    Private,
}

/// One persisted reference to evidence owned by its source system. It cannot
/// carry source text, review bodies, CI logs, or private session payloads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackReferenceFindingV1 {
    pub finding_id: FeedbackFindingId,
    pub kind: FeedbackReferenceFindingKindV1,
    pub retrieval_anchor_id: RetrievalAnchorId,
    pub source_record_id: FeedbackReferenceSourceRecordIdV1,
    pub source_state: FeedbackReferenceSourceStateV1,
    pub coverage: FeedbackReferenceCoverageV1,
    pub observed_at: UtcMicros,
    pub valid_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub safe_bounded_preview: Option<String>,
}

impl FeedbackReferenceFindingV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.finding_id.validate()?;
        self.retrieval_anchor_id.validate()?;
        self.source_record_id.validate()?;
        let matching_source = matches!(
            (&self.kind, &self.source_state),
            (
                FeedbackReferenceFindingKindV1::PostEditDiagnostic,
                FeedbackReferenceSourceStateV1::PostEditDiagnostic(_)
            ) | (
                FeedbackReferenceFindingKindV1::GitHubReview,
                FeedbackReferenceSourceStateV1::GitHubReview { .. }
            ) | (
                FeedbackReferenceFindingKindV1::CiLocalization,
                FeedbackReferenceSourceStateV1::CiLocalization(_)
            ) | (
                FeedbackReferenceFindingKindV1::Proximity,
                FeedbackReferenceSourceStateV1::Proximity(_)
            )
        );
        if !matching_source {
            return Err(DomainError::NonCanonical {
                field: "feedback reference finding source state",
            });
        }
        let matching_coverage = match &self.source_state {
            FeedbackReferenceSourceStateV1::PostEditDiagnostic(state) => match state {
                ProviderEvaluationStateV1::SupportedCompletedComplete => {
                    self.coverage == FeedbackReferenceCoverageV1::Complete
                }
                ProviderEvaluationStateV1::Partial => {
                    self.coverage == FeedbackReferenceCoverageV1::Partial
                }
                ProviderEvaluationStateV1::Stale => {
                    self.coverage == FeedbackReferenceCoverageV1::Stale
                }
                _ => self.coverage == FeedbackReferenceCoverageV1::Unavailable,
            },
            FeedbackReferenceSourceStateV1::GitHubReview {
                provider_outcome, ..
            } => match provider_outcome {
                GitHubReviewIngressProviderOutcomeV1::Complete => {
                    self.coverage == FeedbackReferenceCoverageV1::Complete
                }
                GitHubReviewIngressProviderOutcomeV1::Partial => {
                    self.coverage == FeedbackReferenceCoverageV1::Partial
                }
                GitHubReviewIngressProviderOutcomeV1::Denied => {
                    self.coverage == FeedbackReferenceCoverageV1::Denied
                }
                GitHubReviewIngressProviderOutcomeV1::Stale => {
                    self.coverage == FeedbackReferenceCoverageV1::Stale
                }
                GitHubReviewIngressProviderOutcomeV1::RateLimited
                | GitHubReviewIngressProviderOutcomeV1::Failed => matches!(
                    self.coverage,
                    FeedbackReferenceCoverageV1::Partial | FeedbackReferenceCoverageV1::Unavailable
                ),
                GitHubReviewIngressProviderOutcomeV1::Unavailable => {
                    self.coverage == FeedbackReferenceCoverageV1::Unavailable
                }
            },
            FeedbackReferenceSourceStateV1::CiLocalization(state) => match state {
                CiFailureLocalizationStateV1::Complete => {
                    self.coverage == FeedbackReferenceCoverageV1::Complete
                }
                CiFailureLocalizationStateV1::Partial => {
                    self.coverage == FeedbackReferenceCoverageV1::Partial
                }
                CiFailureLocalizationStateV1::Denied => {
                    self.coverage == FeedbackReferenceCoverageV1::Denied
                }
                CiFailureLocalizationStateV1::Stale => {
                    self.coverage == FeedbackReferenceCoverageV1::Stale
                }
                CiFailureLocalizationStateV1::Failed => matches!(
                    self.coverage,
                    FeedbackReferenceCoverageV1::Partial | FeedbackReferenceCoverageV1::Unavailable
                ),
                CiFailureLocalizationStateV1::Unavailable => {
                    self.coverage == FeedbackReferenceCoverageV1::Unavailable
                }
            },
            FeedbackReferenceSourceStateV1::Proximity(inclusion) => match inclusion {
                ProximityInclusionV1::Included
                | ProximityInclusionV1::BelowThreshold
                | ProximityInclusionV1::SuppressedDuplicate => matches!(
                    self.coverage,
                    FeedbackReferenceCoverageV1::Complete | FeedbackReferenceCoverageV1::Partial
                ),
                ProximityInclusionV1::Stale => self.coverage == FeedbackReferenceCoverageV1::Stale,
                ProximityInclusionV1::Denied => {
                    self.coverage == FeedbackReferenceCoverageV1::Denied
                }
                ProximityInclusionV1::Private => {
                    self.coverage == FeedbackReferenceCoverageV1::Private
                }
            },
        };
        if !matching_coverage {
            return Err(DomainError::NonCanonical {
                field: "feedback reference finding coverage",
            });
        }
        if let Some(preview) = &self.safe_bounded_preview {
            super::validate_label(preview, "feedback reference safe preview")?;
            if preview.len() > 512 {
                return Err(DomainError::UnsafeText {
                    field: "feedback reference safe preview",
                });
            }
        }
        if self.observed_at.0 > self.valid_at.0 || self.valid_at.0 >= self.expires_at.0 {
            return Err(DomainError::NonCanonical {
                field: "feedback reference finding validity",
            });
        }
        if matches!(
            self.coverage,
            FeedbackReferenceCoverageV1::Denied | FeedbackReferenceCoverageV1::Private
        ) && self.safe_bounded_preview.is_some()
        {
            return Err(DomainError::UnsafeText {
                field: "concealed feedback reference preview",
            });
        }
        Ok(())
    }
}

/// Persisted PR13 packet gate. The value is reference-only and advisory-only;
/// task linking and retriever-fusion rank influence remain disabled until PR17.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackReferencePacketV1 {
    pub schema_version: u16,
    pub packet_id: FeedbackPacketId,
    pub cycle_id: FeedbackCycleId,
    pub cycle_result_id: FeedbackResultId,
    pub scope: FeedbackScopeV1,
    pub findings: Vec<FeedbackReferenceFindingV1>,
    pub proximity_contributions: Vec<ProximityContributionV1>,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub advisory_only: bool,
}

impl FeedbackReferencePacketV1 {
    pub fn from_cycle_result(
        result: &FeedbackCycleResultV1,
        findings: Vec<FeedbackReferenceFindingV1>,
        proximity_contributions: Vec<ProximityContributionV1>,
    ) -> Result<Self, DomainError> {
        let mut packet = Self {
            schema_version: FEEDBACK_REFERENCE_PACKET_SCHEMA_VERSION_V1,
            packet_id: FeedbackPacketId::new("pending.feedback.reference.packet")?,
            cycle_id: result.cycle_id.clone(),
            cycle_result_id: result.result_id.clone(),
            scope: result.scope.clone(),
            findings,
            proximity_contributions,
            policy_digest: result.policy_digest.clone(),
            configuration_digest: result.configuration_digest.clone(),
            advisory_only: true,
        };
        packet.packet_id = packet.derive_packet_id()?;
        packet.validate_against(result)?;
        Ok(packet)
    }

    fn derive_packet_id(&self) -> Result<FeedbackPacketId, DomainError> {
        #[derive(Serialize)]
        struct Identity<'a> {
            schema_version: u16,
            cycle_id: &'a FeedbackCycleId,
            cycle_result_id: &'a FeedbackResultId,
            scope: &'a FeedbackScopeV1,
            findings: &'a [FeedbackReferenceFindingV1],
            proximity_contributions: &'a [ProximityContributionV1],
            policy_digest: &'a ManifestDigest,
            configuration_digest: &'a ManifestDigest,
            advisory_only: bool,
        }

        let digest = canonical_sha256(&Identity {
            schema_version: self.schema_version,
            cycle_id: &self.cycle_id,
            cycle_result_id: &self.cycle_result_id,
            scope: &self.scope,
            findings: &self.findings,
            proximity_contributions: &self.proximity_contributions,
            policy_digest: &self.policy_digest,
            configuration_digest: &self.configuration_digest,
            advisory_only: self.advisory_only,
        })?;
        FeedbackPacketId::new(format!("feedback-reference.v1.{}", digest.as_str()))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != FEEDBACK_REFERENCE_PACKET_SCHEMA_VERSION_V1 {
            return Err(DomainError::NonCanonical {
                field: "feedback reference packet schema version",
            });
        }
        self.packet_id.validate()?;
        self.cycle_id.validate()?;
        self.cycle_result_id.validate()?;
        self.scope.validate()?;
        for finding in &self.findings {
            finding.validate()?;
        }
        if self.findings.iter().enumerate().any(|(index, finding)| {
            self.findings[index.saturating_add(1)..]
                .iter()
                .any(|other| other.finding_id == finding.finding_id)
        }) {
            return Err(DomainError::NonCanonical {
                field: "feedback reference packet duplicate finding id",
            });
        }
        for contribution in &self.proximity_contributions {
            contribution.validate()?;
        }
        self.policy_digest.validate()?;
        self.configuration_digest.validate()?;
        if self.packet_id != self.derive_packet_id()? {
            return Err(DomainError::NonCanonical {
                field: "feedback reference packet id",
            });
        }
        if !self.advisory_only {
            return Err(DomainError::NonCanonical {
                field: "feedback reference packet advisory-only flag",
            });
        }
        Ok(())
    }

    /// Proves that this reference-only packet is a projection of the same
    /// canonical application result, not an adapter-local result or a second
    /// set of finding/anchor identities.
    pub fn validate_against(&self, result: &FeedbackCycleResultV1) -> Result<(), DomainError> {
        self.validate()?;
        result.validate()?;
        if result.durability != FeedbackDurabilityV1::Durable
            || self.cycle_id != result.cycle_id
            || self.cycle_result_id != result.result_id
            || self.scope != result.scope
            || self.policy_digest != result.policy_digest
            || self.configuration_digest != result.configuration_digest
            || self.findings.len() != result.findings.len()
        {
            return Err(DomainError::NonCanonical {
                field: "feedback reference packet cycle result",
            });
        }
        for reference in &self.findings {
            let matches_result = result.findings.iter().any(|finding| {
                finding.finding_id == reference.finding_id
                    && finding.retrieval_anchor_id.as_ref() == Some(&reference.retrieval_anchor_id)
            });
            if !matches_result {
                return Err(DomainError::NonCanonical {
                    field: "feedback reference finding identity",
                });
            }
        }
        Ok(())
    }
}

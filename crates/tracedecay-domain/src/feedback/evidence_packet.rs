//! Reference-only durable packet for saved-content feedback.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::{DomainError, ManifestDigest, canonical_sha256};

use super::{
    FeedbackCycleId, FeedbackCycleRequestV1, FeedbackCycleTerminationV1, FeedbackDurabilityV1,
    FeedbackScopeV1, ProviderEvaluationStateV1,
};

const FEEDBACK_PACKET_ID_DOMAIN: &str = "tracedecay.feedback.packet.v1";

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct FeedbackPacketId(String);

fn validate_feedback_packet_id(value: &str) -> Result<(), DomainError> {
    crate::canonical_text::validate_canonical_identity(value, "feedback packet id")
}

impl FeedbackPacketId {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_feedback_packet_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_feedback_packet_id(&self.0)
    }
}

impl<'de> Deserialize<'de> for FeedbackPacketId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for FeedbackPacketId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A packet never copies source text, analyzer payloads, or overlay evidence.
/// Detailed findings are retained by the owning diagnostic/evidence store and
/// expanded only through separately authorized application operations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackEvidencePacketV1 {
    pub packet_id: FeedbackPacketId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub termination: FeedbackCycleTerminationV1,
    pub provider_states: Vec<ProviderEvaluationStateV1>,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub advisory_only: bool,
}

impl FeedbackEvidencePacketV1 {
    pub fn from_request(
        request: &FeedbackCycleRequestV1,
        termination: FeedbackCycleTerminationV1,
        provider_states: &[ProviderEvaluationStateV1],
    ) -> Result<Self, DomainError> {
        request.validate()?;
        if request.durability() != FeedbackDurabilityV1::Durable {
            return Err(DomainError::NonCanonical {
                field: "dirty overlay feedback packet durability",
            });
        }
        if termination == FeedbackCycleTerminationV1::Clean
            && !termination.is_consistent_with_provider_states(provider_states)
        {
            return Err(DomainError::NonCanonical {
                field: "clean feedback packet provider coverage",
            });
        }
        let packet_id = derive_packet_id(request, termination, provider_states)?;
        Ok(Self {
            packet_id,
            cycle_id: request.cycle_id.clone(),
            scope: request.scope.clone(),
            termination,
            provider_states: provider_states.to_vec(),
            policy_digest: request.policy_digest.clone(),
            configuration_digest: request.configuration_digest.clone(),
            advisory_only: true,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.packet_id.validate()?;
        self.cycle_id.validate()?;
        self.scope.validate()?;
        self.policy_digest.validate()?;
        self.configuration_digest.validate()?;
        if !self.advisory_only {
            return Err(DomainError::NonCanonical {
                field: "feedback packet advisory-only flag",
            });
        }
        if self.termination == FeedbackCycleTerminationV1::Clean
            && !self
                .termination
                .is_consistent_with_provider_states(&self.provider_states)
        {
            return Err(DomainError::NonCanonical {
                field: "clean feedback packet provider coverage",
            });
        }
        Ok(())
    }
}

fn derive_packet_id(
    request: &FeedbackCycleRequestV1,
    termination: FeedbackCycleTerminationV1,
    provider_states: &[ProviderEvaluationStateV1],
) -> Result<FeedbackPacketId, DomainError> {
    let digest = canonical_sha256(&(
        FEEDBACK_PACKET_ID_DOMAIN,
        request,
        termination,
        provider_states,
    ))?;
    let encoded =
        crate::canonical_text::sha256_hex_body(digest.as_str(), "feedback packet digest")?;
    FeedbackPacketId::new(format!("feedback.packet.v1.{encoded}"))
}

//! Provider-routing facts declared by the configuration authority.
//!
//! These contracts describe a configured route; the policy crate only ranks
//! the facts supplied here and never discovers a provider or a model itself.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::BTreeSet;

use crate::{
    CredentialReferenceId, DomainError, ProviderId, WorkApprovalPolicy, WorkEgressPolicy,
    WorkExecutionLimits, WorkFallbackTopology, WorkFilesystemPolicy, WorkProviderBackendV1,
    WorkProviderRouteId, WorkSandboxPolicy, canonical_text,
};

const MAX_ROUTE_ENVIRONMENT_KEYS: usize = 128;
const MAX_ROUTE_CREDENTIAL_REFERENCES: usize = 64;

/// Complete execution constraints configured for one provider route.
///
/// The route candidate is the configuration authority for these values. Work
/// admission copies them into the immutable execution snapshot; provider
/// adapters never supply defaults.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRouteExecutionProfileV1 {
    pub sandbox: WorkSandboxPolicy,
    pub approval: WorkApprovalPolicy,
    pub filesystem: WorkFilesystemPolicy,
    pub egress: WorkEgressPolicy,
    pub environment_allowlist: BTreeSet<String>,
    pub credential_references: BTreeSet<CredentialReferenceId>,
    pub limits: WorkExecutionLimits,
    /// Maximum elapsed time from execution admission to provider completion.
    pub maximum_duration_micros: u64,
    pub fallback: WorkFallbackTopology,
}

impl WorkRouteExecutionProfileV1 {
    pub fn validate(&self, provider_id: &ProviderId) -> Result<(), DomainError> {
        if self.maximum_duration_micros == 0
            || self.environment_allowlist.len() > MAX_ROUTE_ENVIRONMENT_KEYS
            || self.credential_references.len() > MAX_ROUTE_CREDENTIAL_REFERENCES
            || self.environment_allowlist.iter().any(|key| {
                key.len() > 128
                    || key.is_empty()
                    || !key.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        {
            return Err(DomainError::NonCanonical {
                field: "work route execution profile",
            });
        }
        match &self.fallback {
            WorkFallbackTopology::Disabled => Ok(()),
            WorkFallbackTopology::CodexCli { route, .. }
                if route.provider_id() == WorkProviderBackendV1::CodexCli.provider_id()
                    && provider_id != WorkProviderBackendV1::CodexCli.provider_id() =>
            {
                Ok(())
            }
            WorkFallbackTopology::CodexCli { .. } => Err(DomainError::NonCanonical {
                field: "work route execution fallback",
            }),
        }
    }
}

/// Ordinal band. Never a probability, never a scalar score.
///
/// Bands are ordered `Lowest` .. `Highest`. Comparison is the only operation
/// consumers perform over them, so no weighted sum can be reconstructed from
/// a recorded decision.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkOrdinalBandV1 {
    Lowest,
    Low,
    Moderate,
    High,
    Highest,
}

impl WorkOrdinalBandV1 {
    /// Widen one band toward `Highest`, saturating.
    pub const fn widened(self) -> Self {
        match self {
            Self::Lowest => Self::Low,
            Self::Low => Self::Moderate,
            Self::Moderate => Self::High,
            Self::High | Self::Highest => Self::Highest,
        }
    }

    /// Mirror a coverage band onto the uncertainty scale.
    pub const fn inverted(self) -> Self {
        match self {
            Self::Lowest => Self::Highest,
            Self::Low => Self::High,
            Self::Moderate => Self::Moderate,
            Self::High => Self::Low,
            Self::Highest => Self::Lowest,
        }
    }
}

/// Where a configured route places task content.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkContentLocationClassV1 {
    Local,
    Tenant,
    External,
}

/// Declared effort class of a configured route.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkEffortClassV1 {
    Minimal,
    Standard,
    Extended,
}

/// One eligible route supplied by the authorized configuration snapshot.
///
/// The application filters these candidates by the current request grant and
/// verifies the exact pinned executable before policy evaluates them.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRouteCandidateV1 {
    pub route_id: String,
    pub provider_capability_id: String,
    pub model_id: String,
    pub effort: WorkEffortClassV1,
    pub declared_budget_ceiling: u64,
    pub content_location: WorkContentLocationClassV1,
    pub correctness: WorkOrdinalBandV1,
    pub sensitive_data_fitness: WorkOrdinalBandV1,
    pub latency: WorkOrdinalBandV1,
    pub cost: WorkOrdinalBandV1,
    pub autonomy: WorkOrdinalBandV1,
    pub evidence_quality: WorkOrdinalBandV1,
    pub execution: WorkRouteExecutionProfileV1,
}

impl WorkRouteCandidateV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        WorkProviderRouteId::new(self.route_id.clone()).map_err(|_| DomainError::NonCanonical {
            field: "work route candidate route id",
        })?;
        let provider = ProviderId::new(self.provider_capability_id.clone()).map_err(|_| {
            DomainError::NonCanonical {
                field: "work route candidate provider id",
            }
        })?;
        if !canonical_text::is_canonical_text_within(&self.model_id, 256)
            || self.declared_budget_ceiling == 0
        {
            return Err(DomainError::NonCanonical {
                field: "work route candidate declaration",
            });
        }
        self.execution.validate(&provider)
    }
}

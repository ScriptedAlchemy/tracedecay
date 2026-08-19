//! Admitted, bounded expansion of one GitHub stack delivery signal.
//!
//! The transport names only a durable signal handle and an optional delivery
//! watermark. The daemon owns recipient authorization and durable host
//! acknowledgement; callers cannot use this request to enumerate signals or
//! acknowledge a delivery they were not authorized to expand.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{ManifestDigest, StackDeliveryWatermarkId, StackSignalId, UtcMicros};

use crate::context::{CancellationSignal, RequestContext};
use crate::error::ApplicationContractError;

pub const GITHUB_STACK_SIGNAL_EXPAND_OPERATION: &str = "github_stack_signal_expand";

/// The public, transport-neutral request for one durable stack signal.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackSignalExpandSurfaceRequest {
    pub signal_id: StackSignalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_watermark_id: Option<StackDeliveryWatermarkId>,
}

impl GitHubStackSignalExpandSurfaceRequest {
    pub fn into_application_request(
        self,
        context: RequestContext,
    ) -> GitHubStackSignalExpandRequestV1 {
        GitHubStackSignalExpandRequestV1 {
            context,
            signal_id: self.signal_id,
            expected_watermark_id: self.expected_watermark_id,
        }
    }
}

/// Request admitted with a daemon-minted [`RequestContext`].
///
/// This shape is deliberately not serializable: the daemon mints its context
/// after it resolves the selected project and capability grant.
#[derive(Clone, Debug)]
pub struct GitHubStackSignalExpandRequestV1 {
    context: RequestContext,
    signal_id: StackSignalId,
    expected_watermark_id: Option<StackDeliveryWatermarkId>,
}

impl GitHubStackSignalExpandRequestV1 {
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    pub fn signal_id(&self) -> &StackSignalId {
        &self.signal_id
    }

    pub fn expected_watermark_id(&self) -> Option<&StackDeliveryWatermarkId> {
        self.expected_watermark_id.as_ref()
    }
}

/// Bounded evidence for the exact signal the coordinator authorized.
///
/// Stack topology, provider payloads, paths, commits, and delivery recipients
/// are intentionally behind the durable signal evidence rather than copied to
/// this result.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackSignalEvidenceRefV1 {
    pub signal_id: StackSignalId,
    pub watermark_id: StackDeliveryWatermarkId,
    pub stack_revision_digest: ManifestDigest,
    pub state_digest: ManifestDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_stack_digest: Option<ManifestDigest>,
    pub observed_at: UtcMicros,
}

impl GitHubStackSignalEvidenceRefV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        signal_id: StackSignalId,
        watermark_id: StackDeliveryWatermarkId,
        stack_revision_digest: ManifestDigest,
        state_digest: ManifestDigest,
        github_stack_digest: Option<ManifestDigest>,
        observed_at: UtcMicros,
    ) -> Result<Self, ApplicationContractError> {
        signal_id.validate()?;
        watermark_id.validate()?;
        stack_revision_digest.validate()?;
        state_digest.validate()?;
        if let Some(github_stack_digest) = &github_stack_digest {
            github_stack_digest.validate()?;
        }
        if observed_at.0 <= 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "GitHub stack signal observed_at",
            });
        }
        Ok(Self {
            signal_id,
            watermark_id,
            stack_revision_digest,
            state_digest,
            github_stack_digest,
            observed_at,
        })
    }
}

/// Truthful non-success outcomes for signal expansion.
///
/// `Concealed` intentionally combines absent and unauthorized signal handles;
/// exposing that distinction would turn this operation into a signal probe.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubStackSignalExpandUnavailableV1 {
    Concealed,
    Stale,
    AuthorityUnmounted,
    Cancelled,
}

/// The one bounded result produced by stack-signal expansion.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GitHubStackSignalExpandSurfaceResultV1 {
    Expanded {
        evidence: GitHubStackSignalEvidenceRefV1,
    },
    Unavailable {
        reason: GitHubStackSignalExpandUnavailableV1,
    },
}

impl GitHubStackSignalExpandSurfaceResultV1 {
    pub const fn unavailable(reason: GitHubStackSignalExpandUnavailableV1) -> Self {
        Self::Unavailable { reason }
    }
}

/// Adapter boundary implemented by the daemon-owned stack coordinator.
///
/// The implementation must authorize `request.context().actor()` before
/// reading the exact signal and must host-ack only after the authorized
/// expansion returned. The application crate deliberately has no dependency
/// on the coordinator or its durable store.
pub trait GitHubStackSignalExpandPort: Send + Sync {
    fn expand(
        &self,
        request: GitHubStackSignalExpandRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<GitHubStackSignalExpandSurfaceResultV1, GitHubStackSignalExpandPortError>;
}

/// Typed adapter failures that never disclose whether a signal exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubStackSignalExpandPortError {
    Concealed,
    Stale,
    Unavailable,
    Cancelled,
}

impl GitHubStackSignalExpandPortError {
    pub const fn into_surface_result(self) -> GitHubStackSignalExpandSurfaceResultV1 {
        let reason = match self {
            Self::Concealed => GitHubStackSignalExpandUnavailableV1::Concealed,
            Self::Stale => GitHubStackSignalExpandUnavailableV1::Stale,
            Self::Unavailable => GitHubStackSignalExpandUnavailableV1::AuthorityUnmounted,
            Self::Cancelled => GitHubStackSignalExpandUnavailableV1::Cancelled,
        };
        GitHubStackSignalExpandSurfaceResultV1::unavailable(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(seed: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).expect("digest")
    }

    #[test]
    fn evidence_reference_rejects_a_nonpositive_observation_time() {
        let result = GitHubStackSignalEvidenceRefV1::new(
            StackSignalId::new("signal.stack.example").expect("signal ID"),
            StackDeliveryWatermarkId::new("watermark.stack.example").expect("watermark ID"),
            digest('a'),
            digest('b'),
            None,
            UtcMicros(0),
        );

        assert_eq!(
            result,
            Err(ApplicationContractError::ZeroValue {
                field: "GitHub stack signal observed_at",
            })
        );
    }

    #[test]
    fn port_failures_preserve_concealed_signal_identity() {
        assert_eq!(
            GitHubStackSignalExpandPortError::Concealed.into_surface_result(),
            GitHubStackSignalExpandSurfaceResultV1::unavailable(
                GitHubStackSignalExpandUnavailableV1::Concealed,
            )
        );
    }
}

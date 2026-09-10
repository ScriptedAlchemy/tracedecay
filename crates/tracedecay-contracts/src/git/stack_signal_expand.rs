//! Admitted, bounded expansion of one GitHub stack delivery signal.
//!
//! The transport may name a durable signal handle and an optional delivery
//! watermark. When the handle is omitted, the daemon selects the oldest
//! pending signal authorized for the admitted actor and exact scope. The daemon owns recipient authorization and durable host
//! acknowledgement; callers cannot use this request to enumerate another
//! recipient's signals or acknowledge a delivery they were not authorized to expand.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BranchStackRevisionId, GitOidV1, ManifestDigest, NativeIntegrationDirectionV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewId,
    NativeIntegrationTerminalOutcomeV1, NativeIntegrationTransactionId, RefId,
    StackDeliveryWatermarkId, StackSignalId, StackSignalKindV1, UtcMicros,
};

use crate::context::{CancellationSignal, RequestContext};
use crate::error::ApplicationContractError;

pub const GITHUB_STACK_SIGNAL_EXPAND_OPERATION: &str = "github_stack_signal_expand";

/// The public, transport-neutral request for one durable stack signal.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackSignalExpandSurfaceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_id: Option<StackSignalId>,
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
    signal_id: Option<StackSignalId>,
    expected_watermark_id: Option<StackDeliveryWatermarkId>,
}

impl GitHubStackSignalExpandRequestV1 {
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    pub fn signal_id(&self) -> Option<&StackSignalId> {
        self.signal_id.as_ref()
    }

    pub fn expected_watermark_id(&self) -> Option<&StackDeliveryWatermarkId> {
        self.expected_watermark_id.as_ref()
    }
}

/// Bounded evidence for the exact signal the coordinator authorized.
///
/// Provider payloads, paths, commit bodies, and delivery recipients remain
/// behind the durable signal evidence rather than copied to this result.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackSignalEvidenceRefV1 {
    pub signal_id: StackSignalId,
    pub watermark_id: StackDeliveryWatermarkId,
    pub kind: StackSignalKindV1,
    pub stack_revision_id: BranchStackRevisionId,
    pub stack_revision_digest: ManifestDigest,
    pub state_digest: ManifestDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_stack_digest: Option<ManifestDigest>,
    pub observed_at: UtcMicros,
    pub native_source: GitHubStackSignalNativeSourceV1,
}

/// Exact branch pair and native preflight that produced the selected signal.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackSignalNativePreviewV1 {
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub direction: NativeIntegrationDirectionV1,
    pub source_ref: RefId,
    pub destination_ref: RefId,
    pub source_tip: GitOidV1,
    pub destination_tip: GitOidV1,
    pub disposition: NativeIntegrationPreviewDispositionV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackSignalNativeTerminalV1 {
    pub transaction_id: NativeIntegrationTransactionId,
    pub receipt_digest: ManifestDigest,
    pub outcome: NativeIntegrationTerminalOutcomeV1,
    pub final_ref_tip: GitOidV1,
    pub completed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum GitHubStackSignalNativeSourceV1 {
    Preflight {
        preview: GitHubStackSignalNativePreviewV1,
    },
    Terminal {
        preview: GitHubStackSignalNativePreviewV1,
        terminal: GitHubStackSignalNativeTerminalV1,
    },
}

impl GitHubStackSignalEvidenceRefV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        signal_id: StackSignalId,
        watermark_id: StackDeliveryWatermarkId,
        kind: StackSignalKindV1,
        stack_revision_id: BranchStackRevisionId,
        stack_revision_digest: ManifestDigest,
        state_digest: ManifestDigest,
        github_stack_digest: Option<ManifestDigest>,
        observed_at: UtcMicros,
        native_source: GitHubStackSignalNativeSourceV1,
    ) -> Result<Self, ApplicationContractError> {
        signal_id.validate()?;
        watermark_id.validate()?;
        stack_revision_id.validate()?;
        stack_revision_digest.validate()?;
        state_digest.validate()?;
        if let Some(github_stack_digest) = &github_stack_digest {
            github_stack_digest.validate()?;
        }
        let preview = match &native_source {
            GitHubStackSignalNativeSourceV1::Preflight { preview } => {
                if preview.preview_digest != state_digest {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "GitHub stack signal native preview digest",
                    });
                }
                preview
            }
            GitHubStackSignalNativeSourceV1::Terminal { preview, terminal } => {
                terminal.transaction_id.validate()?;
                terminal.receipt_digest.validate()?;
                terminal.final_ref_tip.validate()?;
                if terminal.receipt_digest != state_digest || terminal.completed_at.0 <= 0 {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "GitHub stack signal native terminal evidence",
                    });
                }
                preview
            }
        };
        preview.preview_id.validate()?;
        preview.preview_digest.validate()?;
        preview.source_ref.validate()?;
        preview.destination_ref.validate()?;
        preview.source_tip.validate()?;
        preview.destination_tip.validate()?;
        if observed_at.0 <= 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "GitHub stack signal observed_at",
            });
        }
        Ok(Self {
            signal_id,
            watermark_id,
            kind,
            stack_revision_id,
            stack_revision_digest,
            state_digest,
            github_stack_digest,
            observed_at,
            native_source,
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
    NativeEvidenceUnavailable,
    AuthorityUnmounted,
    Cancelled,
}

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
    #[hotpath::skip]
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
    NativeEvidenceUnavailable,
    Unavailable,
    Cancelled,
}

impl GitHubStackSignalExpandPortError {
    #[hotpath::skip]
    pub const fn into_surface_result(self) -> GitHubStackSignalExpandSurfaceResultV1 {
        let reason = match self {
            Self::Concealed => GitHubStackSignalExpandUnavailableV1::Concealed,
            Self::Stale => GitHubStackSignalExpandUnavailableV1::Stale,
            Self::NativeEvidenceUnavailable => {
                GitHubStackSignalExpandUnavailableV1::NativeEvidenceUnavailable
            }
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

    fn native_preview() -> GitHubStackSignalNativePreviewV1 {
        GitHubStackSignalNativePreviewV1 {
            preview_id: NativeIntegrationPreviewId::new("preview.stack.example")
                .expect("preview ID"),
            preview_digest: digest('b'),
            direction: NativeIntegrationDirectionV1::PropagateDependencyToDependent,
            source_ref: RefId::new("refs/heads/dependency").expect("source ref"),
            destination_ref: RefId::new("refs/heads/dependent").expect("destination ref"),
            source_tip: GitOidV1::new("1".repeat(40)).expect("source tip"),
            destination_tip: GitOidV1::new("2".repeat(40)).expect("destination tip"),
            disposition: NativeIntegrationPreviewDispositionV1::NativeConflict {
                conflict_digest: digest('c'),
            },
        }
    }

    #[test]
    fn evidence_reference_rejects_a_nonpositive_observation_time() {
        let result = GitHubStackSignalEvidenceRefV1::new(
            StackSignalId::new("signal.stack.example").expect("signal ID"),
            StackDeliveryWatermarkId::new("watermark.stack.example").expect("watermark ID"),
            StackSignalKindV1::ActualConflict,
            BranchStackRevisionId::new("revision.stack.example").expect("revision ID"),
            digest('a'),
            digest('b'),
            None,
            UtcMicros(0),
            GitHubStackSignalNativeSourceV1::Preflight {
                preview: native_preview(),
            },
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

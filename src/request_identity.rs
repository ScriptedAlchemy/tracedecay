//! Central request/correlation and idempotency identity derivation.
//!
//! Request identity and idempotency identity intentionally have separate APIs:
//! globally unique identities distinguish executions, while logical-effect
//! identities are stable across exact retries.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::RequestId;
use tracedecay_domain::{ManifestDigest, canonical_sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalRequestSurface {
    Cli,
    Http,
    DashboardSettings,
    DashboardStorageTelemetry,
    McpFallback,
    ManagedTestRun,
    FeedbackObservation,
    SemanticEvaluation,
    ProjectOpenFeedbackCycle,
    ProjectOpenGithubDiscovery,
    DaemonDoctor,
    LspFeedbackDiagnostics,
    LspFeedbackGet,
    LspFeedbackExpand,
    SessionRefresh,
    AutomationSessionRetrieval,
    McpSessionRetrieval,
}

impl GlobalRequestSurface {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Cli => "request.cli",
            Self::Http => "request.http",
            Self::DashboardSettings => "request.dashboard.settings",
            Self::DashboardStorageTelemetry => "request.dashboard.storage-telemetry",
            Self::McpFallback => "request.mcp.fallback",
            Self::ManagedTestRun => "request.managed-test-run",
            Self::FeedbackObservation => "request.feedback-observe",
            Self::SemanticEvaluation => "request.semantic-evaluation",
            Self::ProjectOpenFeedbackCycle => "request.project-open.cycle",
            Self::ProjectOpenGithubDiscovery => "request.project-open.github-discovery",
            Self::DaemonDoctor => "request.daemon.doctor",
            Self::LspFeedbackDiagnostics => "request.lsp.feedback-diagnostics",
            Self::LspFeedbackGet => "request.lsp.feedback-get",
            Self::LspFeedbackExpand => "request.lsp.feedback-expand",
            Self::SessionRefresh => "request.mcp.session-refresh",
            Self::AutomationSessionRetrieval => "request.automation.session-retrieval",
            Self::McpSessionRetrieval => "request.mcp.session-retrieval",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalOpaqueIdentityKind {
    DashboardPayloadGcPreview,
    ExplorerRun,
    GitIndexPreview,
    MemoryOperation,
}

impl GlobalOpaqueIdentityKind {
    const fn prefix(self) -> &'static str {
        match self {
            Self::DashboardPayloadGcPreview => "payload-gc",
            Self::ExplorerRun => "explorer-run",
            Self::GitIndexPreview => "preview",
            Self::MemoryOperation => "generated",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalOperationIdentityKind {
    HostArtifact,
    HostComponentSet,
    HostFeedbackRollback,
}

impl GlobalOperationIdentityKind {
    const fn domain(self) -> &'static [u8] {
        match self {
            Self::HostArtifact => b"tracedecay.unique-operation.host-artifact.v1",
            Self::HostComponentSet => b"tracedecay.unique-operation.host-component-set.v1",
            Self::HostFeedbackRollback => b"tracedecay.unique-operation.host-feedback-rollback.v1",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalEffectIdempotencyDomain {
    HostObservation,
    FeedbackObservation,
    FeedbackSourceEvent,
    ConfigurationEffect,
    DoctorRemediationPreviewOperation,
    DoctorRemediationApplyOperation,
}

impl LogicalEffectIdempotencyDomain {
    const fn domain(self) -> &'static str {
        match self {
            Self::HostObservation => "tracedecay.host-observation.idempotency.v1",
            Self::FeedbackObservation => "tracedecay.feedback.observation.plan26.v1",
            Self::FeedbackSourceEvent => "tracedecay.feedback.source-event.plan26.v1",
            Self::ConfigurationEffect => "tracedecay.configuration.effect-idempotency.v1",
            Self::DoctorRemediationPreviewOperation => {
                "tracedecay.doctor-remediation-preview-operation.v1"
            }
            Self::DoctorRemediationApplyOperation => {
                "tracedecay.doctor-remediation-apply-operation.v1"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewIdentityDomain {
    SourceEdit,
}

impl PreviewIdentityDomain {
    const fn domain(self) -> &'static str {
        match self {
            Self::SourceEdit => "tracedecay.source-edit-preview-idempotency.v1",
        }
    }
}

#[derive(Debug, Error)]
pub enum RequestIdentityError {
    #[error("operating-system entropy is unavailable")]
    EntropyUnavailable,
    #[error("the process-wide identity sequence is exhausted")]
    SequenceExhausted,
    #[error("the derived request identity is invalid")]
    InvalidRequestIdentity,
    #[error("the logical-effect identity could not be derived")]
    InvalidLogicalEffectIdentity,
    #[error("the preview identity could not be derived")]
    InvalidPreviewIdentity,
}

struct ProcessUniqueIdentityAuthority {
    instance_nonce: [u8; 16],
    next_sequence: AtomicU64,
}

impl ProcessUniqueIdentityAuthority {
    fn from_instance_nonce(instance_nonce: [u8; 16]) -> Self {
        Self {
            instance_nonce,
            next_sequence: AtomicU64::new(0),
        }
    }

    fn next(&self) -> Result<u64, RequestIdentityError> {
        self.next_sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RequestIdentityError::SequenceExhausted)
    }

    fn mint_string(&self, prefix: &str) -> Result<String, RequestIdentityError> {
        let sequence = self.next()?;
        Ok(format!(
            "{prefix}.{}.{sequence}",
            hex::encode(self.instance_nonce)
        ))
    }

    fn mint_bytes(
        &self,
        kind: GlobalOperationIdentityKind,
    ) -> Result<[u8; 16], RequestIdentityError> {
        let sequence = self.next()?;
        let mut digest = Sha256::new();
        digest.update(kind.domain());
        digest.update([0]);
        digest.update(self.instance_nonce);
        digest.update(sequence.to_be_bytes());
        let digest = digest.finalize();
        let mut identity = [0_u8; 16];
        identity.copy_from_slice(&digest[..16]);
        Ok(identity)
    }
}

fn global_authority() -> Result<&'static ProcessUniqueIdentityAuthority, RequestIdentityError> {
    static AUTHORITY: OnceLock<Option<ProcessUniqueIdentityAuthority>> = OnceLock::new();
    AUTHORITY
        .get_or_init(|| {
            let mut instance_nonce = [0_u8; 16];
            getrandom::getrandom(&mut instance_nonce)
                .ok()
                .map(|()| ProcessUniqueIdentityAuthority::from_instance_nonce(instance_nonce))
        })
        .as_ref()
        .ok_or(RequestIdentityError::EntropyUnavailable)
}

/// Mints a process- and host-independent request identity.
///
/// The process nonce comes from OS entropy and the checked sequence is shared
/// by every surface in this process. Callers cannot accidentally choose a
/// process-local timestamp/counter scope.
pub fn mint_global_request_id(
    surface: GlobalRequestSurface,
) -> Result<RequestId, RequestIdentityError> {
    let value = global_authority()?.mint_string(surface.prefix())?;
    RequestId::new(value).map_err(|_| RequestIdentityError::InvalidRequestIdentity)
}

/// Mints a globally unique opaque identifier for a non-request correlation
/// object such as a preview or dashboard run.
pub fn mint_global_opaque_id(
    kind: GlobalOpaqueIdentityKind,
) -> Result<String, RequestIdentityError> {
    global_authority()?.mint_string(kind.prefix())
}

/// Mints a globally unique binary operation identifier.
pub fn mint_global_operation_id(
    kind: GlobalOperationIdentityKind,
) -> Result<[u8; 16], RequestIdentityError> {
    global_authority()?.mint_bytes(kind)
}

/// Derives the stable identity of one logical effect.
///
/// Unlike [`mint_global_request_id`], identical domain/material pairs must
/// produce identical bytes so an exact retry resolves to the same receipt.
pub fn derive_logical_effect_idempotency<T: Serialize + ?Sized>(
    domain: LogicalEffectIdempotencyDomain,
    material: &T,
) -> Result<ManifestDigest, RequestIdentityError> {
    canonical_sha256(&(domain.domain(), material))
        .map_err(|_| RequestIdentityError::InvalidLogicalEffectIdentity)
}

/// Derives a content-bound preview identity.
///
/// A preview is not an effect replay key: its material includes the request
/// correlation and the complete proposed edit so distinct previews cannot
/// alias even when a transport request id is reused.
pub fn derive_preview_identity<T: Serialize + ?Sized>(
    domain: PreviewIdentityDomain,
    material: &T,
) -> Result<ManifestDigest, RequestIdentityError> {
    canonical_sha256(&(domain.domain(), material))
        .map_err(|_| RequestIdentityError::InvalidPreviewIdentity)
}

/// Widens a JSON-RPC id that is unique only within one globally unique MCP
/// connection scope. This preserves the existing persisted MCP identity format.
pub fn mcp_connection_request_id(id: &Value, connection_scope: &str) -> Option<RequestId> {
    if connection_scope.is_empty() || !matches!(id, Value::String(_) | Value::Number(_)) {
        return None;
    }
    let canonical_id = serde_json::to_vec(id).ok()?;
    let digest = Sha256::digest(&canonical_id);
    RequestId::new(format!(
        "request.mcp.{connection_scope}.{}",
        hex::encode(&digest[..16])
    ))
    .ok()
}

/// Counter for protocol ids whose uniqueness requirement is explicitly one
/// live connection, not global or persistent.
#[derive(Clone, Debug)]
pub struct ConnectionLocalRequestSequence {
    next: u64,
}

impl ConnectionLocalRequestSequence {
    pub const fn starting_at(first: u64) -> Self {
        Self { next: first }
    }

    pub fn next_number(&mut self) -> Result<u64, RequestIdentityError> {
        let current = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or(RequestIdentityError::SequenceExhausted)?;
        Ok(current)
    }

    pub fn next_string(&mut self, prefix: &str) -> Result<String, RequestIdentityError> {
        self.next_number()
            .map(|sequence| format!("{prefix}{sequence}"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::canonical_sha256;

    use super::*;

    #[test]
    fn separate_process_nonces_break_legacy_timestamp_counter_collisions() {
        let first = ProcessUniqueIdentityAuthority::from_instance_nonce([1; 16]);
        let second = ProcessUniqueIdentityAuthority::from_instance_nonce([2; 16]);
        let legacy_pair = (
            "request.http.100.1".to_owned(),
            "request.http.100.1".to_owned(),
        );
        assert_eq!(legacy_pair.0, legacy_pair.1);

        let first = first
            .mint_string(GlobalRequestSurface::Http.prefix())
            .unwrap();
        let second = second
            .mint_string(GlobalRequestSurface::Http.prefix())
            .unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn every_process_local_surface_survives_restart_with_reused_legacy_inputs() {
        let collisions = [
            (GlobalRequestSurface::Cli, "request.cli.100.42.1"),
            (GlobalRequestSurface::Http, "request.http.100.1"),
            (
                GlobalRequestSurface::DashboardSettings,
                "request.dashboard.settings.100.1",
            ),
            (
                GlobalRequestSurface::DashboardStorageTelemetry,
                "request.dashboard.storage-telemetry.100000000",
            ),
            (GlobalRequestSurface::McpFallback, "request.mcp.100.1"),
            (
                GlobalRequestSurface::ManagedTestRun,
                "request.managed-test-run.1",
            ),
            (
                GlobalRequestSurface::FeedbackObservation,
                "request.feedback-observe.100000000",
            ),
            (
                GlobalRequestSurface::SemanticEvaluation,
                "request.semantic-evaluation.100000000",
            ),
            (
                GlobalRequestSurface::ProjectOpenFeedbackCycle,
                "request.project-open.cycle.100000000",
            ),
            (
                GlobalRequestSurface::ProjectOpenGithubDiscovery,
                "request.project-open.github-discovery.commit-a",
            ),
            (
                GlobalRequestSurface::DaemonDoctor,
                "request.daemon.doctor.42.100000000.0",
            ),
            (
                GlobalRequestSurface::LspFeedbackDiagnostics,
                "lsp-feedback-diagnostics-1",
            ),
            (GlobalRequestSurface::LspFeedbackGet, "lsp-feedback-get-1"),
            (
                GlobalRequestSurface::LspFeedbackExpand,
                "lsp-feedback-expand-1",
            ),
            (GlobalRequestSurface::SessionRefresh, "mcp.session-refresh"),
            (
                GlobalRequestSurface::AutomationSessionRetrieval,
                "automation.session-evidence",
            ),
            (
                GlobalRequestSurface::McpSessionRetrieval,
                "mcp.message-search",
            ),
        ];

        for (index, (surface, legacy)) in collisions.into_iter().enumerate() {
            let first = ProcessUniqueIdentityAuthority::from_instance_nonce([1; 16]);
            let second_nonce = u8::try_from(index + 2).unwrap();
            let second = ProcessUniqueIdentityAuthority::from_instance_nonce([second_nonce; 16]);
            let legacy_first_process = legacy.to_owned();
            let legacy_restarted_process = legacy.to_owned();
            assert_eq!(
                legacy_first_process, legacy_restarted_process,
                "the pre-fix pair reused {legacy}"
            );
            assert_ne!(
                first.mint_string(surface.prefix()).unwrap(),
                second.mint_string(surface.prefix()).unwrap(),
                "{surface:?} reused its first identity after restart"
            );
        }
    }

    #[test]
    fn every_global_surface_is_domain_separated() {
        let authority = ProcessUniqueIdentityAuthority::from_instance_nonce([3; 16]);
        let cli = authority
            .mint_string(GlobalRequestSurface::Cli.prefix())
            .unwrap();
        let http = authority
            .mint_string(GlobalRequestSurface::Http.prefix())
            .unwrap();
        assert_ne!(cli, http);
        assert!(cli.starts_with("request.cli."));
        assert!(http.starts_with("request.http."));
    }

    #[test]
    fn mcp_connection_derivation_preserves_wire_type_and_existing_format() {
        let numeric = mcp_connection_request_id(&json!(1), "connection").unwrap();
        let string = mcp_connection_request_id(&json!("1"), "connection").unwrap();
        assert_ne!(numeric, string);
        assert_eq!(
            numeric,
            mcp_connection_request_id(&json!(1), "connection").unwrap()
        );
        assert!(mcp_connection_request_id(&Value::Null, "connection").is_none());
    }

    #[test]
    fn stable_source_edit_identity_preserves_persisted_derivation() {
        let material = ("request.fixture", "edit.fixture");
        let legacy =
            canonical_sha256(&("tracedecay.source-edit-preview-idempotency.v1", material)).unwrap();
        let centralized =
            derive_preview_identity(PreviewIdentityDomain::SourceEdit, &material).unwrap();
        assert_eq!(centralized, legacy);
    }

    #[test]
    fn persisted_idempotency_domains_keep_byte_identical_derivations() {
        for (domain, legacy_domain) in [
            (
                LogicalEffectIdempotencyDomain::HostObservation,
                "tracedecay.host-observation.idempotency.v1",
            ),
            (
                LogicalEffectIdempotencyDomain::FeedbackObservation,
                "tracedecay.feedback.observation.plan26.v1",
            ),
            (
                LogicalEffectIdempotencyDomain::FeedbackSourceEvent,
                "tracedecay.feedback.source-event.plan26.v1",
            ),
            (
                LogicalEffectIdempotencyDomain::DoctorRemediationPreviewOperation,
                "tracedecay.doctor-remediation-preview-operation.v1",
            ),
            (
                LogicalEffectIdempotencyDomain::DoctorRemediationApplyOperation,
                "tracedecay.doctor-remediation-apply-operation.v1",
            ),
        ] {
            let material = ("logical", "effect", 7_u64);
            let legacy = canonical_sha256(&(legacy_domain, &material)).unwrap();
            let centralized = derive_logical_effect_idempotency(domain, &material).unwrap();
            assert_eq!(centralized, legacy, "{domain:?} changed persisted bytes");
        }
    }

    #[test]
    fn idempotency_is_stable_for_same_effect_and_changes_with_effect() {
        let first = derive_logical_effect_idempotency(
            LogicalEffectIdempotencyDomain::ConfigurationEffect,
            &("actor", "scope", "set", "revision", "digest-a"),
        )
        .unwrap();
        let replay = derive_logical_effect_idempotency(
            LogicalEffectIdempotencyDomain::ConfigurationEffect,
            &("actor", "scope", "set", "revision", "digest-a"),
        )
        .unwrap();
        let distinct = derive_logical_effect_idempotency(
            LogicalEffectIdempotencyDomain::ConfigurationEffect,
            &("actor", "scope", "set", "revision", "digest-b"),
        )
        .unwrap();
        assert_eq!(first, replay);
        assert_ne!(first, distinct);
    }

    #[test]
    fn connection_local_sequence_never_wraps_to_a_duplicate() {
        let mut sequence = ConnectionLocalRequestSequence::starting_at(u64::MAX);
        assert!(sequence.next_number().is_err());
    }
}

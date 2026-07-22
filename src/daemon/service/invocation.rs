//! Closed, authenticated daemon invocation protocol.
//!
//! This module deliberately accepts a small typed operation set after the
//! daemon handshake. It is not a generic application invoke endpoint and it
//! never accepts a raw Git request, database selector, or LSP socket address.
//! LSP frames are handled by a daemon-owned protocol actor; the bridge only
//! receives the actor's bounded responses through explicit frame operations.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use tracedecay_application::feedback::{
    FeedbackReadPort, FeedbackRouteAuthorizationPort, FeedbackRuntimeStatePort,
};
use tracedecay_application::{
    AffectedTestsRetrievalPort, AnalyzerAdmittedDiagnosticProviderV1, ApplicationContractError,
    ApplicationOperation, ApplicationOutcome, ApplicationProblem, ApplicationProblemKind,
    ApplicationResult, AuthorityReceipt, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DiagnosticProviderIdentity, DisclosureClass, EffectId,
    EffectReceipt, EffectResult, EvidenceAuthority, EvidenceCoverage, EvidencePacket,
    EvidenceScore, GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexEffectProofV1,
    GitIndexOperationBindingV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexRecoveryRequestV1, GitIndexTransactionApplicationError, GitIndexTransactionPort,
    GitIndexTransactionPortError, GitIndexTransactionService, IdempotencyKey, Omission,
    OperationBudgetUsage, OperationReceipt, OperationTermination, PageState, PolicyConsumerV1,
    PolicyDecisionRef, PolicyEvaluationContextV1, PolicyEvaluatorCompositionV1,
    PolicyEvidenceHorizonV1, PreviewId, PreviewResult, ReconciliationState, RequestContext,
    RequestId, ResolvedScope, RetrieverContribution, RetryDirective, SafeDiagnostic, TemporalState,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, GitHeadStateV1, GitIndexPreviewId, GitIndexPreviewV1,
    GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1, ManifestDigest, ProjectId, RefId,
    RetrievalAnchorId, SourceKindV1, UtcMicros, canonical_sha256,
};
use tracedecay_policy::{AnalyzerAdmissionInputV1, TruthFreshnessRequirementV1};
use tracedecay_tool_catalog::{CapabilityId, EffectClass, UseCaseId};

use crate::application::ProjectSourceAccessSnapshot;
use crate::application::advisory::{
    CanonicalProximityEvidenceAuthorityV1, CiExactEvidenceAuthorityV1, CiReadOnlyProviderArchiveV1,
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCurrentBranchRemapper,
    Pr13AdvisoryDaemonStartupErrorV1, Pr13AdvisoryDaemonStartupRegistrationV1,
    Pr13AdvisoryHookLookupNoticeV1, Pr13AdvisoryProductionOpenErrorV1,
    Pr13AdvisoryProductionOpenV1, Pr13AdvisoryProductionStartupRegistrationV1,
    Pr13AdvisoryProviderAuthoritiesV1, Pr13AdvisoryRuntimeOpenV1,
    open_pr13_advisory_production_authorities, register_pr13_advisory_daemon_startup,
};
use crate::application::configuration::ConfigurationControlStore;
use crate::application::feedback::concrete::{
    Pr12FeedbackRuntime, Pr12FeedbackRuntimeError, ProjectFeedbackStore, open_pr12_feedback_runtime,
};
use crate::application::feedback::observations::{
    Plan26AnchorOperationV1, Plan26DeliveryRouteV1, Plan26FeedbackObservationEmitterV1,
    Plan26FeedbackOperationV1, Plan26FeedbackOutcomeV1, Plan26FeedbackSourceEventV1,
};
use crate::application::feedback::owner::{
    DaemonFeedbackReadOwnerV1, FeedbackReadInvocationResultV1, FeedbackReadOperationV1,
    FeedbackReadOwnerErrorV1, FeedbackReadRequestAuthority,
};
use crate::application::feedback::{
    Pr12FeedbackCycleLspInput, Pr12FeedbackCycleRuntime, Pr12FeedbackCycleRuntimeError,
    open_pr12_feedback_cycle_runtime,
};
use crate::application::lsp_runtime::pr12_lsp_session_factory;
use crate::application::operation_stream::{
    OperationEmitter, OperationEventAuthority, OperationKind, operation_event_authority,
};
use crate::application::primitives::{
    Pr12PrimitiveDispatch, Pr12PrimitiveInvocation, Pr12PrimitiveProjectRuntime,
    Pr12PrimitiveRequest,
};
use crate::application_surface::{GitApplySurfaceRequest, GitPreviewSurfaceRequest};
use crate::daemon::git_transactions::{
    DaemonGitInvocationOwner, DaemonProjectGitIndexTransactionService, daemon_git_policy_evidence,
};
use crate::daemon::lsp_gateway::{
    AdmittedRoot, AuthorizedLspSession, DaemonLspRuntimeSession, DaemonLspSessionEndpoint,
    GatewayCapabilities, LSP_SESSION_TTL_MS, LspEndpointError, LspSessionAccess,
    LspSessionAdmissionPort, LspSessionCredential, LspSessionId, LspSessionOpenRequest,
    LspSessionRegistry, Pr12LspSessionFactory, SessionLifecycle, UpstreamCapabilities,
};
use crate::db::Database;
use crate::diagnostics::lsp::broker::DiagnosticBroker;
use crate::diagnostics::lsp::client::LspRefreshTimeouts;
use crate::diagnostics::lsp::pr12_production_semantic_authorities;
use crate::errors::TraceDecayError;
use crate::lsp_bridge::MAX_LSP_FRAME_BYTES;
use crate::tracedecay::TraceDecay;
use tracedecay_hooks::HookFeedbackDeliveryPortV1;

/// Stable discriminator for the closed post-handshake invocation protocol.
pub(crate) const DAEMON_INVOCATION_PROTOCOL: &str = "tracedecay.daemon.invocation";
/// Initial revision of the daemon-owned invocation wire shape.
pub(crate) const DAEMON_INVOCATION_REVISION: u16 = 1;

pub(crate) fn daemon_operation_event_authority() -> OperationEventAuthority {
    operation_event_authority()
}

const MAX_INVOCATION_REQUEST_ID_BYTES: usize = 128;
const MAX_CLIENT_REVISION_BYTES: usize = 128;
const MAX_ROOT_HINT_BYTES: usize = 4_096;
const MAX_OPAQUE_HANDLE_BYTES: usize = 256;

/// Closed operations accepted by the daemon invocation connection.
///
/// Git operations carry only their reviewed typed surface DTOs. Authority,
/// policy proof, actor, and scope are minted by the daemon after project
/// admission and never accepted from a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DaemonInvocationOperation {
    GitPreview,
    GitApply,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    FeedbackObserve,
    PrimitiveImpact,
    PrimitiveAffectedTests,
    PrimitiveTestResults,
    PrimitiveRead,
    LspOpen,
    LspFrame,
    LspPoll,
    LspAcknowledge,
    LspDetach,
}

impl DaemonInvocationOperation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GitPreview => "git_preview",
            Self::GitApply => "git_apply",
            Self::FeedbackDiagnostics => "feedback_diagnostics",
            Self::FeedbackGet => "feedback_get",
            Self::FeedbackExpand => "feedback_expand",
            Self::FeedbackList => "feedback_list",
            Self::FeedbackObserve => "feedback_observe",
            Self::PrimitiveImpact => "feedback_impact",
            Self::PrimitiveAffectedTests => "affected_tests",
            Self::PrimitiveTestResults => "test_results",
            Self::PrimitiveRead => "primitive_read",
            Self::LspOpen => "lsp_open",
            Self::LspFrame => "lsp_frame",
            Self::LspPoll => "lsp_poll",
            Self::LspAcknowledge => "lsp_acknowledge",
            Self::LspDetach => "lsp_detach",
        }
    }
}

/// Credential-bearing access data exchanged only between a bridge and the
/// authenticated daemon. Its debug representation never prints the secret.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonLspSessionAccess {
    pub(crate) session_id: String,
    credential: String,
}

impl fmt::Debug for DaemonLspSessionAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonLspSessionAccess")
            .field("session_id", &self.session_id)
            .field("credential", &"[redacted]")
            .finish()
    }
}

impl DaemonLspSessionAccess {
    fn from_access(access: &LspSessionAccess) -> Self {
        Self {
            session_id: access.session_id().as_str().to_owned(),
            credential: hex::encode(access.credential().as_bytes()),
        }
    }

    fn into_access(self) -> Result<LspSessionAccess, DaemonInvocationProblem> {
        let session_id = LspSessionId::new(self.session_id)
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let credential = hex::decode(self.credential)
            .ok()
            .and_then(|credential| LspSessionCredential::new(credential).ok())
            .ok_or(DaemonInvocationProblem::InvalidRequest)?;
        Ok(LspSessionAccess::new(session_id, credential))
    }
}

/// One versioned, request-correlated daemon operation.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DaemonInvocationRequest {
    pub(crate) protocol: String,
    pub(crate) revision: u16,
    pub(crate) request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivery_route: Option<Plan26DeliveryRouteV1>,
    #[serde(flatten)]
    pub(crate) payload: DaemonInvocationPayload,
}

/// Operation-specific fields for the closed invocation set.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum DaemonInvocationPayload {
    GitPreview {
        request: GitPreviewSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    GitApply {
        request: GitApplySurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackDiagnostics {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackGet {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackExpand {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackList {
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    FeedbackObserve {
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: Plan26FeedbackSourceEventV1,
    },
    PrimitiveImpact {
        request: tracedecay_application::retrieval::GraphImpactPrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveAffectedTests {
        request: tracedecay_application::retrieval::AffectedFileTestsPrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveTestResults {
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    PrimitiveRead {
        surface_operation: crate::application_surface::ApplicationSurfaceOperation,
        request: Pr12PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    },
    LspOpen {
        client_revision: String,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
    },
    LspFrame {
        session: DaemonLspSessionAccess,
        frame: String,
    },
    LspPoll {
        session: DaemonLspSessionAccess,
    },
    LspAcknowledge {
        session: DaemonLspSessionAccess,
    },
    LspDetach {
        session: DaemonLspSessionAccess,
    },
}

impl DaemonInvocationRequest {
    pub(crate) fn git_preview(
        request_id: impl Into<String>,
        request: GitPreviewSurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitPreview {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub(crate) fn git_apply(
        request_id: impl Into<String>,
        request: GitApplySurfaceRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::GitApply {
                request,
                observed_at,
                deadline,
                cancellation,
            },
        }
    }

    pub(crate) fn feedback(
        request_id: impl Into<String>,
        operation: crate::application_surface::ApplicationSurfaceOperation,
        request_handle: String,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        let payload = match operation {
            crate::application_surface::ApplicationSurfaceOperation::FeedbackDiagnostics => {
                DaemonInvocationPayload::FeedbackDiagnostics {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::FeedbackGet => {
                DaemonInvocationPayload::FeedbackGet {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::FeedbackExpand => {
                DaemonInvocationPayload::FeedbackExpand {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::FeedbackList => {
                DaemonInvocationPayload::FeedbackList {
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact
            | crate::application_surface::ApplicationSurfaceOperation::AffectedTests
            | crate::application_surface::ApplicationSurfaceOperation::TestResults
            | crate::application_surface::ApplicationSurfaceOperation::SessionLookup
            | crate::application_surface::ApplicationSurfaceOperation::QualifiedName
            | crate::application_surface::ApplicationSurfaceOperation::CallChain
            | crate::application_surface::ApplicationSurfaceOperation::FileDependents
            | crate::application_surface::ApplicationSurfaceOperation::SourceLines
            | crate::application_surface::ApplicationSurfaceOperation::SourceBody
            | crate::application_surface::ApplicationSurfaceOperation::SourceOutline
            | crate::application_surface::ApplicationSurfaceOperation::ModuleApi
            | crate::application_surface::ApplicationSurfaceOperation::FileMetadata
            | crate::application_surface::ApplicationSurfaceOperation::HealthRead
            | crate::application_surface::ApplicationSurfaceOperation::StorageStatus
            | crate::application_surface::ApplicationSurfaceOperation::DiagnosticsRead => {
                unreachable!("primitive operations use their typed constructor")
            }
            crate::application_surface::ApplicationSurfaceOperation::GitPreview
            | crate::application_surface::ApplicationSurfaceOperation::GitApply => {
                unreachable!("Git operations use their typed constructors")
            }
        };
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload,
        }
    }

    pub(crate) fn feedback_observation(
        request_id: impl Into<String>,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        event: Plan26FeedbackSourceEventV1,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::FeedbackObserve {
                subject_digest,
                observed_at,
                event,
            },
        }
    }

    pub(crate) fn primitive(
        request_id: impl Into<String>,
        operation: crate::application_surface::ApplicationSurfaceOperation,
        request: Pr12PrimitiveRequest,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Self {
        let payload = match (operation, request) {
            (
                crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact,
                Pr12PrimitiveRequest::Impact(request),
            ) => DaemonInvocationPayload::PrimitiveImpact {
                request,
                observed_at,
                deadline,
                cancellation,
            },
            (
                crate::application_surface::ApplicationSurfaceOperation::AffectedTests,
                Pr12PrimitiveRequest::AffectedFileTests(request),
            ) => DaemonInvocationPayload::PrimitiveAffectedTests {
                request,
                observed_at,
                deadline,
                cancellation,
            },
            (
                crate::application_surface::ApplicationSurfaceOperation::TestResults,
                Pr12PrimitiveRequest::RecentTestResults,
            ) => DaemonInvocationPayload::PrimitiveTestResults {
                observed_at,
                deadline,
                cancellation,
            },
            (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SessionLookup,
                request @ Pr12PrimitiveRequest::SessionLookup(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::QualifiedName,
                request @ Pr12PrimitiveRequest::QualifiedName(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::CallChain,
                request @ Pr12PrimitiveRequest::CallChain(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::FileDependents,
                request @ Pr12PrimitiveRequest::FileDependents(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceLines,
                request @ Pr12PrimitiveRequest::SourceLines(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceBody,
                request @ Pr12PrimitiveRequest::SourceBody(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::SourceOutline,
                request @ Pr12PrimitiveRequest::SourceOutline(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::ModuleApi,
                request @ Pr12PrimitiveRequest::ModuleApi(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::FileMetadata,
                request @ Pr12PrimitiveRequest::FileMetadata(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::HealthRead,
                request @ Pr12PrimitiveRequest::HealthRead(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::StorageStatus,
                request @ Pr12PrimitiveRequest::StorageStatus(_),
            )
            | (
                surface_operation @ crate::application_surface::ApplicationSurfaceOperation::DiagnosticsRead,
                request @ Pr12PrimitiveRequest::DiagnosticsRead(_),
            ) => {
                DaemonInvocationPayload::PrimitiveRead {
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                }
            }
            _ => unreachable!("surface operation and primitive request must match"),
        };
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload,
        }
    }

    pub(crate) fn lsp_open(
        request_id: impl Into<String>,
        client_revision: impl Into<String>,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspOpen {
                client_revision: client_revision.into(),
                requested_root_uri,
                workspace_folders,
            },
        }
    }

    pub(crate) fn lsp_frame(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
        frame: impl Into<String>,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspFrame {
                session,
                frame: frame.into(),
            },
        }
    }

    pub(crate) fn lsp_poll(request_id: impl Into<String>, session: DaemonLspSessionAccess) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspPoll { session },
        }
    }

    pub(crate) fn lsp_acknowledge(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspAcknowledge { session },
        }
    }

    pub(crate) fn lsp_detach(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            delivery_route: None,
            payload: DaemonInvocationPayload::LspDetach { session },
        }
    }

    pub(crate) fn with_delivery_route(mut self, route: Plan26DeliveryRouteV1) -> Self {
        self.delivery_route = Some(route);
        self
    }

    pub(crate) fn operation(&self) -> DaemonInvocationOperation {
        match self.payload {
            DaemonInvocationPayload::GitPreview { .. } => DaemonInvocationOperation::GitPreview,
            DaemonInvocationPayload::GitApply { .. } => DaemonInvocationOperation::GitApply,
            DaemonInvocationPayload::FeedbackDiagnostics { .. } => {
                DaemonInvocationOperation::FeedbackDiagnostics
            }
            DaemonInvocationPayload::FeedbackGet { .. } => DaemonInvocationOperation::FeedbackGet,
            DaemonInvocationPayload::FeedbackExpand { .. } => {
                DaemonInvocationOperation::FeedbackExpand
            }
            DaemonInvocationPayload::FeedbackList { .. } => DaemonInvocationOperation::FeedbackList,
            DaemonInvocationPayload::FeedbackObserve { .. } => {
                DaemonInvocationOperation::FeedbackObserve
            }
            DaemonInvocationPayload::PrimitiveImpact { .. } => {
                DaemonInvocationOperation::PrimitiveImpact
            }
            DaemonInvocationPayload::PrimitiveAffectedTests { .. } => {
                DaemonInvocationOperation::PrimitiveAffectedTests
            }
            DaemonInvocationPayload::PrimitiveTestResults { .. } => {
                DaemonInvocationOperation::PrimitiveTestResults
            }
            DaemonInvocationPayload::PrimitiveRead { .. } => {
                DaemonInvocationOperation::PrimitiveRead
            }
            DaemonInvocationPayload::LspOpen { .. } => DaemonInvocationOperation::LspOpen,
            DaemonInvocationPayload::LspFrame { .. } => DaemonInvocationOperation::LspFrame,
            DaemonInvocationPayload::LspPoll { .. } => DaemonInvocationOperation::LspPoll,
            DaemonInvocationPayload::LspAcknowledge { .. } => {
                DaemonInvocationOperation::LspAcknowledge
            }
            DaemonInvocationPayload::LspDetach { .. } => DaemonInvocationOperation::LspDetach,
        }
    }

    pub(crate) fn requires_project(&self) -> bool {
        matches!(
            self.operation(),
            DaemonInvocationOperation::GitPreview
                | DaemonInvocationOperation::GitApply
                | DaemonInvocationOperation::FeedbackDiagnostics
                | DaemonInvocationOperation::FeedbackGet
                | DaemonInvocationOperation::FeedbackExpand
                | DaemonInvocationOperation::FeedbackList
                | DaemonInvocationOperation::FeedbackObserve
                | DaemonInvocationOperation::PrimitiveImpact
                | DaemonInvocationOperation::PrimitiveAffectedTests
                | DaemonInvocationOperation::PrimitiveTestResults
                | DaemonInvocationOperation::PrimitiveRead
                | DaemonInvocationOperation::LspOpen
        )
    }

    fn validate(&self) -> Result<(), DaemonInvocationProblem> {
        if self.protocol != DAEMON_INVOCATION_PROTOCOL {
            return Err(DaemonInvocationProblem::InvalidRequest);
        }
        if self.revision != DAEMON_INVOCATION_REVISION {
            return Err(DaemonInvocationProblem::UnsupportedRevision);
        }
        if !valid_token(&self.request_id, MAX_INVOCATION_REQUEST_ID_BYTES) {
            return Err(DaemonInvocationProblem::InvalidRequest);
        }
        match &self.payload {
            DaemonInvocationPayload::GitPreview {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::GitApply {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::PrimitiveImpact {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::PrimitiveAffectedTests {
                observed_at,
                deadline,
                cancellation,
                ..
            }
            | DaemonInvocationPayload::PrimitiveTestResults {
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::PrimitiveRead {
                observed_at,
                deadline,
                cancellation,
                ..
            } => {
                if observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::FeedbackDiagnostics {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::FeedbackGet {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::FeedbackExpand {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            }
            | DaemonInvocationPayload::FeedbackList {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                if !valid_token(request_handle, MAX_OPAQUE_HANDLE_BYTES)
                    || observed_at.0 <= 0
                    || deadline.expires_at.0 <= 0
                    || cancellation.token_id.as_str().len() > MAX_OPAQUE_HANDLE_BYTES
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::FeedbackObserve {
                subject_digest,
                observed_at,
                event,
            } => {
                if subject_digest.validate().is_err()
                    || observed_at.0 <= 0
                    || event.validate().is_none()
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspOpen {
                client_revision,
                requested_root_uri,
                workspace_folders,
            } => {
                if !valid_printable(client_revision, MAX_CLIENT_REVISION_BYTES)
                    || requested_root_uri
                        .as_deref()
                        .is_some_and(|uri| !valid_printable(uri, MAX_ROOT_HINT_BYTES))
                    || workspace_folders.len() > 1
                    || workspace_folders
                        .iter()
                        .any(|folder| !valid_printable(folder, MAX_ROOT_HINT_BYTES))
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspFrame { session, frame } => {
                let _ = session.clone().into_access()?;
                if frame.len() > MAX_LSP_FRAME_BYTES {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspPoll { session }
            | DaemonInvocationPayload::LspAcknowledge { session }
            | DaemonInvocationPayload::LspDetach { session } => {
                let _ = session.clone().into_access()?;
            }
        }
        Ok(())
    }
}

/// Parse an invocation only when it explicitly selects this protocol. Ordinary
/// MCP JSON-RPC frames continue through the established daemon route.
pub(crate) fn parse_daemon_invocation_request(
    line: &str,
) -> Option<Result<DaemonInvocationRequest, DaemonInvocationResponse>> {
    let value = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
    if value.get("protocol").and_then(serde_json::Value::as_str) != Some(DAEMON_INVOCATION_PROTOCOL)
    {
        return None;
    }
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some(serde_json::from_value(value).map_err(|_| {
        DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::InvalidRequest)
    }))
}

/// A safe, deliberately non-diagnostic daemon invocation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DaemonInvocationProblem {
    InvalidRequest,
    UnsupportedRevision,
    NotFoundOrNotAuthorized,
    Unavailable,
}

/// Response envelope paired with one invocation request id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonInvocationResponse {
    pub(crate) protocol: String,
    pub(crate) revision: u16,
    pub(crate) request_id: String,
    #[serde(flatten)]
    pub(crate) outcome: DaemonInvocationOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DaemonGitEffectClass {
    IndexStage,
    IndexUnstage,
    IndexCommit,
}

impl DaemonGitEffectClass {
    fn from_application(effect: EffectClass) -> Result<Self, ApplicationContractError> {
        match effect {
            EffectClass::GitIndexStage => Ok(Self::IndexStage),
            EffectClass::GitIndexUnstage => Ok(Self::IndexUnstage),
            EffectClass::GitIndexCommit => Ok(Self::IndexCommit),
            _ => Err(ApplicationContractError::Inconsistent {
                field: "daemon Git effect class",
            }),
        }
    }

    const fn into_application(self) -> EffectClass {
        match self {
            Self::IndexStage => EffectClass::GitIndexStage,
            Self::IndexUnstage => EffectClass::GitIndexUnstage,
            Self::IndexCommit => EffectClass::GitIndexCommit,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonGitPreviewResult {
    preview_id: PreviewId,
    preview_digest: ManifestDigest,
    effect_class: DaemonGitEffectClass,
    authority: AuthorityReceipt,
    expected_state: ManifestDigest,
    execution: OperationReceipt,
    payload: Option<GitIndexPreviewV1>,
}

impl DaemonGitPreviewResult {
    fn from_application(
        result: PreviewResult<GitIndexPreviewV1>,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            preview_id: result.preview_id,
            preview_digest: result.preview_digest,
            effect_class: DaemonGitEffectClass::from_application(result.effect_class)?,
            authority: result.authority,
            expected_state: result.expected_state,
            execution: result.execution,
            payload: result.payload,
        })
    }

    pub(crate) fn into_application_result(
        self,
    ) -> Result<PreviewResult<serde_json::Value>, ApplicationContractError> {
        PreviewResult::new(
            self.preview_id,
            self.preview_digest,
            self.effect_class.into_application(),
            self.authority,
            self.expected_state,
            self.execution,
            self.payload
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "git preview response payload",
                })?,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct DaemonEffectReceipt {
    operation: UseCaseId,
    request_id: RequestId,
    actor: ActorId,
    scope: ResolvedScope,
    effect_class: DaemonGitEffectClass,
    idempotency_key: IdempotencyKey,
    input_digest: ManifestDigest,
    expected_state: ManifestDigest,
    policy_digest: ManifestDigest,
    configuration_digest: ManifestDigest,
    catalog_digest: ManifestDigest,
    privacy_digest: ManifestDigest,
    outcome: tracedecay_application::EffectTermination,
    committed_state: Option<ManifestDigest>,
    external_proof: Option<RetrievalAnchorId>,
}

impl From<EffectReceipt> for DaemonEffectReceipt {
    fn from(receipt: EffectReceipt) -> Self {
        Self {
            operation: receipt.operation,
            request_id: receipt.request_id,
            actor: receipt.actor,
            scope: receipt.scope,
            effect_class: DaemonGitEffectClass::from_application(receipt.effect_class)
                .expect("Git effect receipt class is validated by the application service"),
            idempotency_key: receipt.idempotency_key,
            input_digest: receipt.input_digest,
            expected_state: receipt.expected_state,
            policy_digest: receipt.policy_digest,
            configuration_digest: receipt.configuration_digest,
            catalog_digest: receipt.catalog_digest,
            privacy_digest: receipt.privacy_digest,
            outcome: receipt.outcome,
            committed_state: receipt.committed_state,
            external_proof: receipt.external_proof,
        }
    }
}

impl DaemonEffectReceipt {
    fn into_application(self) -> EffectReceipt {
        EffectReceipt {
            operation: self.operation,
            request_id: self.request_id,
            actor: self.actor,
            scope: self.scope,
            effect_class: self.effect_class.into_application(),
            idempotency_key: self.idempotency_key,
            input_digest: self.input_digest,
            expected_state: self.expected_state,
            policy_digest: self.policy_digest,
            configuration_digest: self.configuration_digest,
            catalog_digest: self.catalog_digest,
            privacy_digest: self.privacy_digest,
            outcome: self.outcome,
            committed_state: self.committed_state,
            external_proof: self.external_proof,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonGitEffectResult {
    effect_id: EffectId,
    effect_class: DaemonGitEffectClass,
    idempotency_key: IdempotencyKey,
    authority: AuthorityReceipt,
    expected_state: ManifestDigest,
    execution: OperationReceipt,
    reconciliation: ReconciliationState,
    receipt: DaemonEffectReceipt,
    payload: Option<GitIndexTransactionReceiptV1>,
}

impl DaemonGitEffectResult {
    fn from_application(
        result: EffectResult<GitIndexTransactionReceiptV1>,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            effect_id: result.effect_id,
            effect_class: DaemonGitEffectClass::from_application(result.effect_class)?,
            idempotency_key: result.idempotency_key,
            authority: result.authority,
            expected_state: result.expected_state,
            execution: result.execution,
            reconciliation: result.reconciliation,
            receipt: result.receipt.into(),
            payload: result.payload,
        })
    }

    pub(crate) fn into_application_result(
        self,
    ) -> Result<EffectResult<serde_json::Value>, ApplicationContractError> {
        EffectResult::new(
            self.effect_id,
            self.effect_class.into_application(),
            self.idempotency_key,
            self.authority,
            self.expected_state,
            self.execution,
            self.reconciliation,
            self.receipt.into_application(),
            self.payload
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "git apply response payload",
                })?,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonFeedbackResult {
    temporal: TemporalState,
    authority: AuthorityReceipt,
    evidence_authorities: Vec<EvidenceAuthority>,
    coverage: EvidenceCoverage,
    omissions: Vec<Omission>,
    scores: Vec<EvidenceScore>,
    contributions: Vec<RetrieverContribution>,
    page: PageState,
    execution: OperationReceipt,
    payload: Option<serde_json::Value>,
}

pub(crate) struct DaemonFeedbackInvocationRequest {
    pub(crate) request_id: RequestId,
    pub(crate) operation: DaemonInvocationOperation,
    pub(crate) request_handle: String,
    pub(crate) observed_at: UtcMicros,
    pub(crate) deadline: Deadline,
    pub(crate) cancellation: CancellationContext,
}

pub(crate) struct DaemonFeedbackInvocationResult {
    pub(crate) scope: ResolvedScope,
    pub(crate) evidence: EvidencePacket<serde_json::Value>,
}

pub(crate) type DaemonFeedbackInvocationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<DaemonFeedbackInvocationResult, ApplicationProblem>> + Send + 'a,
    >,
>;

pub(crate) trait DaemonFeedbackInvocationPort: Send + Sync {
    fn invoke(
        &self,
        request: DaemonFeedbackInvocationRequest,
    ) -> DaemonFeedbackInvocationFuture<'_>;
}

impl<R, P, A> DaemonFeedbackInvocationPort for DaemonFeedbackReadOwnerV1<R, P, A>
where
    R: FeedbackReadRequestAuthority + Send + Sync,
    P: FeedbackReadPort + Send + Sync,
    A: FeedbackRouteAuthorizationPort + Send + Sync,
{
    fn invoke(
        &self,
        request: DaemonFeedbackInvocationRequest,
    ) -> DaemonFeedbackInvocationFuture<'_> {
        Box::pin(async move {
            let operation = match request.operation {
                DaemonInvocationOperation::FeedbackDiagnostics => {
                    FeedbackReadOperationV1::Diagnostics
                }
                DaemonInvocationOperation::FeedbackGet => FeedbackReadOperationV1::Get,
                DaemonInvocationOperation::FeedbackExpand => FeedbackReadOperationV1::Expand,
                DaemonInvocationOperation::FeedbackList => FeedbackReadOperationV1::List,
                _ => {
                    return Err(ApplicationProblem::InvalidRequest {
                        diagnostic: SafeDiagnostic {
                            code: "feedback.invalid_operation".to_owned(),
                            message: "The feedback read operation is invalid".to_owned(),
                        },
                        retry: RetryDirective::Never,
                        legal_actions: Vec::new(),
                    });
                }
            };
            let result = DaemonFeedbackReadOwnerV1::invoke_with_controls(
                self,
                operation,
                &request.request_handle,
                request.observed_at,
                request.deadline,
                request.cancellation,
            )
            .await
            .map_err(feedback_owner_problem)?;
            match result {
                FeedbackReadInvocationResultV1::Diagnostics(result) => {
                    feedback_invocation_result(result)
                }
                FeedbackReadInvocationResultV1::Get(result) => feedback_invocation_result(result),
                FeedbackReadInvocationResultV1::Expand(result) => {
                    feedback_invocation_result(result)
                }
                FeedbackReadInvocationResultV1::List(result) => feedback_invocation_result(result),
            }
        })
    }
}

#[derive(Clone)]
pub(crate) struct DaemonFeedbackInvocationOwner {
    pub(crate) project_id: ProjectId,
    pub(crate) service: Arc<dyn DaemonFeedbackInvocationPort>,
}

impl DaemonFeedbackInvocationOwner {
    pub(crate) fn new(
        project_id: ProjectId,
        service: Arc<dyn DaemonFeedbackInvocationPort>,
    ) -> Self {
        Self {
            project_id,
            service,
        }
    }
}

fn feedback_invocation_result<T>(
    result: ApplicationResult<T>,
) -> Result<DaemonFeedbackInvocationResult, ApplicationProblem>
where
    T: Serialize,
{
    let application = result.map_err(|problem| problem.problem.into_source())?;
    let evidence = match application.outcome {
        ApplicationOutcome::Evidence(packet) => packet,
        ApplicationOutcome::Preview(_) | ApplicationOutcome::Effect(_) => {
            return Err(ApplicationProblem::unavailable(SafeDiagnostic {
                code: "feedback.invalid_owner_result".to_owned(),
                message: "The feedback read owner returned an invalid outcome".to_owned(),
            }));
        }
    };
    let payload = evidence
        .payload
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "feedback.result_encoding_failed".to_owned(),
                message: "The feedback read result could not be encoded".to_owned(),
            })
        })?;
    Ok(DaemonFeedbackInvocationResult {
        scope: application.scope,
        evidence: EvidencePacket {
            temporal: evidence.temporal,
            authority: evidence.authority,
            evidence_authorities: evidence.evidence_authorities,
            coverage: evidence.coverage,
            omissions: evidence.omissions,
            scores: evidence.scores,
            contributions: evidence.contributions,
            page: evidence.page,
            execution: evidence.execution,
            payload,
        },
    })
}

fn feedback_owner_problem(error: FeedbackReadOwnerErrorV1) -> ApplicationProblem {
    match error {
        FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        FeedbackReadOwnerErrorV1::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "feedback.owner_unavailable".to_owned(),
            message: "The feedback read owner is unavailable".to_owned(),
        }),
    }
}

impl DaemonFeedbackResult {
    pub(crate) fn from_application(packet: EvidencePacket<serde_json::Value>) -> Self {
        Self {
            temporal: packet.temporal,
            authority: packet.authority,
            evidence_authorities: packet.evidence_authorities,
            coverage: packet.coverage,
            omissions: packet.omissions,
            scores: packet.scores,
            contributions: packet.contributions,
            page: packet.page,
            execution: packet.execution,
            payload: packet.payload,
        }
    }

    pub(crate) fn into_application(self) -> EvidencePacket<serde_json::Value> {
        EvidencePacket {
            temporal: self.temporal,
            authority: self.authority,
            evidence_authorities: self.evidence_authorities,
            coverage: self.coverage,
            omissions: self.omissions,
            scores: self.scores,
            contributions: self.contributions,
            page: self.page,
            execution: self.execution,
            payload: self.payload,
        }
    }
}

async fn execute_feedback(
    wire_request_id: String,
    owner: Option<DaemonFeedbackInvocationOwner>,
    operation: DaemonInvocationOperation,
    request_handle: String,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    let request_id = match RequestId::new(wire_request_id.clone()) {
        Ok(request_id) => request_id,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let result = owner
        .service
        .invoke(DaemonFeedbackInvocationRequest {
            request_id,
            operation,
            request_handle,
            observed_at,
            deadline,
            cancellation,
        })
        .await;
    match result {
        Ok(result) if result.scope.project_id == owner.project_id => {
            DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::Feedback {
                    scope: result.scope,
                    result: DaemonFeedbackResult::from_application(result.evidence),
                },
            )
        }
        Ok(_) => concealed_application_problem(wire_request_id),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_primitive(
    service: &DaemonInvocationService,
    project_root: Option<&Path>,
    wire_request_id: String,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: Pr12PrimitiveRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(project_root) = project_root else {
        return concealed_application_problem(wire_request_id);
    };
    let dispatch = service
        .primitive_runtimes
        .lock()
        .await
        .get(project_root)
        .map(Pr12PrimitiveProjectRuntime::dispatch);
    let Some(dispatch) = dispatch else {
        return concealed_application_problem(wire_request_id);
    };
    let request_id = match RequestId::new(wire_request_id.clone()) {
        Ok(request_id) => request_id,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let operation = match tracedecay_application::feedback::feedback_surface_operation(
        surface_operation.as_str(),
    )
    .and_then(|operation| {
        operation.map_or_else(
            || {
                tracedecay_application::retrieval::catalog::primitive_read_operation(
                    surface_operation.as_str(),
                )
            },
            |operation| Ok(Some(operation)),
        )
    }) {
        Ok(Some(operation)) => operation,
        _ => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let result = match dispatch
        .dispatch_transport(
            request_id,
            operation,
            request,
            observed_at,
            deadline,
            cancellation,
        )
        .await
    {
        Ok(result) => result,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    match feedback_invocation_result(result) {
        Ok(result) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::Primitive {
                scope: result.scope,
                result: DaemonFeedbackResult::from_application(result.evidence),
            },
        ),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

/// Bounded operation outcomes. LSP payloads remain protocol frames, not an
/// unrestricted stream or arbitrary daemon-socket response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DaemonInvocationOutcome {
    GitPreview {
        scope: ResolvedScope,
        preview: DaemonGitPreviewResult,
    },
    GitApply {
        scope: ResolvedScope,
        effect: DaemonGitEffectResult,
    },
    Feedback {
        scope: ResolvedScope,
        result: DaemonFeedbackResult,
    },
    Primitive {
        scope: ResolvedScope,
        result: DaemonFeedbackResult,
    },
    ObservationAccepted,
    ApplicationProblem {
        problem: ApplicationProblem,
    },
    LspOpened {
        session: DaemonLspSessionAccess,
        expires_at_ms: u64,
    },
    LspFrameAccepted {
        backpressured: bool,
        closed: bool,
    },
    LspFrame {
        frame: Option<String>,
        closed: bool,
    },
    LspAcknowledged {
        acknowledged: bool,
    },
    LspDetached,
    Problem {
        problem: DaemonInvocationProblem,
    },
}

impl DaemonInvocationResponse {
    pub(crate) fn problem(request_id: impl Into<String>, problem: DaemonInvocationProblem) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            outcome: DaemonInvocationOutcome::Problem { problem },
        }
    }

    pub(crate) fn application_problem(
        request_id: impl Into<String>,
        problem: ApplicationProblem,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            outcome: DaemonInvocationOutcome::ApplicationProblem { problem },
        }
    }

    fn lsp_opened(request_id: String, session: DaemonLspSessionAccess, expires_at_ms: u64) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id,
            outcome: DaemonInvocationOutcome::LspOpened {
                session,
                expires_at_ms,
            },
        }
    }

    fn with_outcome(request_id: String, outcome: DaemonInvocationOutcome) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id,
            outcome,
        }
    }
}

/// Retained daemon state for the typed LSP invocation operations.
#[derive(Clone)]
pub(crate) struct DaemonInvocationService {
    lsp_sessions: Arc<Mutex<BTreeMap<LspSessionId, RuntimeLspSession>>>,
    feedback_runtimes: Arc<Mutex<BTreeMap<PathBuf, RegisteredFeedbackRuntime>>>,
    feedback_cycles: Arc<Mutex<BTreeMap<PathBuf, Arc<Pr12FeedbackCycleRuntime>>>>,
    primitive_runtimes: Arc<Mutex<BTreeMap<PathBuf, Pr12PrimitiveProjectRuntime>>>,
    lsp_owners: Arc<Mutex<BTreeMap<PathBuf, DaemonLspInvocationOwner>>>,
    advisory_runtimes: Arc<Mutex<BTreeMap<PathBuf, Arc<dyn Any + Send + Sync>>>>,
    semantic_runtimes:
        Arc<Mutex<BTreeMap<PathBuf, crate::semantic_code::DaemonSemanticRuntimeHandleV1>>>,
    operation_events: OperationEventAuthority,
}

impl Default for DaemonInvocationService {
    fn default() -> Self {
        Self {
            lsp_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            feedback_runtimes: Arc::new(Mutex::new(BTreeMap::new())),
            feedback_cycles: Arc::new(Mutex::new(BTreeMap::new())),
            primitive_runtimes: Arc::new(Mutex::new(BTreeMap::new())),
            lsp_owners: Arc::new(Mutex::new(BTreeMap::new())),
            advisory_runtimes: Arc::new(Mutex::new(BTreeMap::new())),
            semantic_runtimes: Arc::new(Mutex::new(BTreeMap::new())),
            operation_events: daemon_operation_event_authority(),
        }
    }
}

struct RegisteredFeedbackRuntime {
    project_id: ProjectId,
    runtime: Arc<Pr12FeedbackRuntime>,
}

#[derive(Debug, Error)]
pub(crate) enum DaemonFeedbackRuntimeRegistrationError {
    #[error("a PR12 feedback runtime is already mounted for this project database")]
    AlreadyRegistered,
    #[error("the PR12 feedback runtime must be mounted before its cycle")]
    MissingRuntime,
    #[error("the PR12 feedback runtime could not be opened")]
    Runtime(#[from] Pr12FeedbackRuntimeError),
    #[error("the PR12 feedback cycle runtime could not be opened")]
    Cycle(#[from] Pr12FeedbackCycleRuntimeError),
    #[error("the PR11 policy evaluator composition is invalid")]
    Policy(#[from] ApplicationContractError),
}

#[derive(Clone)]
pub(crate) struct DaemonFeedbackRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonFeedbackRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    /// Registers feedback readers from the authoritative admission result.
    pub(crate) async fn open_and_register(
        &self,
        database: Database,
        project_root: PathBuf,
        scope: ResolvedScope,
        access: ProjectSourceAccessSnapshot,
    ) -> Result<ProjectFeedbackStore, DaemonFeedbackRuntimeRegistrationError> {
        let mut runtimes = self.service.feedback_runtimes.lock().await;
        if runtimes.contains_key(&project_root) {
            return Err(DaemonFeedbackRuntimeRegistrationError::AlreadyRegistered);
        }
        let project_id = scope.project_id.clone();
        let runtime = Arc::new(open_pr12_feedback_runtime(
            database,
            project_root.clone(),
            scope,
            access,
        )?);
        let publications = runtime.publication_store();
        runtimes.insert(
            project_root.clone(),
            RegisteredFeedbackRuntime {
                project_id,
                runtime,
            },
        );
        Ok(publications)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_cycle_and_register(
        &self,
        project_root: PathBuf,
        database: Database,
        runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
        policy_context: PolicyEvaluationContextV1,
        evidence_horizon: PolicyEvidenceHorizonV1,
        evaluated_at: UtcMicros,
        provider_candidates: Vec<(DiagnosticProviderIdentity, AnalyzerAdmissionInputV1)>,
        graph: Arc<TraceDecay>,
        affected_tests: Arc<dyn AffectedTestsRetrievalPort + Send + Sync>,
        operation: ApplicationOperation,
        graph_operation: ApplicationOperation,
        tests_operation: ApplicationOperation,
        lsp_input: Pr12FeedbackCycleLspInput,
    ) -> Result<Arc<Pr12FeedbackCycleRuntime>, DaemonFeedbackRuntimeRegistrationError> {
        let policy = PolicyEvaluatorCompositionV1::from_application_catalog()?;
        let correlation_state = evidence_horizon.routing_state();
        let correlation_policy = operation.evaluate_policy_route(
            &policy,
            PolicyConsumerV1::LocalLiveCorrelation,
            &policy_context,
            correlation_state,
            TruthFreshnessRequirementV1::FreshOrPartial,
            Some(evidence_horizon),
            evaluated_at,
        )?;
        let provider_admissions = provider_candidates
            .into_iter()
            .map(|(identity, input)| {
                AnalyzerAdmittedDiagnosticProviderV1::evaluate_current_plan20_snapshot(
                    &policy,
                    &policy_context,
                    identity,
                    input,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let feedback = self
            .service
            .feedback_runtime(Some(&project_root))
            .await
            .ok_or(DaemonFeedbackRuntimeRegistrationError::MissingRuntime)?;
        let observations = feedback.observation_port();
        let runtime = open_pr12_feedback_cycle_runtime(
            database,
            feedback,
            runtime_state,
            correlation_policy,
            provider_admissions,
            graph,
            affected_tests,
            observations,
            operation,
            graph_operation,
            tests_operation,
            lsp_input,
        )?;
        self.service
            .feedback_cycles
            .lock()
            .await
            .insert(project_root, runtime.clone());
        Ok(runtime)
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonPrimitiveRuntimeRegistrationError {
    #[error("a PR12 primitive runtime is already mounted for this project")]
    AlreadyRegistered,
}

/// Central project-open registration for the owned primitive facade.
#[derive(Clone)]
pub(crate) struct DaemonPrimitiveRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonPrimitiveRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    /// Retains the already-opened project runtime as its teardown owner.
    /// Scope/access were bound by the concrete project-open factory.
    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        project_runtime: Pr12PrimitiveProjectRuntime,
    ) -> Result<Arc<dyn Pr12PrimitiveDispatch>, DaemonPrimitiveRuntimeRegistrationError> {
        let mut runtimes = self.service.primitive_runtimes.lock().await;
        if runtimes.contains_key(&project_root) {
            return Err(DaemonPrimitiveRuntimeRegistrationError::AlreadyRegistered);
        }
        let dispatch = project_runtime.dispatch();
        runtimes.insert(project_root, project_runtime);
        Ok(dispatch)
    }

    pub(crate) async fn unregister(&self, project_root: &Path) -> bool {
        let runtime = self
            .service
            .primitive_runtimes
            .lock()
            .await
            .remove(project_root);
        runtime.is_some_and(|runtime| {
            runtime.teardown();
            true
        })
    }
}

#[derive(Clone)]
pub(crate) struct DaemonLspOwnerRegistrar {
    service: DaemonInvocationService,
}

impl DaemonLspOwnerRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register_lsp_owner(
        &self,
        project_root: PathBuf,
        owner: DaemonLspInvocationOwner,
    ) {
        self.service.install_lsp_owner(project_root, owner).await;
    }

    pub(crate) async fn register_pr12_factory(
        &self,
        project_root: PathBuf,
        factory: Arc<Pr12LspSessionFactory>,
    ) {
        self.register_lsp_owner(project_root, DaemonLspInvocationOwner::new(factory))
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build_and_register_pr12(
        &self,
        project_root: PathBuf,
        database: Database,
        runtime: tokio::runtime::Handle,
        diagnostic_broker: Arc<Mutex<DiagnosticBroker>>,
        language: &str,
        root_uri: String,
        timeouts: LspRefreshTimeouts,
        diagnostics_quiet_window: Duration,
        gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
    ) -> Result<Arc<Pr12LspSessionFactory>, TraceDecayError> {
        let feedback_runtime = self
            .service
            .feedback_runtime(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "PR12 feedback runtime is not registered for the project".to_owned(),
            })?;
        let feedback_cycle = self
            .service
            .feedback_cycle(Some(&project_root))
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "PR12 feedback cycle is not registered for the project".to_owned(),
            })?;
        let semantics = pr12_production_semantic_authorities(
            runtime.clone(),
            diagnostic_broker.clone(),
            database.clone(),
            language,
            project_root.clone(),
            root_uri,
            timeouts,
        )
        .await?;
        let factory = Arc::new(
            pr12_lsp_session_factory(
                runtime,
                feedback_runtime,
                database,
                move |_| feedback_cycle.context_projection_input(),
                semantics.semantics,
                diagnostic_broker,
                diagnostics_quiet_window,
                semantics.cancellation,
                gateway_capabilities,
                upstream_capabilities,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("could not construct PR12 LSP session factory: {error:?}"),
            })?,
        );
        self.register_pr12_factory(project_root, factory.clone())
            .await;
        Ok(factory)
    }
}

#[derive(Debug, Error)]
pub(crate) enum DaemonAdvisoryRuntimeRegistrationError {
    #[error("a PR13 advisory runtime is already mounted for this project")]
    AlreadyRegistered,
    #[error("the shared PR12 feedback readers must be registered before PR13")]
    MissingFeedbackRuntime,
    #[error("the PR13 production authorities could not be opened")]
    Production(#[from] Pr13AdvisoryProductionOpenErrorV1),
    #[error(transparent)]
    Startup(#[from] Pr13AdvisoryDaemonStartupErrorV1),
}

#[derive(Clone)]
pub(crate) struct DaemonAdvisoryRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonAdvisoryRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register<GR, GA, CS, CE, PE, PC>(
        &self,
        project_root: PathBuf,
        input: Pr13AdvisoryRuntimeOpenV1,
        providers: Pr13AdvisoryProviderAuthoritiesV1<GR, GA, CS, CE, PE, PC>,
        lsp_session_factory: Arc<Pr12LspSessionFactory>,
        hook_delivery_port: Arc<
            dyn HookFeedbackDeliveryPortV1<Pr13AdvisoryHookLookupNoticeV1> + Send + Sync,
        >,
    ) -> Result<
        Arc<Pr13AdvisoryDaemonStartupRegistrationV1<GR, GA, CS, CE, PE, PC>>,
        DaemonAdvisoryRuntimeRegistrationError,
    >
    where
        GR: GitHubCurrentBranchRemapper + Send + Sync + 'static,
        GA: GitHubCanonicalReviewAnchorAuthorityV1 + Send + Sync + 'static,
        CS: CiReadOnlyProviderArchiveV1 + Send + Sync + 'static,
        CE: CiExactEvidenceAuthorityV1<CS::Record> + Send + Sync + 'static,
        PE: CanonicalProximityEvidenceAuthorityV1 + Send + Sync + 'static,
        PC: ConfigurationControlStore + Send + Sync + 'static,
    {
        let project_id = input.resolved_scope.project_id.clone();
        let feedback_registered = self
            .service
            .feedback_runtimes
            .lock()
            .await
            .get(&project_root)
            .is_some_and(|runtime| runtime.project_id == project_id);
        if !feedback_registered {
            return Err(DaemonAdvisoryRuntimeRegistrationError::MissingFeedbackRuntime);
        }
        let mut runtimes = self.service.advisory_runtimes.lock().await;
        if runtimes.contains_key(&project_root) {
            return Err(DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered);
        }
        let registration = Arc::new(register_pr13_advisory_daemon_startup(
            input,
            providers,
            lsp_session_factory.clone(),
            hook_delivery_port,
        )?);
        let registered_root = project_root.clone();
        runtimes.insert(project_root, registration.clone());
        drop(runtimes);
        self.service
            .install_lsp_owner(
                registered_root,
                DaemonLspInvocationOwner::new(lsp_session_factory),
            )
            .await;
        Ok(registration)
    }

    pub(crate) async fn register_production(
        &self,
        project_root: PathBuf,
        input: Pr13AdvisoryRuntimeOpenV1,
        production: Pr13AdvisoryProductionOpenV1,
        lsp_session_factory: Arc<Pr12LspSessionFactory>,
    ) -> Result<
        Arc<Pr13AdvisoryProductionStartupRegistrationV1>,
        DaemonAdvisoryRuntimeRegistrationError,
    > {
        let authorities = open_pr13_advisory_production_authorities(production)?;
        let (providers, hook_delivery_port) = authorities.into_registrar_parts();
        self.register(
            project_root,
            input,
            providers,
            lsp_session_factory,
            hook_delivery_port,
        )
        .await
    }
}

/// Mounts one project's semantic-runtime scheduling handle as daemon-private
/// retained state. Semantic scheduling is never a wire operation: the daemon
/// consults the retained handle for status/coverage and to hand work to the
/// bounded background scheduler, and clients observe only the typed
/// freshness/coverage that ordinary operations already report.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DaemonSemanticRuntimeRegistrationError {
    #[error("a semantic runtime scheduler is already mounted for this project")]
    AlreadyRegistered,
}

pub(crate) struct DaemonSemanticRuntimeRegistrar {
    service: DaemonInvocationService,
}

impl DaemonSemanticRuntimeRegistrar {
    pub(crate) fn new(service: &DaemonInvocationService) -> Self {
        Self {
            service: service.clone(),
        }
    }

    pub(crate) async fn register(
        &self,
        project_root: PathBuf,
        handle: crate::semantic_code::DaemonSemanticRuntimeHandleV1,
    ) -> Result<(), DaemonSemanticRuntimeRegistrationError> {
        let mut runtimes = self.service.semantic_runtimes.lock().await;
        if runtimes.contains_key(&project_root) {
            return Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered);
        }
        runtimes.insert(project_root, handle);
        Ok(())
    }
}

impl DaemonInvocationService {
    /// Returns the retained semantic scheduling handle for `project_root`,
    /// or the sole mounted handle when no root is given and exactly one
    /// project is registered.
    pub(crate) async fn semantic_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<crate::semantic_code::DaemonSemanticRuntimeHandleV1> {
        let runtimes = self.semantic_runtimes.lock().await;
        match project_root {
            Some(root) => runtimes.get(root).cloned(),
            None if runtimes.len() == 1 => runtimes.values().next().cloned(),
            None => None,
        }
    }
}

struct RuntimeLspSession {
    expires_at_ms: u64,
    actor: RuntimeLspActor,
}

type RuntimeLspActor = DaemonLspRuntimeSession;

#[derive(Clone)]
pub(crate) struct DaemonLspInvocationOwner {
    factory: Arc<Pr12LspSessionFactory>,
}

impl DaemonLspInvocationOwner {
    pub(crate) fn new(factory: Arc<Pr12LspSessionFactory>) -> Self {
        Self { factory }
    }
}

/// Admission binds a session to the root independently resolved by the daemon
/// before this protocol is invoked. Client root hints are never consulted.
#[derive(Clone, Debug)]
struct AdmittedRootSessionAdmission {
    root: AdmittedRoot,
}

impl LspSessionAdmissionPort for AdmittedRootSessionAdmission {
    fn admit_lsp_session(
        &self,
        _request: &LspSessionOpenRequest,
        now_ms: u64,
    ) -> Result<AuthorizedLspSession, LspEndpointError> {
        let mut session_bytes = [0_u8; 16];
        let mut credential_bytes = [0_u8; 32];
        getrandom::getrandom(&mut session_bytes)
            .map_err(|_| LspEndpointError::AdmissionRejected)?;
        getrandom::getrandom(&mut credential_bytes)
            .map_err(|_| LspEndpointError::AdmissionRejected)?;
        let session_id = LspSessionId::new(format!("lsp-{}", hex::encode(session_bytes)))?;
        let credential = LspSessionCredential::new(credential_bytes.to_vec())?;
        Ok(AuthorizedLspSession {
            session_id,
            credential,
            root: self.root.clone(),
            expires_at_ms: now_ms.saturating_add(LSP_SESSION_TTL_MS),
        })
    }
}

#[derive(Clone)]
struct SharedGitTransactionPort(Arc<DaemonProjectGitIndexTransactionService>);

impl GitIndexTransactionPort for SharedGitTransactionPort {
    fn preview(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError> {
        self.0.preview(request)
    }

    fn apply(
        &self,
        request: &GitIndexApplyRequestV1,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        self.0.apply(request)
    }

    fn recover(
        &self,
        request: &GitIndexRecoveryRequestV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexTransactionPortError> {
        self.0.recover(request)
    }
}

async fn execute_git_preview(
    operation_events: &OperationEventAuthority,
    wire_request_id: String,
    owner: Option<DaemonGitInvocationOwner>,
    request: GitPreviewSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    if request.repository_snapshot.project_id != owner.project_id {
        return concealed_application_problem(wire_request_id);
    }
    let service = owner.service;
    let request = match build_git_preview_request(
        &wire_request_id,
        request,
        observed_at,
        deadline,
        cancellation,
    ) {
        Ok(request) => request,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let scope = request.context.scope().clone();
    let emitter = match operation_events
        .begin(&request.context, OperationKind::GitPreview, observed_at)
        .await
    {
        Ok(emitter) => emitter,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let _ = emitter.progress(0, Some(1)).await;
    let started_at = request.observed_at;
    let effective_deadline = request.context.deadline().clone();
    let result = tokio::task::spawn_blocking(move || {
        GitIndexTransactionService::new(SharedGitTransactionPort(service)).preview(request)
    })
    .await;
    let response = match result {
        Ok(Ok(preview)) => match DaemonGitPreviewResult::from_application(preview) {
            Ok(preview) => DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::GitPreview { scope, preview },
            ),
            Err(_) => DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            ),
        },
        Ok(Err(error)) => application_problem(wire_request_id, map_git_error(error)),
        Err(_) => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    };
    publish_invocation_terminal(&emitter, &response, started_at, effective_deadline).await;
    response
}

async fn execute_git_apply(
    operation_events: &OperationEventAuthority,
    wire_request_id: String,
    owner: Option<DaemonGitInvocationOwner>,
    request: GitApplySurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(owner) = owner else {
        return concealed_application_problem(wire_request_id);
    };
    if request.preview.repository_snapshot.project_id != owner.project_id {
        return concealed_application_problem(wire_request_id);
    }
    let service = owner.service;
    let request = match build_git_apply_request(
        &wire_request_id,
        request,
        observed_at,
        deadline,
        cancellation,
    ) {
        Ok(request) => request,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let scope = request.context.scope().clone();
    let emitter = match operation_events
        .begin(&request.context, OperationKind::GitApply, observed_at)
        .await
    {
        Ok(emitter) => emitter,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let _ = emitter.progress(0, Some(1)).await;
    let started_at = request.observed_at;
    let effective_deadline = request.context.deadline().clone();
    let result = tokio::task::spawn_blocking(move || {
        GitIndexTransactionService::new(SharedGitTransactionPort(service)).apply(request)
    })
    .await;
    let response = match result {
        Ok(Ok(effect)) => match DaemonGitEffectResult::from_application(effect) {
            Ok(effect) => DaemonInvocationResponse::with_outcome(
                wire_request_id,
                DaemonInvocationOutcome::GitApply { scope, effect },
            ),
            Err(_) => DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            ),
        },
        Ok(Err(error)) => application_problem(wire_request_id, map_git_error(error)),
        Err(_) => {
            DaemonInvocationResponse::problem(wire_request_id, DaemonInvocationProblem::Unavailable)
        }
    };
    publish_invocation_terminal(&emitter, &response, started_at, effective_deadline).await;
    response
}

async fn publish_invocation_terminal(
    emitter: &OperationEmitter,
    response: &DaemonInvocationResponse,
    started_at: UtcMicros,
    effective_deadline: Deadline,
) {
    let ended_at = current_micros();
    let ended_at = if ended_at < started_at {
        started_at
    } else {
        ended_at
    };
    let receipt = invocation_operation_receipt(response).unwrap_or_else(|| OperationReceipt {
        started_at,
        ended_at,
        effective_deadline,
        cancellation: None,
        budget: OperationBudgetUsage::default(),
        termination: OperationTermination::Failed,
    });
    if receipt.termination == OperationTermination::Completed {
        let _ = emitter.progress(1, Some(1)).await;
    }
    let _ = emitter.terminal(receipt).await;
}

fn invocation_operation_receipt(response: &DaemonInvocationResponse) -> Option<OperationReceipt> {
    match &response.outcome {
        DaemonInvocationOutcome::GitPreview { preview, .. } => Some(preview.execution.clone()),
        DaemonInvocationOutcome::GitApply { effect, .. } => Some(effect.execution.clone()),
        DaemonInvocationOutcome::Feedback { result, .. } => Some(result.execution.clone()),
        _ => None,
    }
}

fn build_git_preview_request(
    request_id: &str,
    request: GitPreviewSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<GitIndexPreviewRequestV1, ApplicationProblem> {
    let preview_id = mint_git_preview_id()?;
    let mut selected_hunks = request.selected_hunks;
    for hunk in &mut selected_hunks {
        hunk.preview_id = preview_id.as_str().to_owned();
    }
    let (context, authority, binding) = git_request_authority(
        request_id,
        &request.repository_snapshot,
        request.operation,
        deadline,
        cancellation,
        observed_at,
    )?;
    Ok(GitIndexPreviewRequestV1 {
        context,
        authority,
        binding,
        preview_id,
        repository_snapshot: request.repository_snapshot,
        selected_hunks,
        commit_intent: request.commit_intent,
        observed_at,
    })
}

fn mint_git_preview_id() -> Result<GitIndexPreviewId, ApplicationProblem> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "git_index.preview_identity_unavailable".to_owned(),
            message: "The daemon could not mint a Git preview identity".to_owned(),
        })
    })?;
    GitIndexPreviewId::new(format!("preview.{}", hex::encode(bytes))).map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "git_index.preview_identity_unavailable".to_owned(),
            message: "The daemon could not mint a Git preview identity".to_owned(),
        })
    })
}

fn build_git_apply_request(
    request_id: &str,
    request: GitApplySurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<GitIndexApplyRequestV1, ApplicationProblem> {
    let (context, authority, binding) = git_request_authority(
        request_id,
        &request.preview.repository_snapshot,
        request.preview.operation,
        deadline,
        cancellation,
        observed_at,
    )?;
    let (_, configuration_digest) = daemon_git_policy_evidence().map_err(map_git_port_problem)?;
    Ok(GitIndexApplyRequestV1 {
        context,
        authority: authority.clone(),
        binding,
        preview_id: request.preview.preview_id,
        preview_digest: request.preview.preview_digest,
        idempotency_key: request.idempotency_key,
        proof: GitIndexEffectProofV1 {
            policy_digest: authority.policy.digest,
            configuration_digest,
            catalog_digest: stable_digest(&"tracedecay.application.catalog.v1")?,
            privacy_digest: stable_digest(&"tracedecay.application.privacy.v1")?,
            external_proof: None,
        },
        observed_at,
    })
}

fn git_request_authority(
    request_id: &str,
    snapshot: &tracedecay_domain::RepositoryStateSnapshotV1,
    operation: GitIndexTransactionOperationV1,
    deadline: Deadline,
    cancellation: CancellationContext,
    observed_at: UtcMicros,
) -> Result<(RequestContext, AuthorityReceipt, GitIndexOperationBindingV1), ApplicationProblem> {
    if cancellation.is_cancelled() {
        return Err(ApplicationProblem::cancelled_before_admission());
    }
    if deadline.is_elapsed_at(now_micros()) || deadline.is_elapsed_at(observed_at) {
        return Err(ApplicationProblem::timed_out_before_admission());
    }
    snapshot.validate().map_err(|_| invalid_git_request())?;
    let worktree_id = snapshot
        .worktree_id
        .clone()
        .ok_or_else(invalid_git_request)?;
    let reference = match &snapshot.head {
        GitHeadStateV1::Attached { branch, .. } | GitHeadStateV1::Unborn { branch } => {
            Some(RefId::new(branch.clone()).map_err(|_| invalid_git_request())?)
        }
        GitHeadStateV1::Detached { .. } => None,
    };
    let scope = ResolvedScope::new(
        snapshot.project_id.clone(),
        snapshot.repository_id.clone(),
        worktree_id,
        reference,
    )
    .map_err(|_| invalid_git_request())?;
    let (capability, use_case) = git_operation_ids(operation);
    let capability_id = CapabilityId::new(capability).map_err(|_| invalid_git_request())?;
    let use_case_id = UseCaseId::new(use_case).map_err(|_| invalid_git_request())?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.git.{request_id}"))
            .map_err(|_| invalid_git_request())?,
        1,
        stable_digest(&("tracedecay.daemon.git-grant.v1", request_id, &scope))?,
        ActorId::new("actor.tracedecay-daemon").map_err(|_| invalid_git_request())?,
        observed_at,
        deadline.expires_at,
        scope.clone(),
        std::collections::BTreeSet::from([capability_id.clone()]),
        std::collections::BTreeSet::from([use_case_id.clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid_git_request())?;
    let context = RequestContext::new(
        ActorId::new("actor.tracedecay-client").map_err(|_| invalid_git_request())?,
        scope,
        grant,
        RequestId::new(request_id).map_err(|_| invalid_git_request())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid_git_request())?;
    let (policy_digest, _) = daemon_git_policy_evidence().map_err(map_git_port_problem)?;
    let authority = AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.git-index.v1",
            1,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.git-policy.v1")
                .map_err(|_| invalid_git_request())?,
        )
        .map_err(|_| invalid_git_request())?,
        observed_at,
    )
    .map_err(|_| invalid_git_request())?;
    Ok((
        context,
        authority,
        GitIndexOperationBindingV1 {
            capability_id,
            use_case_id,
            operation,
        },
    ))
}

fn git_operation_ids(operation: GitIndexTransactionOperationV1) -> (&'static str, &'static str) {
    match operation {
        GitIndexTransactionOperationV1::StageHunks => {
            ("capability.git.stage-hunks", "use-case.git.stage-hunks")
        }
        GitIndexTransactionOperationV1::UnstageHunks => {
            ("capability.git.unstage-hunks", "use-case.git.unstage-hunks")
        }
        GitIndexTransactionOperationV1::CommitIndex => {
            ("capability.git.commit-index", "use-case.git.commit-index")
        }
    }
}

fn stable_digest(material: &impl Serialize) -> Result<ManifestDigest, ApplicationProblem> {
    canonical_sha256(material).map_err(|_| invalid_git_request())
}

fn now_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_micros())
                .unwrap_or(0),
        )
        .unwrap_or(i64::MAX),
    )
}

fn invalid_git_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "git_index.invalid_request".to_owned(),
            message: "The Git index request is invalid".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

fn map_git_error(error: GitIndexTransactionApplicationError) -> ApplicationProblem {
    match error {
        GitIndexTransactionApplicationError::Contract(_) => invalid_git_request(),
        GitIndexTransactionApplicationError::Port(error) => map_git_port_problem(error),
    }
}

fn map_git_port_problem(error: GitIndexTransactionPortError) -> ApplicationProblem {
    match error {
        GitIndexTransactionPortError::StalePreview => ApplicationProblem::stale(SafeDiagnostic {
            code: "git_index.stale_preview".to_owned(),
            message: "The Git index preview is stale or absent".to_owned(),
        }),
        GitIndexTransactionPortError::PolicyDenied => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        GitIndexTransactionPortError::IdempotencyConflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "git_index.idempotency_conflict".to_owned(),
                message: "The idempotency key is already bound to another input".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
        GitIndexTransactionPortError::Unsupported => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "git_index.unsupported".to_owned(),
                message: "The repository state does not support this Git index operation"
                    .to_owned(),
            },
            retry: RetryDirective::AfterRevalidate,
            legal_actions: Vec::new(),
        },
        GitIndexTransactionPortError::DaemonUnavailable
        | GitIndexTransactionPortError::RecoveryRequired
        | GitIndexTransactionPortError::NeedsInspection
        | GitIndexTransactionPortError::NativeFailure => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: match error {
                    GitIndexTransactionPortError::RecoveryRequired => "git_index.recovery_required",
                    GitIndexTransactionPortError::NeedsInspection => "git_index.needs_inspection",
                    GitIndexTransactionPortError::NativeFailure => "git_index.native_failure",
                    _ => "git_index.unavailable",
                }
                .to_owned(),
                message: "The Git index transaction owner is not ready".to_owned(),
            })
        }
    }
}

fn application_problem(
    request_id: String,
    problem: ApplicationProblem,
) -> DaemonInvocationResponse {
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::ApplicationProblem { problem },
    )
}

fn concealed_application_problem(request_id: String) -> DaemonInvocationResponse {
    application_problem(
        request_id,
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    )
}

impl DaemonInvocationService {
    pub(crate) fn operation_events(&self) -> OperationEventAuthority {
        self.operation_events.clone()
    }

    /// Exact in-process handler call for a daemon-retained PR12 primitive.
    /// Callers must supply the authenticated request context minted during
    /// project admission; no path or client selector is resolved here.
    pub(crate) async fn dispatch_pr12_primitive(
        &self,
        project_root: &Path,
        invocation: Pr12PrimitiveInvocation,
        context: RequestContext,
        observed_at: UtcMicros,
    ) -> Option<ApplicationResult<serde_json::Value>> {
        let dispatch = self
            .primitive_runtimes
            .lock()
            .await
            .get(project_root)
            .map(Pr12PrimitiveProjectRuntime::dispatch)?;
        Some(dispatch.dispatch(invocation, context, observed_at).await)
    }

    pub(crate) async fn feedback_owner(
        &self,
        project_root: Option<&Path>,
    ) -> Option<DaemonFeedbackInvocationOwner> {
        let project_root = project_root?;
        let runtimes = self.feedback_runtimes.lock().await;
        let registered = runtimes.get(project_root)?;
        Some(DaemonFeedbackInvocationOwner::new(
            registered.project_id.clone(),
            registered.runtime.owner(),
        ))
    }

    pub(crate) async fn feedback_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<Pr12FeedbackRuntime>> {
        let project_root = project_root?;
        self.feedback_runtimes
            .lock()
            .await
            .get(project_root)
            .map(|registered| registered.runtime.clone())
    }

    pub(crate) async fn feedback_cycle(
        &self,
        project_root: Option<&Path>,
    ) -> Option<Arc<Pr12FeedbackCycleRuntime>> {
        let project_root = project_root?;
        self.feedback_cycles.lock().await.get(project_root).cloned()
    }

    pub(crate) async fn feedback_publication_store(
        &self,
        project_root: Option<&Path>,
    ) -> Option<ProjectFeedbackStore> {
        let project_root = project_root?;
        self.feedback_runtimes
            .lock()
            .await
            .get(project_root)
            .map(|registered| registered.runtime.publication_store())
    }

    async fn install_lsp_owner(&self, project_root: PathBuf, owner: DaemonLspInvocationOwner) {
        self.lsp_owners.lock().await.insert(project_root, owner);
    }

    pub(crate) async fn lsp_owner(
        &self,
        project_root: Option<&Path>,
    ) -> Option<DaemonLspInvocationOwner> {
        let project_root = project_root?;
        self.lsp_owners.lock().await.get(project_root).cloned()
    }

    /// Executes a closed request after daemon socket authentication. `root` is
    /// supplied only after the daemon has opened and authorized the project;
    /// existing LSP session operations do not re-resolve client paths.
    pub(crate) async fn invoke(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        project_root: Option<&Path>,
        root: Option<AdmittedRoot>,
        git_service: Option<DaemonGitInvocationOwner>,
        request: DaemonInvocationRequest,
    ) -> DaemonInvocationResponse {
        let request_id = request.request_id.clone();
        let operation = request.operation();
        let delivery_route = request.delivery_route;
        let feedback_runtime = self.feedback_runtime(project_root).await;
        let observations = feedback_runtime
            .as_ref()
            .map(|runtime| runtime.source_observation_port());
        let observation_subject = plan26_invocation_subject(&request_id, operation, delivery_route);
        if let Err(problem) = request.validate() {
            if plan26_observable_operation(operation) {
                emit_plan26_invocation_event(
                    observations.as_ref(),
                    observation_subject.as_ref(),
                    current_micros(),
                    Plan26FeedbackSourceEventV1::ArgumentRejected {
                        operation: plan26_feedback_operation(operation),
                        outcome: Plan26FeedbackOutcomeV1::Rejected,
                    },
                );
            }
            return DaemonInvocationResponse::problem(request_id, problem);
        }
        let dispatched_at = current_micros();
        if plan26_observable_operation(operation) {
            emit_plan26_invocation_event(
                observations.as_ref(),
                observation_subject.as_ref(),
                dispatched_at,
                Plan26FeedbackSourceEventV1::Dispatch {
                    operation: plan26_feedback_operation(operation),
                    outcome: Plan26FeedbackOutcomeV1::Admitted,
                    capacity: 1,
                    admitted: 1,
                },
            );
        }
        let now_ms = now_millis();
        self.expire_sessions(now_ms).await;
        let feedback_service = self.feedback_owner(project_root).await;
        let lsp_owner = self.lsp_owner(project_root).await;

        let response = match request.payload {
            DaemonInvocationPayload::GitPreview {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_git_preview(
                    &self.operation_events,
                    request_id,
                    git_service,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::GitApply {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_git_apply(
                    &self.operation_events,
                    request_id,
                    git_service,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackDiagnostics {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackDiagnostics,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackGet {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackGet,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackExpand {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackExpand,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackList {
                request_handle,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_feedback(
                    request_id,
                    feedback_service,
                    DaemonInvocationOperation::FeedbackList,
                    request_handle,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::FeedbackObserve {
                subject_digest,
                observed_at,
                event,
            } => {
                if let Some(observations) = observations.as_ref() {
                    observations.observe_source_event_for_subject(
                        subject_digest,
                        observed_at,
                        event,
                    );
                    DaemonInvocationResponse::with_outcome(
                        request_id,
                        DaemonInvocationOutcome::ObservationAccepted,
                    )
                } else {
                    DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    )
                }
            }
            DaemonInvocationPayload::PrimitiveImpact {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::FeedbackImpact,
                    Pr12PrimitiveRequest::Impact(request),
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::PrimitiveAffectedTests {
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::AffectedTests,
                    Pr12PrimitiveRequest::AffectedFileTests(request),
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::PrimitiveTestResults {
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    crate::application_surface::ApplicationSurfaceOperation::TestResults,
                    Pr12PrimitiveRequest::RecentTestResults,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::PrimitiveRead {
                surface_operation,
                request,
                observed_at,
                deadline,
                cancellation,
            } => {
                execute_primitive(
                    self,
                    project_root,
                    request_id,
                    surface_operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation,
                )
                .await
            }
            DaemonInvocationPayload::LspOpen {
                client_revision,
                requested_root_uri,
                workspace_folders,
            } => {
                self.open_lsp_session(
                    lsp_registry,
                    root,
                    request_id,
                    client_revision,
                    requested_root_uri,
                    workspace_folders,
                    now_ms,
                    lsp_owner,
                )
                .await
            }
            DaemonInvocationPayload::LspFrame { session, frame } => {
                self.send_lsp_frame(lsp_registry, request_id, session, frame, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspPoll { session } => {
                self.poll_lsp_frame(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspAcknowledge { session } => {
                self.acknowledge_lsp_frame(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspDetach { session } => {
                self.detach_lsp_session(lsp_registry, request_id, session, now_ms)
                    .await
            }
        };
        if plan26_observable_operation(operation) {
            observe_plan26_invocation_response(
                observations.as_ref(),
                observation_subject.as_ref(),
                operation,
                delivery_route,
                dispatched_at,
                &response,
            );
        }
        response
    }

    pub(crate) async fn expire_all(&self) {
        self.lsp_sessions.lock().await.clear();
        self.feedback_runtimes.lock().await.clear();
        self.feedback_cycles.lock().await.clear();
        self.primitive_runtimes.lock().await.clear();
        let semantic_runtimes = std::mem::take(&mut *self.semantic_runtimes.lock().await);
        for handle in semantic_runtimes.into_values() {
            handle.cancel();
        }
        self.lsp_owners.lock().await.clear();
        self.advisory_runtimes.lock().await.clear();
        self.operation_events.expire_all().await;
    }

    async fn open_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        root: Option<AdmittedRoot>,
        request_id: String,
        client_revision: String,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
        now_ms: u64,
        lsp_owner: Option<DaemonLspInvocationOwner>,
    ) -> DaemonInvocationResponse {
        let Some(root) = root else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let Some(lsp_owner) = lsp_owner else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let request = LspSessionOpenRequest {
            requested_root_uri,
            workspace_folders,
            client_revision,
        };
        let access = {
            let mut registry = lsp_registry.lock().await;
            let existing = std::mem::take(&mut *registry);
            let mut endpoint = DaemonLspSessionEndpoint::with_registry(
                AdmittedRootSessionAdmission { root: root.clone() },
                existing,
            );
            let result = endpoint.open(request, now_ms);
            *registry = endpoint.into_registry();
            result
        };
        let access = match access {
            Ok(access) => access,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            }
        };
        let expires_at_ms = now_ms.saturating_add(LSP_SESSION_TTL_MS);
        let session_id = access.session_id().clone();
        let actor = runtime_lsp_actor(root, lsp_owner);
        self.lsp_sessions.lock().await.insert(
            session_id,
            RuntimeLspSession {
                expires_at_ms,
                actor,
            },
        );
        DaemonInvocationResponse::lsp_opened(
            request_id,
            DaemonLspSessionAccess::from_access(&access),
            expires_at_ms,
        )
    }

    async fn send_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        frame: String,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let dispatch = session.actor.handle_payload(frame.as_bytes(), now_ms);
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrameAccepted {
                backpressured: dispatch.backpressured,
                closed: dispatch.closed,
            },
        )
    }

    async fn poll_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let dispatch = session.actor.flush_due(now_ms);
        let frame = session
            .actor
            .poll_outbound()
            .and_then(|frame| std::str::from_utf8(frame).ok())
            .map(str::to_owned);
        let closed = dispatch.closed
            || matches!(
                session.actor.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            );
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrame { frame, closed },
        )
    }

    async fn acknowledge_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspAcknowledged {
                acknowledged: session.actor.acknowledge_outbound(),
            },
        )
    }

    async fn detach_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let endpoint_detached = {
            let mut registry = lsp_registry.lock().await;
            registry.detach(&access, now_ms).is_ok()
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if !endpoint_detached || session.actor.detach().is_err() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        DaemonInvocationResponse::with_outcome(request_id, DaemonInvocationOutcome::LspDetached)
    }

    async fn authenticate(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> Result<LspSessionAccess, DaemonInvocationProblem> {
        let access = session.into_access()?;
        let authenticated = {
            let mut registry = lsp_registry.lock().await;
            registry.authenticate(&access, now_ms).is_ok()
        };
        if authenticated {
            Ok(access)
        } else {
            self.lsp_sessions.lock().await.remove(access.session_id());
            Err(DaemonInvocationProblem::NotFoundOrNotAuthorized)
        }
    }

    async fn expire_sessions(&self, now_ms: u64) {
        self.lsp_sessions
            .lock()
            .await
            .retain(|_, session| session.expires_at_ms > now_ms);
    }
}

fn runtime_lsp_actor(root: AdmittedRoot, owner: DaemonLspInvocationOwner) -> RuntimeLspActor {
    owner.factory.open_session(root)
}

fn plan26_invocation_subject(
    request_id: &str,
    operation: DaemonInvocationOperation,
    route: Option<Plan26DeliveryRouteV1>,
) -> Option<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.feedback.transport-observation.v1",
        request_id,
        operation.as_str(),
        route,
    ))
    .ok()
}

fn plan26_observable_operation(operation: DaemonInvocationOperation) -> bool {
    matches!(
        operation,
        DaemonInvocationOperation::FeedbackDiagnostics
            | DaemonInvocationOperation::FeedbackGet
            | DaemonInvocationOperation::FeedbackExpand
            | DaemonInvocationOperation::FeedbackList
            | DaemonInvocationOperation::PrimitiveImpact
            | DaemonInvocationOperation::PrimitiveAffectedTests
            | DaemonInvocationOperation::PrimitiveTestResults
            | DaemonInvocationOperation::PrimitiveRead
    )
}

fn plan26_feedback_operation(operation: DaemonInvocationOperation) -> Plan26FeedbackOperationV1 {
    match operation {
        DaemonInvocationOperation::FeedbackDiagnostics => {
            Plan26FeedbackOperationV1::FeedbackDiagnostics
        }
        DaemonInvocationOperation::FeedbackGet => Plan26FeedbackOperationV1::FeedbackGet,
        DaemonInvocationOperation::FeedbackExpand => Plan26FeedbackOperationV1::FeedbackExpand,
        DaemonInvocationOperation::FeedbackList => Plan26FeedbackOperationV1::FeedbackList,
        DaemonInvocationOperation::FeedbackObserve => Plan26FeedbackOperationV1::FeedbackCycle,
        DaemonInvocationOperation::PrimitiveImpact => Plan26FeedbackOperationV1::PrimitiveImpact,
        DaemonInvocationOperation::PrimitiveAffectedTests => {
            Plan26FeedbackOperationV1::PrimitiveAffectedTests
        }
        DaemonInvocationOperation::PrimitiveTestResults => {
            Plan26FeedbackOperationV1::PrimitiveTestResults
        }
        DaemonInvocationOperation::PrimitiveRead => Plan26FeedbackOperationV1::FeedbackCycle,
        DaemonInvocationOperation::LspOpen
        | DaemonInvocationOperation::LspFrame
        | DaemonInvocationOperation::LspPoll
        | DaemonInvocationOperation::LspAcknowledge
        | DaemonInvocationOperation::LspDetach => Plan26FeedbackOperationV1::LspSession,
        DaemonInvocationOperation::GitPreview | DaemonInvocationOperation::GitApply => {
            Plan26FeedbackOperationV1::FeedbackCycle
        }
    }
}

fn emit_plan26_invocation_event(
    observations: Option<&Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>>,
    subject: Option<&ManifestDigest>,
    observed_at: UtcMicros,
    event: Plan26FeedbackSourceEventV1,
) {
    if let (Some(observations), Some(subject)) = (observations, subject) {
        observations.observe_source_event_for_subject(subject.clone(), observed_at, event);
    }
}

fn plan26_response_outcome(response: &DaemonInvocationResponse) -> Plan26FeedbackOutcomeV1 {
    match &response.outcome {
        DaemonInvocationOutcome::GitPreview { .. }
        | DaemonInvocationOutcome::GitApply { .. }
        | DaemonInvocationOutcome::ObservationAccepted
        | DaemonInvocationOutcome::LspOpened { .. }
        | DaemonInvocationOutcome::LspAcknowledged { .. }
        | DaemonInvocationOutcome::LspDetached => Plan26FeedbackOutcomeV1::Completed,
        DaemonInvocationOutcome::Feedback { result, .. }
        | DaemonInvocationOutcome::Primitive { result, .. } => match result.execution.termination {
            OperationTermination::Completed => Plan26FeedbackOutcomeV1::Completed,
            OperationTermination::Cancelled => Plan26FeedbackOutcomeV1::Cancelled,
            OperationTermination::TimedOut => Plan26FeedbackOutcomeV1::TimedOut,
            OperationTermination::Failed | OperationTermination::EffectUnknown => {
                Plan26FeedbackOutcomeV1::Failed
            }
            OperationTermination::Partial => Plan26FeedbackOutcomeV1::Partial,
        },
        DaemonInvocationOutcome::LspFrameAccepted { backpressured, .. } => {
            if *backpressured {
                Plan26FeedbackOutcomeV1::AtCapacity
            } else {
                Plan26FeedbackOutcomeV1::Accepted
            }
        }
        DaemonInvocationOutcome::LspFrame { closed, .. } => {
            if *closed {
                Plan26FeedbackOutcomeV1::Disconnected
            } else {
                Plan26FeedbackOutcomeV1::Completed
            }
        }
        DaemonInvocationOutcome::ApplicationProblem { problem } => match problem.kind() {
            ApplicationProblemKind::InvalidRequest => Plan26FeedbackOutcomeV1::Rejected,
            ApplicationProblemKind::NotFoundOrNotAuthorized => Plan26FeedbackOutcomeV1::Denied,
            ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => {
                Plan26FeedbackOutcomeV1::Stale
            }
            ApplicationProblemKind::Unsupported | ApplicationProblemKind::Unavailable => {
                Plan26FeedbackOutcomeV1::Unavailable
            }
            ApplicationProblemKind::Saturated => Plan26FeedbackOutcomeV1::AtCapacity,
            ApplicationProblemKind::Cancelled => Plan26FeedbackOutcomeV1::Cancelled,
            ApplicationProblemKind::TimedOut => Plan26FeedbackOutcomeV1::TimedOut,
        },
        DaemonInvocationOutcome::Problem { problem } => match problem {
            DaemonInvocationProblem::InvalidRequest
            | DaemonInvocationProblem::UnsupportedRevision => Plan26FeedbackOutcomeV1::Rejected,
            DaemonInvocationProblem::NotFoundOrNotAuthorized => Plan26FeedbackOutcomeV1::Denied,
            DaemonInvocationProblem::Unavailable => Plan26FeedbackOutcomeV1::Unavailable,
        },
    }
}

fn observe_plan26_invocation_response(
    observations: Option<&Arc<dyn Plan26FeedbackObservationEmitterV1 + Send + Sync>>,
    subject: Option<&ManifestDigest>,
    operation: DaemonInvocationOperation,
    route: Option<Plan26DeliveryRouteV1>,
    started_at: UtcMicros,
    response: &DaemonInvocationResponse,
) {
    let observed_at = current_micros();
    let outcome = plan26_response_outcome(response);
    let duration_micros = u64::try_from(observed_at.0.saturating_sub(started_at.0)).ok();
    if let Some(route) = route {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::Delivery {
                operation: plan26_feedback_operation(operation),
                route,
                outcome,
                item_count: match &response.outcome {
                    DaemonInvocationOutcome::Feedback { result, .. }
                    | DaemonInvocationOutcome::Primitive { result, .. } => {
                        result.page.returned.try_into().unwrap_or(u32::MAX)
                    }
                    _ => 0,
                },
                duration_micros,
            },
        );
    }
    if matches!(
        outcome,
        Plan26FeedbackOutcomeV1::Cancelled | Plan26FeedbackOutcomeV1::TimedOut
    ) {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::Cancellation {
                operation: plan26_feedback_operation(operation),
                outcome,
            },
        );
    }
    if matches!(outcome, Plan26FeedbackOutcomeV1::Rejected) {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::ArgumentRejected {
                operation: plan26_feedback_operation(operation),
                outcome,
            },
        );
    }
    if let DaemonInvocationOutcome::Feedback { result, .. }
    | DaemonInvocationOutcome::Primitive { result, .. } = &response.outcome
    {
        let omitted = result
            .page
            .total
            .map(|total| total.saturating_sub(result.page.returned))
            .unwrap_or_else(|| u64::from(result.page.cursor.is_some()));
        if omitted > 0 || result.page.cursor.is_some() {
            emit_plan26_invocation_event(
                observations,
                subject,
                observed_at,
                Plan26FeedbackSourceEventV1::Truncation {
                    operation: plan26_feedback_operation(operation),
                    returned_count: result.page.returned.try_into().unwrap_or(u32::MAX),
                    omitted_count: omitted.try_into().unwrap_or(u32::MAX),
                },
            );
        }
    }
    if operation == DaemonInvocationOperation::FeedbackExpand {
        emit_plan26_invocation_event(
            observations,
            subject,
            observed_at,
            Plan26FeedbackSourceEventV1::AnchorExpansion {
                operation: Plan26AnchorOperationV1::HandleExpansion,
                outcome,
                returned_count: match &response.outcome {
                    DaemonInvocationOutcome::Feedback { result, .. } => {
                        result.page.returned.try_into().unwrap_or(u32::MAX)
                    }
                    _ => 0,
                },
                duration_micros,
            },
        );
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn current_micros() -> UtcMicros {
    UtcMicros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
            .unwrap_or_default(),
    )
}

fn valid_token(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_printable(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_protocol_frames_select_the_invocation_route() {
        assert!(parse_daemon_invocation_request(r#"{"jsonrpc":"2.0","method":"ping"}"#).is_none());
        let request = DaemonInvocationRequest::lsp_open(
            "request.1",
            "client.1",
            Some("file:///untrusted".to_owned()),
            Vec::new(),
        );
        let encoded = serde_json::to_string(&request).expect("encode request");
        assert!(matches!(
            parse_daemon_invocation_request(&encoded),
            Some(Ok(_))
        ));
    }

    #[tokio::test]
    async fn semantic_scheduler_is_daemon_private_retained_state_not_a_wire_operation() {
        let service = DaemonInvocationService::default();
        let registrar = DaemonSemanticRuntimeRegistrar::new(&service);
        let project_root = PathBuf::from("/project/semantic-runtime");
        let handle = crate::semantic_code::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20)
            .expect("semantic scheduler");

        registrar
            .register(project_root.clone(), handle.clone())
            .await
            .expect("mount semantic scheduler");
        assert_eq!(
            service
                .semantic_runtime(Some(&project_root))
                .await
                .expect("retained semantic scheduler")
                .status(),
            crate::semantic_code::SemanticRuntimeScheduleStatusV1::Unavailable
        );
        assert!(matches!(
            registrar.register(project_root, handle).await,
            Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered)
        ));
        assert!(
            serde_json::to_string(&DaemonInvocationOperation::LspOpen)
                .expect("serialize existing operation")
                .find("semantic")
                .is_none(),
            "semantic scheduling must not add a public daemon operation"
        );
    }

    #[test]
    fn feedback_invocation_preserves_transport_deadline_and_cancellation() {
        let deadline = Deadline::new(UtcMicros(90)).expect("deadline");
        let cancellation =
            CancellationContext::cancelled("cancel.feedback.transport", UtcMicros(40))
                .expect("cancellation");
        let request = DaemonInvocationRequest::feedback(
            "request.feedback.transport",
            crate::application_surface::ApplicationSurfaceOperation::FeedbackList,
            "feedback-handle.transport".to_owned(),
            UtcMicros(30),
            deadline.clone(),
            cancellation.clone(),
        );

        assert!(matches!(
            request.payload,
            DaemonInvocationPayload::FeedbackList {
                observed_at: UtcMicros(30),
                deadline: carried_deadline,
                cancellation: carried_cancellation,
                ..
            } if carried_deadline == deadline && carried_cancellation == cancellation
        ));
    }

    #[tokio::test]
    async fn lsp_session_rejects_a_client_root_that_differs_from_the_admitted_root() {
        let service = DaemonInvocationService::default();
        let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
        let response = service
            .invoke(
                &registry,
                None,
                Some(AdmittedRoot::new("file:///authoritative")),
                None,
                DaemonInvocationRequest::lsp_open(
                    "request.1",
                    "client.1",
                    Some("file:///untrusted".to_owned()),
                    Vec::new(),
                ),
            )
            .await;
        let DaemonInvocationOutcome::LspOpened { session, .. } = response.outcome else {
            panic!("expected an admitted LSP session");
        };

        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///untrusted","capabilities":{}}}"#;
        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_frame("request.2", session.clone(), initialize),
            )
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspFrameAccepted { .. }
        ));

        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_poll("request.3", session.clone()),
            )
            .await;
        let DaemonInvocationOutcome::LspFrame {
            frame: Some(frame), ..
        } = response.outcome
        else {
            panic!("expected initialize response");
        };
        let response: serde_json::Value =
            serde_json::from_str(&frame).expect("initialize error must be JSON-RPC");
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(
            response["error"]["data"]["detail"],
            "root is not the daemon-admitted root"
        );

        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_acknowledge("request.4", session.clone()),
            )
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspAcknowledged { acknowledged: true }
        ));

        let initialize = r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"rootUri":"file:///authoritative","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#;
        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_frame("request.5", session.clone(), initialize),
            )
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspFrameAccepted {
                backpressured: false,
                closed: false
            }
        ));

        let response = service
            .invoke(
                &registry,
                None,
                None,
                None,
                DaemonInvocationRequest::lsp_poll("request.6", session),
            )
            .await;
        let DaemonInvocationOutcome::LspFrame {
            frame: Some(frame), ..
        } = response.outcome
        else {
            panic!("expected initialize success response");
        };
        let response: serde_json::Value =
            serde_json::from_str(&frame).expect("initialize success must be JSON-RPC");
        assert_eq!(response["id"], 2);
        assert!(response["result"]["capabilities"].is_object());
    }

    #[tokio::test]
    async fn feedback_handles_fail_closed_without_an_owner() {
        let service = DaemonInvocationService::default();
        let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
        let response = service
            .invoke(
                &registry,
                None,
                Some(AdmittedRoot::new("file:///authoritative")),
                None,
                DaemonInvocationRequest {
                    protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
                    revision: DAEMON_INVOCATION_REVISION,
                    request_id: "request.1".to_owned(),
                    delivery_route: None,
                    payload: DaemonInvocationPayload::FeedbackList {
                        request_handle: "handle.1".to_owned(),
                        observed_at: UtcMicros(1),
                        deadline: Deadline::new(UtcMicros(2)).expect("deadline"),
                        cancellation: CancellationContext::active("cancel.feedback-owner")
                            .expect("cancellation"),
                    },
                },
            )
            .await;
        assert_eq!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::Unavailable,
            }
        );
    }

    #[test]
    fn feedback_invocation_retains_trusted_delivery_route() {
        let request = DaemonInvocationRequest::feedback(
            "request.delivery-route",
            crate::application_surface::ApplicationSurfaceOperation::FeedbackList,
            "handle.delivery-route".to_owned(),
            UtcMicros(1),
            Deadline::new(UtcMicros(2)).expect("deadline"),
            CancellationContext::active("cancel.delivery-route").expect("cancellation"),
        )
        .with_delivery_route(Plan26DeliveryRouteV1::Mcp);
        assert_eq!(request.delivery_route, Some(Plan26DeliveryRouteV1::Mcp));
        let encoded = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(encoded["delivery_route"], "mcp");
        assert!(request.validate().is_ok());
    }

    #[test]
    fn feedback_observation_invocation_accepts_only_content_free_events() {
        let subject =
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("subject digest");
        let request = DaemonInvocationRequest::feedback_observation(
            "request.feedback-observe",
            subject,
            UtcMicros(1),
            Plan26FeedbackSourceEventV1::SseLifecycle {
                lifecycle: crate::application::feedback::observations::Plan26SseLifecycleV1::Gap,
                sequence: Some(1),
                item_count: 0,
                duration_micros: None,
            },
        );
        assert_eq!(
            request.operation(),
            DaemonInvocationOperation::FeedbackObserve
        );
        assert!(request.validate().is_ok());
        let encoded = serde_json::to_string(&request).expect("serialize request");
        assert!(!encoded.contains("source"));
        assert!(!encoded.contains("comment"));
        assert!(!encoded.contains("log"));
    }
}

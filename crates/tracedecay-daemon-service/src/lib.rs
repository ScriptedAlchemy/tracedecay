//! Closed, authenticated daemon invocation service.
//!
//! This crate owns request dispatch, project-runtime publication, and the
//! payload handlers that execute after daemon handshake. It sits above
//! usecases, application, agent-hosts, and code-index-runtime, and below the
//! composition root. OS lifecycle management (install/start/stop/probe) lives
//! in `tracedecay-daemon-control`.
//!
//! [`application_surface`] is the transport adaptation shared by HTTP, MCP,
//! and the CLI: it resolves catalog bindings, normalizes request controls, and
//! projects canonical application envelopes. Route descriptors and encoding
//! stay in `tracedecay-api`; the composition root only injects the
//! authenticated executor into the routers assembled here.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::unused_self)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::option_option)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::ref_option)]
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::manual_async_fn)]
#![allow(clippy::unused_async)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::if_not_else)]
#![allow(clippy::fn_params_excessive_bools)]
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::missing_fields_in_debug)]
#![allow(clippy::single_match_else)]
#![allow(clippy::large_futures)]

/// Abort bound for in-flight invocation tasks during shutdown.
///
/// Re-exported here as the daemon-service authority while the lower runtime
/// crate remains the cycle-free owner shared with code-index runtime.
pub use tracedecay_runtime_core::DAEMON_TASK_ABORT_DEADLINE as TASK_ABORT_DEADLINE;

pub mod application_surface;
pub mod callable_code_authorization;
pub mod invocation;
mod mcp_project_registry;
mod mcp_workflow_index;
pub mod profile_host_admission_replay;
pub mod project_owner_registration;
pub mod project_runtime;
pub mod query_authority_provider;
pub mod query_mcp_admission;
pub mod remote_http_transport;
mod remote_protocol;
pub mod request_cancellation;
mod shutdown_coordination;

pub use callable_code_authorization::{
    DaemonCallableCodeAuthorizationSource, DaemonCodeGraphReadAdmission, GRANT_HORIZON,
    daemon_owned_project_source_access_at, project_open_source_access_authority,
};
pub use invocation::semantic_evaluation::SemanticInvocationControlV1;
#[cfg(any(test, feature = "test-helpers"))]
pub use invocation::{
    AuthorizedDaemonLspWorkspace, DaemonConfigurationRuntimeRegistrationPauseV1,
    DaemonFeedbackPublicationTestGate, InvocationProjectRuntimeIdentityV1, LspLeaseTaskRegistry,
    RuntimeLspSession, WorkAttemptProcessRegistryV1, canonicalize_lsp_roots, current_micros,
    execute_work_application, lsp_delivery_attempt, mounted_configuration_layers, now_millis,
    retain_lsp_delivery_attempt,
};
pub use invocation::{
    BoundedHookOrchestratorV1, ConfigurationRuntimeRefreshFuture, ConfigurationRuntimeRefreshPort,
    DaemonAdvisoryCycleInvocationFuture, DaemonAdvisoryCycleInvocationOwner,
    DaemonAdvisoryCycleInvocationPort, DaemonAdvisoryCycleInvocationRequest,
    DaemonAdvisoryRuntimeRegistrar, DaemonAdvisoryRuntimeRegistrationError,
    DaemonConfigurationGrantAuthority, DaemonConfigurationRuntimeRegistrar,
    DaemonContextScoutRuntimeRegistrar, DaemonContextScoutRuntimeRegistrationError,
    DaemonFeedbackInvocationOwner, DaemonFeedbackRuntimeRegistrar,
    DaemonFeedbackRuntimeRegistrationError, DaemonInvocationService, DaemonLspInvocationOwner,
    DaemonLspOwnerRegistrar, DaemonNativeIntegrationRuntimeRegistrar,
    DaemonPrimitiveRuntimeRegistrar, DaemonPrimitiveRuntimeRegistrationError,
    DaemonRetainedRuntimeRegistrar, DaemonSemanticOwnerRuntimeRegistrar,
    DaemonSemanticRuntimeRegistrar, DaemonSemanticRuntimeRegistrationError,
    DaemonSourceEditOwnerRegistrationError, DaemonWorkProposalRoutingAuthorityV1,
    DaemonWorkRuntimeRegistrar, FeedbackCycleRuntimeBuilderV1, HookOrchestrationAdmissionV1,
    HookOrchestrationRequestV1, HookOrchestrationTriggerV1, HookOrchestrationWorkOutcomeV1,
    LSP_WORKSPACE_CAPABILITY_ID_V1, LSP_WORKSPACE_USE_CASE_ID_V1, LspDeliverySettlementAdmissionV1,
    MAX_COALESCED_HOOK_COMPLETIONS, RegisteredCallableCodeRuntime, RegisteredConfigurationRuntime,
    RegisteredFeedbackRuntime, RegisteredRetainedRequestContextError, RegisteredRetainedRuntime,
    RegisteredWorkRuntime, SwitchableFeedbackCycleRuntimeV1, UnavailableFeedbackCycleRuntimeV1,
    admit_registered_hook_orchestration, advisory_cycle_invocation_result,
    callable_code_request_context, daemon_operation_event_authority,
    register_hook_orchestration_runtime, unregister_hook_orchestration_runtime,
};
pub use mcp_project_registry::DaemonProjectRegistryReadService;
pub use mcp_workflow_index::DaemonWorkflowIndexReadService;
#[cfg(any(test, feature = "test-helpers"))]
pub use profile_host_admission_replay::BootstrapCompletion;
pub use profile_host_admission_replay::{
    ProfileHostAdmissionBootstrapOperation, ProfileHostAdmissionBootstrapStatus,
    ProfileHostAdmissionReplayPass, ProfileHostAdmissionReplayRegistry,
};
pub use project_runtime::{
    FeedbackCyclePublicationError, ProjectRuntimeAlreadyRegistered,
    ProjectRuntimePublicationAttemptV1, ProjectRuntimePublicationStateV1,
    ProjectRuntimeRegistryError, ProjectRuntimeRegistryV1, ProjectRuntimeRequestLeaseV1,
    ProjectRuntimeRootQuiescenceV1, RegisteredDeliveryReadAuthorityV1,
    RegisteredObservabilityProducerV1, RegisteredSemanticOwnerTaskV1,
    SemanticOwnerRegistrationSignalsV1, StoreObservabilityMountErrorV1, StoreObservabilityMountV1,
    StoreObservabilityRegistryV1,
};
pub use query_authority_provider::{
    DaemonQueryActivationRegistrarV1, DaemonQueryAuthorityProviderV1,
    QueryAuthorityProviderStatusV1, QueryAuthorityUpdateErrorV1,
};
pub use query_mcp_admission::{
    QUERY_MCP_READ_CAPABILITY_V1, QueryMcpAdmissionUnavailableV1, QueryMcpReadAdmissionProviderV1,
    QueryMcpReadAdmissionV1, admit_query_mcp_read,
};
pub use remote_protocol::build_daemon_remote_protocol_router;
pub use request_cancellation::{Lease, RequestCancellationRegistryV1};
pub use shutdown_coordination::ShutdownCoordinatorV1;
pub use tracedecay_daemon_protocol::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonFeedbackResult,
    DaemonGitEffectResult, DaemonGitPreviewResult, DaemonInvocationOperation,
    DaemonInvocationOutcome, DaemonInvocationPayload, DaemonInvocationProblem,
    DaemonInvocationRequest, DaemonInvocationResponse, DaemonLspSessionAccess,
    HandoffApplicationInvocationV1, HandoffApplicationOutcomeV1, LspSessionAccess,
    LspSessionCredential, LspSessionId, WorkApplicationInvocationV1, WorkApplicationOutcomeV1,
    parse_daemon_invocation_request,
};

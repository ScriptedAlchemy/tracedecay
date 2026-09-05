//! Closed, authenticated daemon invocation service.
//!
//! This crate owns request dispatch, project-runtime publication, and the
//! payload handlers that execute after daemon handshake. It sits above
//! usecases, application, agent-hosts, and code-index-runtime, and below the
//! composition root. OS lifecycle management (install/start/stop/probe) lives
//! in `tracedecay-daemon-control`.

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

pub mod invocation;
pub mod project_runtime;
pub mod request_cancellation;

mod multi_root;

pub use invocation::semantic_evaluation::SemanticInvocationControlV1;
#[cfg(any(test, feature = "test-helpers"))]
pub use invocation::{
    AuthorizedDaemonLspWorkspace, DaemonFeedbackPublicationTestGate,
    InvocationProjectRuntimeIdentityV1, LspLeaseTaskRegistry, RuntimeLspSession,
    WorkAttemptProcessRegistryV1, canonicalize_lsp_roots, current_micros, execute_work_application,
    lsp_delivery_attempt, mounted_configuration_layers, now_millis, retain_lsp_delivery_attempt,
};
pub use invocation::{
    BoundedHookOrchestratorV1, DaemonAdvisoryCycleInvocationFuture,
    DaemonAdvisoryCycleInvocationOwner, DaemonAdvisoryCycleInvocationPort,
    DaemonAdvisoryCycleInvocationRequest, DaemonAdvisoryRuntimeRegistrar,
    DaemonAdvisoryRuntimeRegistrationError, DaemonConfigurationGrantAuthority,
    DaemonConfigurationRuntimeRegistrar, DaemonContextScoutRuntimeRegistrar,
    DaemonContextScoutRuntimeRegistrationError, DaemonFeedbackInvocationOwner,
    DaemonFeedbackRuntimeRegistrar, DaemonFeedbackRuntimeRegistrationError,
    DaemonInvocationService, DaemonLspInvocationOwner, DaemonLspOwnerRegistrar,
    DaemonNativeIntegrationRuntimeRegistrar, DaemonPrimitiveRuntimeRegistrar,
    DaemonPrimitiveRuntimeRegistrationError, DaemonRetainedRuntimeRegistrar,
    DaemonSemanticRuntimeRegistrar, DaemonSemanticRuntimeRegistrationError,
    DaemonWorkProposalRoutingAuthorityV1, DaemonWorkRuntimeRegistrar, HookOrchestrationAdmissionV1,
    HookOrchestrationRequestV1, HookOrchestrationTriggerV1, HookOrchestrationWorkOutcomeV1,
    LSP_WORKSPACE_CAPABILITY_ID_V1, LSP_WORKSPACE_USE_CASE_ID_V1, LspDeliverySettlementAdmissionV1,
    MAX_COALESCED_HOOK_COMPLETIONS, RegisteredCallableCodeRuntime, RegisteredConfigurationRuntime,
    RegisteredFeedbackRuntime, RegisteredRetainedRequestContextError, RegisteredRetainedRuntime,
    RegisteredWorkRuntime, SwitchableFeedbackCycleRuntimeV1, UnavailableFeedbackCycleRuntimeV1,
    admit_registered_hook_orchestration, advisory_cycle_invocation_result,
    callable_code_request_context, daemon_operation_event_authority,
    register_hook_orchestration_runtime, unregister_hook_orchestration_runtime,
};
pub use project_runtime::{
    FeedbackCyclePublicationError, ProjectRuntimeAlreadyRegistered, ProjectRuntimeRegistryError,
    ProjectRuntimeRegistryV1, ProjectRuntimeRequestLeaseV1, ProjectRuntimeRootQuiescenceV1,
    RegisteredDeliveryReadAuthorityV1, RegisteredObservabilityProducerV1,
    StoreObservabilityMountErrorV1, StoreObservabilityMountV1, StoreObservabilityRegistryV1,
};
pub use request_cancellation::{Lease, cancel, register};
pub use tracedecay_daemon_protocol::{
    DAEMON_INVOCATION_PROTOCOL, DAEMON_INVOCATION_REVISION, DaemonFeedbackResult,
    DaemonGitEffectResult, DaemonGitPreviewResult, DaemonInvocationOperation,
    DaemonInvocationOutcome, DaemonInvocationPayload, DaemonInvocationProblem,
    DaemonInvocationRequest, DaemonInvocationResponse, DaemonLspSessionAccess,
    HandoffApplicationInvocationV1, HandoffApplicationOutcomeV1, LspSessionAccess,
    LspSessionCredential, LspSessionId, WorkApplicationInvocationV1, WorkApplicationOutcomeV1,
    parse_daemon_invocation_request,
};

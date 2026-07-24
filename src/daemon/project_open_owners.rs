//! Project-open registration for PR11–PR13 production owners.
//!
//! After Scout bootstrap and successful cache publication, the daemon mounts
//! concrete feedback, cycle, primitive, LSP, advisory, and Hook/Scout host-
//! delivery owners from the admitted project identity. Cycle/LSP/advisory mount
//! only when their real upstream authorities resolve; missing identity fails
//! closed and placeholder owners are never installed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
    FEEDBACK_EXPAND_CAPABILITY_ID_V1, FEEDBACK_GET_CAPABILITY_ID_V1,
    FEEDBACK_LIST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    GitHubReviewReadRequestV1, PROXIMITY_CAPABILITY_ID_V1, ProximityEvaluationRequestV1,
};
use tracedecay_application::{ApplicationContractError, ResolvedScope};
use tracedecay_domain::configuration::{
    AuthorityRef, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::feedback::{
    CiFailureParserIdentityV1, FeedbackScopeV1, FeedbackTriggerV1, GitHubPullRequestIdV1,
    GitHubReviewReadOperationV1,
};
use tracedecay_domain::{
    ActorId, CommitId, LocatorDigest, ProjectId, ProviderId, RefId, RepositoryId, UtcMicros,
    canonical_sha256,
};
use tracedecay_hooks::{HookFeedbackDeliveryRouteV1, HookFeedbackRollbackSwitchV1, HookHostV1};
use tracedecay_tool_catalog::CapabilityId;

use super::{
    BoundedPr13HookOrchestratorV1, DaemonAdvisoryRuntimeRegistrationError,
    DaemonContextScoutRuntimeRegistrationError, DaemonFeedbackRuntimeRegistrationError,
    DaemonInvocationState, DaemonPrimitiveRuntimeRegistrationError, Pr13HookOrchestrationRequestV1,
    Pr13HookOrchestrationTriggerV1,
};
use crate::agents::context_scout_ports::{
    ContextScoutAuthorityPinV1, ContextScoutCanonicalInputAssemblerV1,
    ContextScoutConfigurationPinV1, ProjectContextScoutAddressRegistryV1,
};
use crate::agents::context_scout_v2::ContextScoutDeliveryWindowV1;
use crate::agents::host_bundle_v2::HostKindV1;
use crate::application::advisory::github_runtime::{
    GitHubExactCommitDiscoveryOutcomeV1, discover_exact_commit_pull_request_v1,
};
use crate::application::advisory::{
    GitHubHttpReadConfigV1, GitHubReadOnlyCredentialV1, GitHubRepositoryTargetV1,
    GitHubReviewProviderIdentityV1, GitHubReviewRuntimeOwnerConfigV1, Pr13AdvisoryCycleControlV1,
    Pr13AdvisoryCycleRequestV1, Pr13AdvisoryHookLookupNoticeV1, Pr13AdvisoryHookNoticeQueueV1,
    Pr13AdvisoryHookNoticeSinkV1, Pr13AdvisoryProductionOpenV1,
    Pr13AdvisoryProductionStartupRegistrationV1, Pr13AdvisoryRuntimeOpenV1,
    ProductionCiProviderConfigV1, ProjectCiCodeAnchorStoreV1, ProjectCiRetainedObservationStoreV1,
    register_pr13_advisory_hook_notice_queue,
};
use crate::application::context::{CancellationToken, MonotonicDeadline};
use crate::application::feedback::{
    Pr12FeedbackCycleInvocation, Pr12FeedbackCycleLspInput, ProductionFeedbackCycleOpenV1,
    ProductionFeedbackRuntimeStateV1, resolve_production_feedback_cycle_parts,
};
use crate::application::operation_stream::OperationKind;
use crate::application::primitives::{
    admitted_root_uri_for_project, locator_digest_for_project,
    open_pr12_production_primitive_runtime, worktree_id_for_project,
};
use crate::application::source_authorization::ProjectSourceAccessSnapshot;
use crate::daemon::git_transactions::DaemonGitIndexTransactionServiceRegistry;
use crate::daemon::lsp_gateway::{
    ContextProjectionKind, DiagnosticTrigger, FeedbackCycleRequest, GatewayCapabilities,
};
use crate::daemon::service::invocation::daemon_operation_event_authority;
use crate::diagnostics::lsp::broker::{AdmittedLspProvider, MountedLspProvider};
use crate::diagnostics::lsp::client::LspRefreshTimeouts;
use crate::diagnostics::lsp::semantic::graph_semantic_capabilities;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::McpServer;

const DAEMON_REQUESTER: &str = "actor.tracedecay-daemon.project-open";
const DAEMON_BINDING: &str = "binding.tracedecay-daemon.project-open";
const GRANT_HORIZON: Duration = Duration::from_hours(24);
const POLICY_REVISION_V1: u64 = 1;
const LSP_DIAGNOSTICS_QUIET: Duration = Duration::from_secs(2);

fn unavailable_advisory_hook_notice(
    _notice: &Pr13AdvisoryHookLookupNoticeV1,
) -> tracedecay_hooks::HookFeedbackDeliveryOutcomeV1 {
    tracedecay_hooks::HookFeedbackDeliveryOutcomeV1::Unavailable
}

fn unavailable_advisory_hook_sink() -> Arc<Pr13AdvisoryHookNoticeSinkV1> {
    Arc::new(unavailable_advisory_hook_notice)
}

/// Registers concrete production owners for one newly inserted project server.
pub(crate) async fn register_project_open_production_owners(
    invocation: &DaemonInvocationState,
    git_transactions: &DaemonGitIndexTransactionServiceRegistry,
    project_root: &Path,
    project_id: &str,
    server: &McpServer,
) -> Result<()> {
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|_| TraceDecayError::Config {
            message: "project-open owners require an authoritative project identity".to_owned(),
        })?;
    let graph = server.cg().await;
    let database = graph.db().clone();
    let session_db = server
        .project_session_db()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open owners require the daemon-owned project session database"
                .to_owned(),
        })?;
    let scope = resolved_scope_for_project(project_root, &project_id).map_err(|error| {
        TraceDecayError::Config {
            message: format!("project-open resolved scope denied: {error}"),
        }
    })?;
    if let Err(error) = invocation
        .mount_pr9_authority_for_project(project_root, &scope)
        .await
    {
        tracing::debug!(
            event = "pr9_authority_mount",
            outcome = "unavailable",
            project_id = %project_id,
            reason = %error,
            "PR9 search authority unavailable; non-search project surfaces remain mounted"
        );
    }
    let configuration = graph.configuration_runtime().configuration().clone();
    let scout_configuration = crate::application::configuration::ConfigurationCurrentStateV1 {
        revision_id: configuration.revision_id.clone(),
        snapshot: configuration.snapshot.clone(),
    };
    if let Ok(configuration_pin) =
        crate::application::semantic_runtime::SemanticConfigurationPinV1::from_current(
            &scout_configuration,
        )
    {
        match crate::application::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1::open(
            Arc::clone(&session_db),
            scope.clone(),
        )
        .await
        {
            Ok(store) => {
                if let Err(error) = crate::daemon::code_index_scheduler::semantic_query_runtime::
                    mount_current_semantic_query_authority_on_project_open(
                        &invocation.code_index_schedulers,
                        project_root,
                        &scope,
                        &store,
                        &configuration_pin,
                    )
                    .await
                {
                    tracing::debug!(
                        event = "semantic_query_authority_mount",
                        outcome = "unavailable",
                        project_id = %project_id,
                        reason = %error,
                        "semantic query authority unavailable; canonical PR9 remains mounted"
                    );
                }
            }
            Err(error) => {
                tracing::debug!(
                    event = "semantic_configuration_store_open",
                    outcome = "unavailable",
                    project_id = %project_id,
                    reason = ?error,
                    "semantic configuration unavailable; canonical PR9 remains mounted"
                );
            }
        }
    }
    let scout_registry = match invocation
        .context_scout_runtime_registrar()
        .open_and_register(database.clone(), project_id.clone())
        .await
    {
        Ok(registry) => registry,
        Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered) => invocation
            .context_scout_runtime_registrar()
            .get(&project_id)
            .await
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open Context Scout registry disappeared".to_owned(),
            })?,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("project-open Context Scout registry failed: {error}"),
            });
        }
    };
    let access = daemon_owned_project_source_access(&scope, project_root, &configuration).map_err(
        |error| TraceDecayError::Config {
            message: format!("project-open source access denied: {error}"),
        },
    )?;
    let configuration_digest = access.configuration_digest.clone();
    let grant_expires_at = access.grant_expires_at;
    let requester = access.requester.clone();
    let repository_root = crate::worktree::git_worktree_root(project_root)
        .unwrap_or_else(|| project_root.to_path_buf());
    git_transactions
        .install_authority(
            &repository_root,
            access.clone(),
            Arc::clone(&session_db),
            tokio::runtime::Handle::current(),
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Git authority registration failed: {error}"),
        })?;
    let configuration_policy_digest = canonical_sha256(&(
        "tracedecay.daemon.configuration-policy.v1",
        &scope.scope_digest,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open configuration policy digest failed: {error}"),
    })?;
    invocation
        .configuration_runtime_registrar()
        .register(
            project_root.to_path_buf(),
            Arc::clone(graph.configuration_runtime()),
            scope.clone(),
            requester.clone(),
            grant_expires_at,
            None,
            configuration_policy_digest,
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open configuration runtime registration failed: {error}"),
        })?;
    register_semantic_activation_owner(invocation, project_root, &graph, scope.clone()).await?;

    match invocation
        .feedback_runtime_registrar()
        .open_and_register(
            database.clone(),
            project_root.to_path_buf(),
            scope.clone(),
            access.clone(),
        )
        .await
    {
        Ok(_) | Err(DaemonFeedbackRuntimeRegistrationError::AlreadyRegistered) => {}
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("project-open feedback runtime registration failed: {error:?}"),
            });
        }
    }

    let admitted_root_uri =
        admitted_root_uri_for_project(project_root).map_err(|error| TraceDecayError::Config {
            message: format!("project-open admitted root URI denied: {error}"),
        })?;
    let primitive_runtime = open_pr12_production_primitive_runtime(
        database.clone(),
        Arc::clone(&graph),
        Arc::clone(&session_db),
        scope.clone(),
        access.clone(),
        admitted_root_uri.clone(),
        daemon_operation_event_authority(),
        configuration_digest.clone(),
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open primitive runtime open failed: {error}"),
    })?;
    match invocation
        .primitive_runtime_registrar()
        .register(project_root.to_path_buf(), primitive_runtime)
        .await
    {
        Ok(_) | Err(DaemonPrimitiveRuntimeRegistrationError::AlreadyRegistered) => {}
    }

    let indexed_files = graph
        .get_file_token_map()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open LSP language discovery failed: {error}"),
        })?
        .into_keys()
        .collect::<Vec<_>>();
    let diagnostic_broker = server.diagnostics_lsp();
    let admitted_providers = diagnostic_broker
        .lock()
        .await
        .admitted_providers_for_files(&indexed_files);
    let mounted_providers = admitted_providers
        .iter()
        .filter_map(AdmittedLspProvider::mounted)
        .collect::<Vec<_>>();

    let (feedback_cycle, feedback_scope, feedback_lsp_input) = register_production_feedback_cycle(
        invocation,
        project_root,
        database.clone(),
        Arc::clone(&session_db),
        Arc::clone(&graph),
        scope.clone(),
        configuration,
        requester,
        grant_expires_at,
        mounted_providers.clone(),
    )
    .await?;

    let lsp_session_factory = register_production_lsp_owner(
        invocation,
        project_root,
        database.clone(),
        diagnostic_broker,
        &admitted_providers,
        admitted_root_uri.clone(),
    )
    .await?;

    register_production_advisory_owner(
        invocation,
        project_root,
        database,
        session_db,
        Arc::clone(&graph),
        scope,
        feedback_scope,
        feedback_cycle,
        feedback_lsp_input,
        lsp_session_factory,
        scout_registry,
        scout_configuration,
        admitted_root_uri,
        indexed_files,
    )
    .await?;

    Ok(())
}

async fn register_semantic_activation_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    graph: &Arc<crate::tracedecay::TraceDecay>,
    scope: ResolvedScope,
) -> Result<()> {
    let Some(inspector) =
        crate::application::semantic_runtime::project_semantic_production_runtime(project_root)
    else {
        return Ok(());
    };
    let configuration_store =
        crate::application::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1::open(
            graph.configuration_runtime().registered_database(),
            scope,
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("semantic retrieval configuration store unavailable: {error}"),
        })?;
    let observer = invocation.pr9_activation_registrar(project_root);
    if let Some(committed) = configuration_store
        .current_committed_state()
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("semantic retrieval committed state unavailable: {error}"),
        })?
    {
        observer
            .activation_committed(committed)
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("semantic retrieval activation restore failed: {error}"),
            })?;
    }
    let owner = Arc::new(
        crate::application::semantic_runtime::ProductionSemanticActivationCoordinatorV1::new(
            configuration_store,
            graph.configuration_runtime().configuration_store(),
            inspector,
            observer,
        ),
    );
    graph
        .configuration_runtime()
        .install_semantic_runtime(owner)?;
    let accepted_profiles = Arc::new(
        crate::application::semantic_runtime::RegisteredSemanticAcceptedProfileAuthorityV1::open(
            graph.configuration_runtime().registered_database(),
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("semantic accepted-profile authority unavailable: {error}"),
        })?,
    );
    let operation = Arc::new(
        crate::application::semantic_runtime::ProductionSemanticConfigurationOperationV1::new(
            Arc::clone(graph.configuration_runtime()),
            accepted_profiles,
        ),
    );
    invocation
        .configuration_runtime_registrar()
        .install_semantic_operation(project_root, operation)
        .await
}

async fn register_production_feedback_cycle(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    database: crate::db::Database,
    project_runtime_db: Arc<crate::global_db::RegisteredGlobalDb>,
    graph: Arc<crate::tracedecay::TraceDecay>,
    scope: ResolvedScope,
    configuration: crate::config::PinnedRuntimeConfiguration,
    requester: ActorId,
    grant_expires_at: UtcMicros,
    mounted_providers: Vec<MountedLspProvider>,
) -> Result<(
    Arc<crate::application::feedback::Pr12FeedbackCycleRuntime>,
    FeedbackScopeV1,
    crate::application::feedback::Pr12FeedbackCycleLspInput,
)> {
    let configuration_digest = &configuration.snapshot.effective_behavior_digest;
    let policy_digest = canonical_sha256(&(
        "tracedecay.project-open.policy.v1",
        configuration_digest,
        POLICY_REVISION_V1,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open feedback policy digest failed: {error}"),
    })?;
    let runtime_state = Arc::new(ProductionFeedbackRuntimeStateV1::new(
        Arc::clone(&graph),
        configuration_digest.clone(),
        policy_digest,
    ));
    let parts = resolve_production_feedback_cycle_parts(ProductionFeedbackCycleOpenV1 {
        project_root: project_root.to_path_buf(),
        project_runtime_db,
        scope,
        access_configuration: crate::application::configuration::ConfigurationCurrentStateV1 {
            revision_id: configuration.revision_id,
            snapshot: configuration.snapshot,
        },
        requester,
        grant_expires_at,
        graph: Arc::clone(&graph),
        runtime_state: Arc::clone(&runtime_state) as _,
        document_identity: Arc::new(invocation.code_index_schedulers.clone()),
        mounted_providers,
    })
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open feedback cycle parts failed: {error}"),
    })?;
    let feedback_scope = parts.feedback_scope.clone();
    let feedback_lsp_input = Arc::clone(&parts.lsp_input);
    invocation
        .feedback_runtime_registrar()
        .open_cycle_and_register(
            project_root.to_path_buf(),
            database,
            parts.runtime_state,
            parts.policy_context,
            parts.evidence_horizon,
            parts.evaluated_at,
            parts.provider_candidates,
            graph,
            parts.affected_tests,
            parts.operation,
            parts.graph_operation,
            parts.tests_operation,
            parts.lsp_input,
            parts.proximity,
        )
        .await
        .map(|runtime| (runtime, feedback_scope, feedback_lsp_input))
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open feedback cycle registration failed: {error}"),
        })
}

async fn register_production_lsp_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    database: crate::db::Database,
    diagnostic_broker: Arc<tokio::sync::Mutex<crate::diagnostics::lsp::broker::DiagnosticBroker>>,
    admitted_providers: &[AdmittedLspProvider],
    root_uri: String,
) -> Result<Arc<crate::daemon::lsp_gateway::Pr12LspSessionFactory>> {
    let (language, gateway_capabilities) = production_lsp_registration(admitted_providers);
    invocation
        .lsp_owner_registrar()
        .build_and_register_pr12(
            project_root.to_path_buf(),
            database,
            Arc::new(invocation.code_index_schedulers.clone()),
            tokio::runtime::Handle::current(),
            diagnostic_broker,
            language,
            root_uri,
            LspRefreshTimeouts::from_diagnostics_quiet_window(LSP_DIAGNOSTICS_QUIET),
            LSP_DIAGNOSTICS_QUIET,
            gateway_capabilities,
        )
        .await
}

fn production_lsp_registration(
    admitted_providers: &[AdmittedLspProvider],
) -> (Option<&str>, GatewayCapabilities) {
    let revision = crate::daemon::lsp_gateway::TRACEDECAY_CONTEXT_REVISION;
    let gateway_capabilities = GatewayCapabilities {
        semantic: graph_semantic_capabilities(),
        context_projections: BTreeMap::from([
            (ContextProjectionKind::diagnostics(), revision),
            (ContextProjectionKind::post_edit_impact(), revision),
            (ContextProjectionKind::affected_tests(), revision),
            (ContextProjectionKind::test_run_results(), revision),
        ]),
        ..Default::default()
    };
    let provider = admitted_providers
        .iter()
        .find(|provider| provider.analyzer_available)
        .or_else(|| admitted_providers.first());
    (
        provider.map(|provider| provider.language.as_str()),
        gateway_capabilities,
    )
}

async fn register_production_advisory_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    database: crate::db::Database,
    project_runtime_db: Arc<crate::global_db::RegisteredGlobalDb>,
    graph: Arc<crate::tracedecay::TraceDecay>,
    resolved_scope: ResolvedScope,
    feedback_scope: FeedbackScopeV1,
    feedback_cycle: Arc<crate::application::feedback::Pr12FeedbackCycleRuntime>,
    feedback_lsp_input: Pr12FeedbackCycleLspInput,
    lsp_session_factory: Arc<crate::daemon::lsp_gateway::Pr12LspSessionFactory>,
    scout_registry: Arc<ProjectContextScoutAddressRegistryV1>,
    scout_configuration: crate::application::configuration::ConfigurationCurrentStateV1,
    root_uri: String,
    indexed_files: Vec<String>,
) -> Result<Option<()>> {
    let scout_configuration = ContextScoutConfigurationPinV1::from_current(&scout_configuration)
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open Context Scout configuration is unavailable".to_owned(),
        })?;
    let scout_owner =
        graph
            .context_scout_owner()
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open Context Scout owner is unavailable".to_owned(),
            })?;
    scout_owner
        .install_configuration(scout_configuration.clone(), None)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Context Scout configuration failed: {error}"),
        })?;
    let github = resolve_production_github_review_config(
        project_root,
        database.clone(),
        resolved_scope.clone(),
        feedback_scope.clone(),
    )
    .await;
    let (github, ci_config, github_pull_request_id) =
        optional_remote_provider_configuration(github)?;
    let ci_retained = Arc::new(
        ProjectCiRetainedObservationStoreV1::new(database.clone(), feedback_scope.clone())
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open CI retained store failed: invalid feedback scope"
                    .to_string(),
            })?,
    ) as _;
    let ci_code_anchors = Arc::new(
        ProjectCiCodeAnchorStoreV1::new(Arc::clone(&graph), feedback_scope.clone()).ok_or_else(
            || TraceDecayError::Config {
                message: "project-open CI anchor store failed: invalid feedback scope".to_string(),
            },
        )?,
    ) as _;
    let hook_notices = Pr13AdvisoryHookNoticeQueueV1::new(feedback_scope.clone());
    let hook_v2 = hook_notices.sink();
    let legacy_hook = unavailable_advisory_hook_sink();
    let (hook_project_id, hook_worktree_id) = crate::hooks::hook_v2_scope_locators(&resolved_scope);
    if !crate::daemon::context_scout_lifecycle::register_context_scout_lifecycle_authority(
        hook_project_id,
        hook_worktree_id,
        feedback_scope.project_id.clone(),
        feedback_scope.worktree_id.clone(),
        &project_runtime_db,
    ) {
        return Err(TraceDecayError::Config {
            message: "project-open Context Scout lifecycle authority registration failed"
                .to_owned(),
        });
    }
    if !register_pr13_advisory_hook_notice_queue(hook_project_id, hook_worktree_id, &hook_notices) {
        return Err(TraceDecayError::Config {
            message: "project-open advisory Hook notice queue registration failed".to_owned(),
        });
    }
    let feedback_runtime = feedback_cycle.feedback_runtime();
    let feedback_scope_for_work = feedback_scope.clone();
    let input = Pr13AdvisoryRuntimeOpenV1 {
        database: database.clone(),
        project_root: project_root.to_path_buf(),
        resolved_scope,
        feedback_scope: feedback_scope.clone(),
        github,
        feedback_cycle,
    };
    let production = Pr13AdvisoryProductionOpenV1 {
        database,
        project_runtime_db,
        graph,
        project_root: project_root.to_path_buf(),
        feedback_scope,
        ci_config,
        ci_retained,
        ci_code_anchors,
        hook_v2,
        legacy_hook,
    };
    let registration = match invocation
        .advisory_runtime_registrar()
        .register_production(
            project_root.to_path_buf(),
            input,
            production,
            lsp_session_factory,
        )
        .await
    {
        Ok(registration) => registration,
        Err(DaemonAdvisoryRuntimeRegistrationError::AlreadyRegistered) => return Ok(Some(())),
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("project-open advisory runtime registration failed: {error}"),
            });
        }
    };
    let registered_root = project_root.to_path_buf();
    let work_root = registered_root.clone();
    let work = move |request: Pr13HookOrchestrationRequestV1| {
        let registration = Arc::clone(&registration);
        let feedback_lsp_input = Arc::clone(&feedback_lsp_input);
        let scout_owner = Arc::clone(&scout_owner);
        let scout_registry = Arc::clone(&scout_registry);
        let scout_configuration = scout_configuration.clone();
        let feedback_runtime = Arc::clone(&feedback_runtime);
        let github_pull_request_id = github_pull_request_id.clone();
        let feedback_scope = feedback_scope_for_work.clone();
        let project_root = work_root.clone();
        let root_uri = root_uri.clone();
        let indexed_files = indexed_files.clone();
        async move {
            run_production_pr13_hook_cycle(
                request,
                registration,
                feedback_lsp_input,
                scout_owner,
                scout_registry,
                scout_configuration,
                feedback_runtime,
                github_pull_request_id,
                feedback_scope,
                project_root,
                root_uri,
                indexed_files,
            )
            .await;
        }
    };
    let orchestrator =
        BoundedPr13HookOrchestratorV1::new(1, work).ok_or_else(|| TraceDecayError::Config {
            message: "project-open PR13 Hook orchestration capacity is invalid".to_owned(),
        })?;
    invocation
        .advisory_runtime_registrar()
        .register_hook_orchestrator(
            registered_root,
            hook_project_id,
            hook_worktree_id,
            orchestrator,
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open PR13 Hook orchestration failed: {error}"),
        })?;
    Ok(Some(()))
}

#[allow(clippy::too_many_arguments)]
async fn run_production_pr13_hook_cycle(
    request: Pr13HookOrchestrationRequestV1,
    registration: Arc<Pr13AdvisoryProductionStartupRegistrationV1>,
    feedback_lsp_input: Pr12FeedbackCycleLspInput,
    scout_owner: Arc<crate::agents::context_scout_owner::ProjectContextScoutOwnerV1>,
    scout_registry: Arc<ProjectContextScoutAddressRegistryV1>,
    scout_configuration: ContextScoutConfigurationPinV1,
    feedback_runtime: Arc<crate::application::feedback::concrete::Pr12FeedbackRuntime>,
    github_pull_request_id: Option<GitHubPullRequestIdV1>,
    feedback_scope: FeedbackScopeV1,
    project_root: std::path::PathBuf,
    root_uri: String,
    indexed_files: Vec<String>,
) {
    let Some(document_uri) = hook_feedback_document_uri(&project_root, &indexed_files, &request)
    else {
        return;
    };
    let diagnostic_trigger = match request.trigger {
        Pr13HookOrchestrationTriggerV1::SavedEdit => DiagnosticTrigger::DocumentSave,
        Pr13HookOrchestrationTriggerV1::Stop | Pr13HookOrchestrationTriggerV1::Explicit => {
            DiagnosticTrigger::ExplicitDocumentDiagnostics
        }
    };
    let Ok(mut invocation) = feedback_lsp_input(FeedbackCycleRequest {
        root_uri,
        document_uri,
        trigger: diagnostic_trigger,
    })
    .await
    else {
        return;
    };
    if request.trigger == Pr13HookOrchestrationTriggerV1::Stop {
        invocation.request.input.request.trigger = FeedbackTriggerV1::AgentStopGate;
        let Ok(validated) =
            Pr12FeedbackCycleInvocation::new(invocation.context, invocation.request)
        else {
            return;
        };
        invocation = validated;
    }
    let observed_at = invocation.request.input.observed_at;
    let expires_at = UtcMicros(observed_at.0.saturating_add(5 * 60 * 1_000_000));
    let operation_authority = daemon_operation_event_authority();
    let Ok(operation) = operation_authority
        .begin(
            &invocation.context,
            OperationKind::FeedbackDiagnostics,
            observed_at,
        )
        .await
    else {
        return;
    };
    let advisory = Pr13AdvisoryCycleRequestV1 {
        feedback: invocation.request,
        github: github_pull_request_id.map(|pull_request_id| GitHubReviewReadRequestV1 {
            operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
            scope: feedback_scope.clone(),
            pull_request_id,
        }),
        ci: None,
        proximity: Some(ProximityEvaluationRequestV1 {
            scope: feedback_scope.clone(),
            observed_at,
        }),
        validity: tracedecay_application::AdvisoryFindingValidityWindowV1 {
            valid_at: observed_at,
            expires_at,
        },
    };
    let host = host_kind_for_hook(request.hook.envelope().producer);
    let rollback = HookFeedbackRollbackSwitchV1 {
        configuration_revision: request.hook_configuration_revision,
        route: HookFeedbackDeliveryRouteV1::HookV2,
    };
    if registration
        .run_once(
            &invocation.context,
            Pr13AdvisoryCycleControlV1 {
                operation,
                deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            },
            advisory,
            host,
            rollback,
        )
        .await
        .is_err()
    {
        return;
    }
    let Some(lifecycle) = request.lifecycle else {
        return;
    };
    let Some(pin) = ContextScoutAuthorityPinV1::new(
        &invocation.context,
        feedback_scope,
        scout_configuration,
        observed_at,
    ) else {
        return;
    };
    let assembler = ContextScoutCanonicalInputAssemblerV1::new(
        scout_registry.as_ref(),
        feedback_runtime.as_ref(),
    );
    let Some(canonical) = assembler
        .bind_and_assemble(
            &request.hook,
            &pin,
            lifecycle,
            &invocation.context,
            observed_at,
        )
        .await
    else {
        return;
    };
    let window = match request.trigger {
        Pr13HookOrchestrationTriggerV1::SavedEdit => ContextScoutDeliveryWindowV1::NextBoundary,
        Pr13HookOrchestrationTriggerV1::Stop => ContextScoutDeliveryWindowV1::Immediate,
        Pr13HookOrchestrationTriggerV1::Explicit => ContextScoutDeliveryWindowV1::OnRequest,
    };
    let Some(selection) = canonical.selection_input(&request.hook, observed_at, window) else {
        return;
    };
    let _ = scout_owner
        .prepare_configured(
            &selection,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            CancellationToken::new(),
        )
        .await;
}

fn hook_feedback_document_uri(
    project_root: &Path,
    indexed_files: &[String],
    request: &Pr13HookOrchestrationRequestV1,
) -> Option<String> {
    let logical_path = match &request.hook.envelope().event {
        tracedecay_hooks::HookEventV2::SavedEdit { file_id, .. } => {
            indexed_files.iter().find(|logical_path| {
                hash16(logical_path.as_bytes()) == *file_id
                    || hash16(project_root.join(logical_path).to_string_lossy().as_bytes())
                        == *file_id
            })?
        }
        _ => indexed_files.first()?,
    };
    url::Url::from_file_path(project_root.join(logical_path))
        .ok()
        .map(Into::into)
}

fn hash16(value: &[u8]) -> [u8; 16] {
    let digest = Sha256::digest(value);
    let mut value = [0_u8; 16];
    value.copy_from_slice(&digest[..16]);
    value
}

fn optional_remote_provider_configuration(
    github: Option<GitHubReviewRuntimeOwnerConfigV1>,
) -> Result<(
    Option<GitHubReviewRuntimeOwnerConfigV1>,
    Option<ProductionCiProviderConfigV1>,
    Option<GitHubPullRequestIdV1>,
)> {
    let ci_config = github
        .as_ref()
        .map(|github| {
            Ok::<ProductionCiProviderConfigV1, TraceDecayError>(ProductionCiProviderConfigV1 {
                provider: ProviderId::new("provider.github-actions").map_err(|error| {
                    TraceDecayError::Config {
                        message: format!("project-open CI provider identity failed: {error}"),
                    }
                })?,
                parser: CiFailureParserIdentityV1 {
                    parser_id: "parser.github-actions.v1".to_owned(),
                    parser_version: "1".to_owned(),
                },
                target: github.target.clone(),
                credential: github.credential.clone(),
                http: github.http.clone(),
            })
        })
        .transpose()?;
    let github_pull_request_id = github
        .as_ref()
        .map(|github| github.target.pull_request_id.clone());
    Ok((github, ci_config, github_pull_request_id))
}

const fn host_kind_for_hook(host: HookHostV1) -> HostKindV1 {
    match host {
        HookHostV1::ClaudeCode => HostKindV1::ClaudeCode,
        HookHostV1::Codex => HostKindV1::Codex,
        HookHostV1::CursorDesktop => HostKindV1::CursorDesktop,
        HookHostV1::CursorCloud => HostKindV1::CursorCloud,
        HookHostV1::Hermes => HostKindV1::Hermes,
        HookHostV1::Kiro => HostKindV1::Kiro,
        HookHostV1::KimiCode => HostKindV1::KimiCode,
        HookHostV1::OpenCode => HostKindV1::OpenCode,
        HookHostV1::Cline => HostKindV1::Cline,
        HookHostV1::RooCode => HostKindV1::RooCode,
        HookHostV1::Kilo => HostKindV1::Kilo,
    }
}

async fn resolve_production_github_review_config(
    project_root: &Path,
    database: crate::db::Database,
    resolved_scope: ResolvedScope,
    feedback_scope: FeedbackScopeV1,
) -> Option<GitHubReviewRuntimeOwnerConfigV1> {
    let (owner, repository) =
        github_repository_from_remote(&crate::tracedecay::git_remote_url(project_root)?)?;
    let head_commit_id = feedback_scope.head_commit_id.clone();
    let http = GitHubHttpReadConfigV1::default();
    let discovery_http = http.clone();
    let discovery = tokio::task::spawn_blocking(move || {
        discover_exact_commit_pull_request_v1(&owner, &repository, &head_commit_id, &discovery_http)
    })
    .await
    .ok()?;
    let GitHubExactCommitDiscoveryOutcomeV1::Found(pull) = discovery else {
        return None;
    };
    let target = pull.target.clone();
    let credential = resolve_production_github_credential()?;
    let identity =
        resolve_production_github_identity(project_root, &feedback_scope, &target, pull)?;
    Some(GitHubReviewRuntimeOwnerConfigV1 {
        database,
        resolved_scope,
        feedback_scope,
        target,
        credential,
        http,
        identity,
    })
}

fn github_repository_from_remote(remote: &str) -> Option<(String, String)> {
    let (owner, repository) = if let Ok(url) = url::Url::parse(remote) {
        if (url.scheme() != "https" && url.scheme() != "ssh")
            || !url.host_str()?.eq_ignore_ascii_case("github.com")
            || url.password().is_some()
            || (url.scheme() == "https" && !url.username().is_empty())
            || (url.scheme() == "ssh" && url.username() != "git")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let segments = url.path_segments()?.collect::<Vec<_>>();
        if segments.len() != 2 {
            return None;
        }
        (segments[0].to_owned(), segments[1].to_owned())
    } else {
        let remote = remote.strip_prefix("git@github.com:")?;
        let mut segments = remote.split('/');
        let owner = segments.next()?;
        let repository = segments.next()?;
        if segments.next().is_some() {
            return None;
        }
        (owner.to_owned(), repository.to_owned())
    };
    let repository = repository
        .strip_suffix(".git")
        .unwrap_or(&repository)
        .to_owned();
    let target = GitHubRepositoryTargetV1 {
        owner,
        repository,
        pull_request_number: 1,
        pull_request_id: GitHubPullRequestIdV1::new("1").ok()?,
    };
    target
        .validate()
        .then_some((target.owner, target.repository))
}

fn resolve_production_github_credential() -> Option<GitHubReadOnlyCredentialV1> {
    // `gh auth token` does not prove the token's effective GitHub permissions.
    // Relabeling it with locally declared read scopes could admit a
    // write-capable credential. Public-repository anonymous reads are the only
    // production credential posture until a registered authority supplies
    // verified provider permissions.
    Some(GitHubReadOnlyCredentialV1::anonymous())
}

fn resolve_production_github_identity(
    project_root: &Path,
    feedback_scope: &FeedbackScopeV1,
    target: &GitHubRepositoryTargetV1,
    pull: crate::application::advisory::github_runtime::GitHubExactCommitPullRequestV1,
) -> Option<GitHubReviewProviderIdentityV1> {
    let base = pull.base_commit_id;
    let head = pull.head_commit_id;
    if pull.target != *target || head != feedback_scope.head_commit_id {
        // Keep the advisory target bound to the admitted feedback head.
        return None;
    }
    let merge_base = Command::new(crate::git::git_program())
        .args([
            "-C",
            &project_root.to_string_lossy(),
            "merge-base",
            base.as_str(),
            head.as_str(),
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| {
            matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })?;
    let identity = GitHubReviewProviderIdentityV1 {
        provider: ProviderId::new("provider.github").ok()?,
        repository_owner: target.owner.clone(),
        repository_name: target.repository.clone(),
        pull_request_number: target.pull_request_number,
        base_commit_id: base,
        head_commit_id: head,
        merge_base_commit_id: CommitId::new(merge_base).ok()?,
    };
    identity.validate().then_some(identity)
}

fn daemon_owned_project_source_access(
    scope: &ResolvedScope,
    project_root: &Path,
    configuration: &crate::config::PinnedRuntimeConfiguration,
) -> std::result::Result<ProjectSourceAccessSnapshot, ApplicationContractError> {
    let locator = locator_digest_for_project(project_root)?;
    let locator = LocatorDigest::new(locator.as_str().to_owned()).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open locator digest",
        }
    })?;
    let binding = ScopeSourceBinding::new(
        SourceBindingId::new(DAEMON_BINDING.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open source binding id",
            }
        })?,
        SourceKindV1::Cursor,
        locator,
        AuthorityRef::Project(scope.project_id.clone()),
    )
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open source binding",
    })?;
    if configuration.target.project_id != scope.project_id {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open configuration project",
        });
    }
    configuration
        .snapshot
        .validate()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open configuration snapshot",
        })?;
    Ok(ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester: ActorId::new(DAEMON_REQUESTER.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open requester",
            }
        })?,
        binding,
        configuration_revision: configuration.revision_id.clone(),
        configuration_digest: configuration.snapshot.effective_behavior_digest.clone(),
        configuration_provenance_digest: configuration
            .snapshot
            .resolution_provenance_digest
            .clone(),
        effective_capabilities: production_owner_capabilities()?,
        grant_expires_at: UtcMicros(
            now_micros()
                .0
                .saturating_add(i64::try_from(GRANT_HORIZON.as_micros()).unwrap_or(i64::MAX)),
        ),
    })
}

fn production_owner_capabilities()
-> std::result::Result<BTreeSet<CapabilityId>, ApplicationContractError> {
    let mut capabilities = BTreeSet::new();
    for capability in [
        FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
        FEEDBACK_GET_CAPABILITY_ID_V1,
        FEEDBACK_EXPAND_CAPABILITY_ID_V1,
        FEEDBACK_LIST_CAPABILITY_ID_V1,
        "capability.application.feedback.impact",
        "capability.application.feedback.affected-tests",
        "capability.application.feedback.test-results",
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        PROXIMITY_CAPABILITY_ID_V1,
        "capability.application.primitive.session-lookup",
        "capability.application.primitive.qualified-name",
        "capability.application.primitive.call-chain",
        "capability.application.primitive.file-dependents",
        "capability.application.primitive.source-lines",
        "capability.application.primitive.source-body",
        "capability.application.primitive.source-outline",
        "capability.application.primitive.module-api",
        "capability.application.primitive.file-metadata",
        "capability.application.primitive.health-read",
        "capability.application.primitive.storage-status",
        "capability.application.primitive.diagnostics-read",
        "capability.git.stage-hunks",
        "capability.git.unstage-hunks",
        "capability.git.commit-index",
    ] {
        capabilities.insert(CapabilityId::new(capability.to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open capability",
            }
        })?);
    }
    Ok(capabilities)
}

pub(crate) fn resolved_scope_for_project(
    project_root: &Path,
    project_id: &ProjectId,
) -> std::result::Result<ResolvedScope, ApplicationContractError> {
    let repository_digest = locator_digest_for_project(project_root)?;
    let repository_id = RepositoryId::new(format!(
        "repository.{}",
        repository_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open repository id",
    })?;
    let worktree_id = worktree_id_for_project(project_root)?;
    let reference = crate::branch::current_branch(project_root)
        .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
    ResolvedScope::new(project_id.clone(), repository_id, worktree_id, reference).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open resolved scope",
        }
    })
}

fn now_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted(language: &str, analyzer_available: bool) -> AdmittedLspProvider {
        AdmittedLspProvider {
            language: language.to_owned(),
            command: format!("{language}-language-server"),
            analyzer_available,
        }
    }

    #[test]
    fn absent_analyzer_still_mounts_graph_and_managed_lsp_capabilities() {
        let admitted = [admitted("rust", false)];
        let (language, gateway) = production_lsp_registration(&admitted);

        assert_eq!(language, Some("rust"));
        assert!(gateway.supports_managed_diagnostics);
        assert_eq!(gateway.semantic, graph_semantic_capabilities());
    }

    #[test]
    fn registration_language_prefers_mounted_python_or_typescript_provider() {
        for language in ["python", "typescript"] {
            let admitted = [
                admitted("rust", false),
                admitted(language, true),
                admitted("go", false),
            ];
            let (selected, gateway) = production_lsp_registration(&admitted);

            assert_eq!(selected, Some(language));
            assert_eq!(gateway.semantic, graph_semantic_capabilities());
        }
    }

    #[test]
    fn unavailable_legacy_hook_accepts_each_notice_borrow_without_retaining_it() {
        fn notice(suffix: &str) -> Pr13AdvisoryHookLookupNoticeV1 {
            Pr13AdvisoryHookLookupNoticeV1 {
                scope: FeedbackScopeV1 {
                    project_id: ProjectId::new("project.hook-lifetime").expect("project"),
                    repository_id: RepositoryId::new("repository.hook-lifetime")
                        .expect("repository"),
                    worktree_id: tracedecay_domain::WorktreeId::new("worktree.hook-lifetime")
                        .expect("worktree"),
                    branch_ref: "refs/heads/main".to_owned(),
                    head_commit_id: CommitId::new("a".repeat(40)).expect("commit"),
                },
                result_id: tracedecay_domain::feedback::FeedbackResultId::new(format!(
                    "result.{suffix}"
                ))
                .expect("result"),
                cycle_id: tracedecay_domain::feedback::FeedbackCycleId::new(format!(
                    "cycle.{suffix}"
                ))
                .expect("cycle"),
                returned_findings: 0,
                omitted_findings: 0,
            }
        }

        let sink = unavailable_advisory_hook_sink();
        let first = notice("first");
        assert_eq!(
            sink(&first),
            tracedecay_hooks::HookFeedbackDeliveryOutcomeV1::Unavailable
        );
        let second = notice("second");
        assert_eq!(
            sink(&second),
            tracedecay_hooks::HookFeedbackDeliveryOutcomeV1::Unavailable
        );
    }

    #[test]
    fn non_github_project_keeps_remote_provider_configuration_optional() {
        let (github, ci, pull_request) =
            optional_remote_provider_configuration(None).expect("optional provider config");

        assert!(github.is_none());
        assert!(ci.is_none());
        assert!(pull_request.is_none());
    }
}

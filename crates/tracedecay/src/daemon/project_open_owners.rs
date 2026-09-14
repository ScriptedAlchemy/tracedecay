//! Project-open registration for the daemon's production owners.
//!
//! After Scout bootstrap and successful cache publication, the daemon mounts
//! each concrete owner from the admitted project identity. Owners mount only
//! when their real upstream authorities resolve; missing identity fails closed
//! and placeholder owners are never installed.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracedecay_application::advisory::GitHubRepositoryTargetV1;
use tracedecay_application::project_open_authorization::project_open_work_grant;
use tracedecay_contracts::{ApplicationContractError, ResolvedScope, now_micros};
use tracedecay_domain::feedback::GitHubPullRequestIdV1;
use tracedecay_domain::{ProjectId, UtcMicros, canonical_sha256};

use super::DaemonInvocationState;
use crate::mcp::McpServer;
use tracedecay_agent_hosts::native_integration::{
    DaemonNativeIntegrationAnalysisV1, NativeIntegrationTargetV1,
};
use tracedecay_application::lsp_runtime::DaemonLspSessionFactory;
use tracedecay_application::primitives::admitted_root_uri_for_project;
use tracedecay_application::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_code_index_runtime::git_transactions::DaemonGitIndexTransactionServiceRegistry;
use tracedecay_daemon_service::{
    DaemonCallableCodeAuthorizationSource, DaemonContextScoutRuntimeRegistrationError,
    DaemonFeedbackRuntimeRegistrationError, DaemonNativeIntegrationRuntimeRegistrar,
    DaemonWorkProposalRoutingAuthorityV1, daemon_owned_project_source_access_at,
    project_open_source_access_authority,
    project_owner_registration::{
        ProjectSourceEditAuthorizationV1, ProjectSourceEditOwnerV1, SourceEditMutationGate,
        production_lsp_registration, project_open_lsp_scope_grant,
    },
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_lsp::analyzer::broker::AdmittedLspProvider;
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;

mod advisory_runtime;
mod automation_effect_recovery;
#[cfg(test)]
#[path = "project_open_owners/code_index_reads/ignored_dependency_admission_tests.rs"]
mod code_index_ignored_dependency_admission_tests;
mod primitive_runtime;
mod query_authority_upgrade;

pub(crate) use advisory_runtime::ProjectOpenDependentOwnerState;
pub(super) use advisory_runtime::register_project_open_dependent_owners;
pub(crate) use automation_effect_recovery::reconcile_project_open_automation_effects;

use primitive_runtime::open_and_register_project_primitive_runtime;

const POLICY_REVISION_V1: u64 = 1;
const LSP_DIAGNOSTICS_QUIET: Duration = Duration::from_secs(2);
pub(super) use tracedecay_daemon_service::{
    LSP_WORKSPACE_CAPABILITY_ID_V1, LSP_WORKSPACE_USE_CASE_ID_V1,
};

async fn install_project_open_source_edit_owners(
    server: &McpServer,
    project_root: &Path,
    owner: Arc<ProjectSourceEditOwnerV1>,
) -> Result<()> {
    let Some(service) = server.daemon_invocation_service() else {
        return Err(TraceDecayError::Config {
            message: "project-open source edit authority requires the daemon invocation service"
                .to_owned(),
        });
    };
    service
        .register_source_edit_owner(project_root.to_path_buf(), owner)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open source edit authority failed to register: {error}"),
        })
}

pub(crate) async fn install_project_open_source_edit_preview_owner(
    server: &McpServer,
    graph: Arc<crate::project::TraceDecay>,
    code_graph: Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort>,
    project_root: &Path,
    project_id: &str,
) -> Result<Arc<SourceEditMutationGate>> {
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|_| TraceDecayError::Config {
            message: "project-open source edit preview requires authoritative project identity"
                .to_owned(),
        })?;
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(project_root, &project_id)
            .map_err(|error| TraceDecayError::Config {
                message: format!("project-open source edit preview scope denied: {error}"),
            })?;
    let catalog = tracedecay_contracts::catalog_composition::build_application_catalog_snapshot()
        .map_err(|error| TraceDecayError::Config {
        message: format!("project-open source edit catalog is unavailable: {error}"),
    })?;
    let authorization = ProjectSourceEditAuthorizationV1::new(
        project_root.to_path_buf(),
        scope,
        Arc::clone(graph.configuration_runtime()),
        Arc::new(catalog),
        Arc::new(project_open_source_access_authority().map_err(|error| {
            TraceDecayError::Config {
                message: format!("project-open source edit access authority is invalid: {error}"),
            }
        })?),
    );
    let runtime: Arc<dyn tracedecay_source_edit::SourceEditRuntimePort> = graph;
    let mutation = SourceEditMutationGate::warming();
    let owner = Arc::new(ProjectSourceEditOwnerV1::new(
        runtime,
        code_graph,
        authorization,
        Arc::clone(&mutation),
    ));
    install_project_open_source_edit_owners(server, project_root, owner).await?;
    Ok(mutation)
}

#[cfg(feature = "test-transport")]
pub(crate) async fn install_project_open_source_edit_owners_for_test(
    server: &McpServer,
) -> Result<bool> {
    let graph = server.cg().await;
    if server.daemon_invocation_service().is_none() {
        return Ok(false);
    }
    let Some(code_graph) = server.code_graph_projection_read_port() else {
        // A directly constructed test server carries no production code-graph
        // projection port, so the daemon-owned source-edit authority cannot
        // mount. Report that typed state instead of failing: the dispatch
        // boundary (selector rejection, argument validation) is still the
        // production path, and an actual edit then reports its typed
        // executor-unavailable refusal rather than dying here before dispatch.
        return Ok(false);
    };
    let project_root = graph.project_root().to_path_buf();
    let project_id = graph
        .configuration_runtime()
        .configuration_target()
        .project_id
        .clone();
    let mutation = install_project_open_source_edit_preview_owner(
        server,
        graph,
        code_graph,
        &project_root,
        project_id.as_str(),
    )
    .await?;
    mutation.mark_ready();
    Ok(true)
}

/// Registers code-index-independent owners for one newly inserted project.
#[hotpath::measure(label = "daemon.project.owners.register", future = true)]
#[expect(
    clippy::too_many_lines,
    reason = "Production owner registration is one ordered phase list for a project open."
)]
pub(super) async fn register_project_open_production_owners(
    invocation: &DaemonInvocationState,
    git_transactions: &DaemonGitIndexTransactionServiceRegistry,
    native_integration: &DaemonNativeIntegrationRuntimeRegistrar,
    project_root: &Path,
    project_id: &str,
    server: &McpServer,
    source_edit_mutation: Arc<SourceEditMutationGate>,
) -> Result<ProjectOpenDependentOwnerState> {
    // Retain the admitted owner state once across its asynchronous phases.
    Box::pin(async move {
    let owner_registration_started = Instant::now();
    let mut owner_phase_started = owner_registration_started;
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|_| TraceDecayError::Config {
            message: "project-open owners require an authoritative project identity".to_owned(),
        })?;
    let graph = server.cg().await;
    let code_graph =
        server
            .code_graph_projection_read_port()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project-open owners require the verified code-graph projection port"
                    .to_owned(),
            })?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "registered_project_storage_acquired",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let database = graph.db().clone();
    let session_db = server
        .project_session_db()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open owners require the daemon-owned project session database"
                .to_owned(),
        })?;
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(project_root, &project_id)
            .map_err(|error| TraceDecayError::Config {
                message: format!("project-open resolved scope denied: {error}"),
            })?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "owner_scope_resolved",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let configuration = hotpath::future!(
        graph.configuration_runtime().client().current(),
        label = "daemon.project.open.owners.configuration_read"
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open configuration currentness failed: {error}"),
    })?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "configuration_current",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let scout_configuration =
        tracedecay_global_db::configuration::contracts::ports::ConfigurationCurrentStateV1 {
        revision_id: configuration.revision_id().clone(),
        snapshot: configuration.snapshot().clone(),
    };
    let _scout_registry = match hotpath::future!(
        invocation
            .context_scout_runtime_registrar()
            .open_and_register(
                database.clone(),
                session_db.binding().shard_id.profile_id.clone(),
                project_id.clone(),
                project_root.to_path_buf(),
            ),
        label = "daemon.project.open.owners.scout"
    )
    .await
    {
        Ok(registry) => registry,
        Err(DaemonContextScoutRuntimeRegistrationError::AlreadyRegistered) => invocation
            .context_scout_runtime_registrar()
            .get(
                &session_db.binding().shard_id.profile_id,
                &project_id,
                project_root,
            )
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
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "context_scout_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let access =
        daemon_owned_project_source_access_at(&scope, project_root, &configuration, now_micros())
            .map_err(|error| TraceDecayError::Config {
            message: format!("project-open source access denied: {error}"),
        })?;
    let grant_expires_at = access.grant_expires_at;
    let requester = access.requester.clone();
    // Primitive reads are part of the admitted core route. Publish their
    // runtime and callable-code authorization before the slower mutation,
    // delivery, native-integration, and Work owners finish mounting.
    match hotpath::future!(
        invocation.feedback_runtime_registrar().open_and_register(
            database.clone(),
            project_root.to_path_buf(),
            scope.clone(),
            access.clone(),
            Arc::new(DaemonCallableCodeAuthorizationSource::production(
                project_root.to_path_buf(),
                scope.clone(),
                Arc::clone(graph.configuration_runtime()),
            )),
        ),
        label = "daemon.project.open.owners.feedback"
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
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "feedback_runtime_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();

    let admitted_root_uri =
        admitted_root_uri_for_project(project_root).map_err(|error| TraceDecayError::Config {
            message: format!("project-open admitted root URI denied: {error}"),
        })?;
    let source = graph
        .source_read_context()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open primitive runtime requires an exact registered source identity"
                .to_owned(),
        })?;
    open_and_register_project_primitive_runtime(
        invocation,
        project_root,
        source,
        server,
        session_db.clone(),
        access.clone(),
        &admitted_root_uri,
    )
    .await?;
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "primitive_runtime_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();

    // One worktree discovery serves both the Git transaction authority here
    // and the native-integration mount below.
    let repository_root = tracedecay_runtime_core::worktree::git_worktree_root(project_root);
    if let Some(repository_root) = repository_root.as_deref() {
        hotpath::future!(
            git_transactions.install_authority(
                repository_root,
                access.clone(),
                session_db.clone(),
                tokio::runtime::Handle::current(),
            ),
            label = "daemon.project.open.owners.git_authority"
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Git authority registration failed: {error}"),
        })?;
    }
    // Preview executors were published with the read-only core. Open their
    // mutation lane only after the exact Git transaction authority exists.
    // A later failure in this function retires the whole server, and project
    // open marks the lane failed as it does so, so the lane never stays warming.
    source_edit_mutation.mark_ready();
    let configuration_policy_digest =
        super::project_delivery_mount::ensure_project_delivery_settlement(
            invocation,
            project_root,
            session_db.clone(),
            &scope,
            &access,
        )
        .await?;
    let configuration_profile_id = server
        .profile_identity()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open configuration requires exact profile authority".to_owned(),
        })?
        .profile_id()
        .clone();
    hotpath::future!(
        invocation.configuration_runtime_registrar().register(
            project_root.to_path_buf(),
            Arc::clone(graph.configuration_runtime()),
            scope.clone(),
            configuration_profile_id,
            requester.clone(),
            grant_expires_at,
            None,
            configuration_policy_digest.clone(),
        ),
        label = "daemon.project.open.owners.configuration"
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open configuration runtime registration failed: {error}"),
    })?;
    let retained_observed_at = now_micros();
    let retained_grant =
        project_open_retained_grant(&access, retained_observed_at).map_err(|error| {
            TraceDecayError::Config {
                message: format!("project-open retained grant is invalid: {error}"),
            }
        })?;
    let retained_ports = server.retained_surface_ports(
        project_root,
        scope.project_id.clone(),
        access.configuration_digest.clone(),
    );
    hotpath::future!(
        invocation.retained_runtime_registrar().register(
            project_root.to_path_buf(),
            scope.clone(),
            requester.clone(),
            retained_grant,
            retained_ports,
        ),
        label = "daemon.project.open.owners.retained"
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open retained runtime registration failed: {error}"),
    })?;
    // Mount the native-integration authority under the same pinned policy
    // digest the configuration runtime just registered, so the coordinator's
    // stale/denied predicates and the handler's minted grants agree on one
    // policy identity. Non-Git projects advertise no native mutation
    // authority; the handler keeps answering the typed unavailable result.
    let native_owner = if let Some(repository_root) = repository_root {
        let analysis = Arc::new(DaemonNativeIntegrationAnalysisV1::new(
            invocation.code_index_schedulers.clone(),
            scope.clone(),
            tokio::runtime::Handle::current(),
        ));
        let native_owner = hotpath::future!(
            async {
                let native_owner = native_integration
                    .ensure(
                        session_db.clone(),
                        NativeIntegrationTargetV1 {
                            repository_root,
                            project_id: scope.project_id.clone(),
                            repository_id: scope.repository_id.clone(),
                            policy_digest: configuration_policy_digest.clone(),
                        },
                        now_micros(),
                        analysis,
                    )
                    .await
                    .map_err(|error| TraceDecayError::Config {
                        message: format!(
                            "project-open native integration authority registration failed: {error}"
                        ),
                    })?;
                invocation
                    .service
                    .install_worktree_cleanup_recovery_fences(&native_owner)
                    .await
                    .map_err(|error| TraceDecayError::Config {
                        message: format!(
                            "project-open worktree cleanup recovery fencing failed: {error}"
                        ),
                    })?;
                Ok::<_, TraceDecayError>(native_owner)
            },
            label = "daemon.project.open.owners.native"
        )
        .await?;
        Some(native_owner)
    } else {
        None
    };
    let work_grant = project_open_work_grant(&access, now_micros()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("project-open Work grant is invalid: {error}"),
        }
    })?;
    let work_topology_policy =
        tracedecay_configuration::config::topology::resolved_work_topology_policy(
            configuration.snapshot(),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open work topology policy is unavailable: {error}"),
        })?
        .clone();
    // Project-open has no authenticated GitHub response or persisted source
    // record. It mounts policy and delivery only; the review refresh owner is
    // the sole producer of canonical provider observations and anchors.
    if tracedecay_runtime_core::git::git_remote_url(project_root)
        .as_deref()
        .and_then(github_repository_from_remote)
        .is_some()
    {
        let stack_coordinator = invocation.github_stack_coordinator();
        stack_coordinator
            .register_scope(
                &scope,
                work_topology_policy.review_topology.github_stacked_prs,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "project-open GitHub stack coordinator registration failed: {error:?}"
                ),
            })?;
        if let Some(native_owner) = native_owner.as_ref() {
            let stack_runtime = native_owner
                .mount_github_stack_runtime(
                    session_db.clone(),
                    scope.clone(),
                    access.clone(),
                    Arc::clone(&stack_coordinator),
                )
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "project-open GitHub stack delivery runtime registration failed: {error:?}"
                    ),
                })?;
            tracedecay_agent_hosts::native_integration::register_github_stack_hook_runtime(
                &scope,
                &stack_runtime,
            );
        }
    }
    if let Some(work_grant) = work_grant {
        let work_authority = tracedecay_domain::WorkAuthority::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            requester.clone(),
            work_grant.digest.clone(),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Work authority is invalid: {error}"),
        })?;
        let work_proposal_routing = DaemonWorkProposalRoutingAuthorityV1::mount(
            scope.clone(),
            &configuration,
            &access.configuration_digest,
            &work_grant,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open Work proposal routing is unavailable: {error}"),
        })?;
        let work_evidence_retrieval =
            server.work_evidence_retrieval(&scope, invocation.work_federated_query_authority())?;
        hotpath::future!(
            async {
            invocation
                .work_runtime_registrar()
                .register(
                    project_root.to_path_buf(),
                    session_db.clone(),
                    work_authority.clone(),
                    requester.clone(),
                    work_grant.clone(),
                    configuration_policy_digest.clone(),
                    access.configuration_digest.clone(),
                    work_topology_policy,
                    work_proposal_routing,
                    work_evidence_retrieval,
                )
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "project-open Workflow authority registration failed: {error}"
                    ),
                })?;
            if !invocation
                .work_runtime_registrar()
                .authority_matches(
                    project_root,
                    &work_authority,
                    &requester,
                    &work_grant,
                    &configuration_policy_digest,
                    &access.configuration_digest,
                )
                .await
            {
                return Err(TraceDecayError::Config {
                    message: "project-open Workflow authority registration did not match the admitted project"
                        .to_owned(),
                });
            }
            Ok::<_, TraceDecayError>(())
            },
            label = "daemon.project.open.owners.work"
        )
        .await?;
    }
    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "configuration_runtime_registered",
        step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
        elapsed_ms = owner_registration_started.elapsed().as_millis(),
    );
    owner_phase_started = Instant::now();
    let mut mounted_providers = Vec::new();
    let mut lsp_session_factory = None;
    let diagnostic_broker = server.diagnostics_lsp();
    // Bounds the orchestration wait around the scheduler's generation decode;
    // the decode itself is instrumented inside the code-index subsystem.
    let indexed_generation = hotpath::future!(
        invocation
            .code_index_schedulers
            .latest_complete_ready_decoded_for_root_scope(project_root, &scope),
        label = "daemon.project.open.owners.lsp_census"
    )
    .await;
    if let Some(generation) = indexed_generation {
        let mut indexed_files = generation
            .generation()
            .snapshot()
            .files
            .iter()
            .map(|file| file.logical_path.clone())
            .collect::<Vec<_>>();
        indexed_files.sort();
        let admitted_providers = {
            let mut broker = diagnostic_broker.lock().await;
            let admitted = broker.admitted_providers_for_files(&indexed_files);
            mounted_providers = broker.mounted_providers_for_files(&indexed_files);
            admitted
        };
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "lsp_languages_discovered",
            step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
            elapsed_ms = owner_registration_started.elapsed().as_millis(),
        );
        owner_phase_started = Instant::now();

        // Feedback runtime registration installed a typed unavailable cycle.
        // The LSP gateway publishes only against a real sealed file census.
        let lsp_scope_grant =
            project_open_lsp_scope_grant(&access, now_micros()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("project-open LSP workspace grant is invalid: {error}"),
                }
            })?;
        lsp_session_factory = Some(
            register_production_lsp_owner(
                invocation,
                project_root,
                lsp_scope_grant,
                session_db.clone(),
                database.clone(),
                Arc::clone(&diagnostic_broker),
                &admitted_providers,
                admitted_root_uri.clone(),
            )
            .await?,
        );
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "lsp_owner_registered",
            step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
            elapsed_ms = owner_registration_started.elapsed().as_millis(),
        );
    } else {
        // Protocol sessions (initialize / shutdown / exit) must be admissible
        // as soon as the project route is published. Diagnostics still wait
        // for a sealed census; the deferred owner upgrade replaces this
        // warming registration when that generation arrives.
        let lsp_scope_grant =
            project_open_lsp_scope_grant(&access, now_micros()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("project-open LSP workspace grant is invalid: {error}"),
                }
            })?;
        register_production_lsp_owner(
            invocation,
            project_root,
            lsp_scope_grant,
            session_db.clone(),
            database.clone(),
            Arc::clone(&diagnostic_broker),
            &[],
            admitted_root_uri.clone(),
        )
        .await?;
        tracing::info!(
            event = "project_open_owner_phase",
            project = %project_root.display(),
            phase = "lsp_owner_registered",
            reason = "warming_without_sealed_generation",
            step_elapsed_ms = owner_phase_started.elapsed().as_millis(),
            elapsed_ms = owner_registration_started.elapsed().as_millis(),
        );
    }

    // Hook V2 envelopes that missed their synchronous budget are durable in
    // the per-host transport spool. Replay is project-scoped, not Git-scoped:
    // non-Git and unborn projects must drain their admitted envelopes too.
    let delivery_settlements = invocation
        .service
        .delivery_settlement_authority(Some(project_root))
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("hook delivery settlement authority is invalid: {error}"),
        })?
        .ok_or_else(|| TraceDecayError::Config {
            message: "hook delivery settlement authority is unavailable".to_owned(),
        })?;
    crate::daemon::hook_v2_replay_consumer::register_hook_v2_replay_consumer(
        Arc::clone(&graph),
        delivery_settlements,
        session_db.clone(),
        server.background_cpu_authority().ok_or_else(|| TraceDecayError::Config {
            message: "project-open hook replay requires the daemon background CPU authority"
                .to_owned(),
        })?,
    );

    // At-rest privacy remediation is bounded background work after fail-closed
    // admission; it never blocks admission or retrieval.
    let privacy_graph = Arc::clone(&graph);
    let privacy_session_db = session_db.clone();
    let privacy_grant =
        tracedecay_privacy::PrivacyRemediationGrantV1::new(access.grant_expires_at, now_micros());
    let privacy_read =
        tracedecay_privacy::granted_remediation_read_control(&privacy_grant, now_micros);
    let privacy_write =
        tracedecay_privacy::granted_remediation_write_control(&privacy_grant, now_micros);
    let _privacy_remediation_admitted = tracedecay_privacy::spawn_at_rest_privacy_remediation(
        |task| server.spawn_background_task(task),
        tracedecay_privacy::AdmittedPrivacyProjectV1::new(
            project_id.clone(),
            project_root.display(),
        ),
        privacy_grant,
        async move {
            privacy_graph
                .project_memory_application()?
                .privacy_remediation_rescan(
                    tracedecay_session_memory::memory::PrivacyRemediationTriggerV1::DetectorRevisionAdoption,
                    &privacy_read,
                    &privacy_write,
                )
                .await
                .map(Into::into)
                .map_err(tracedecay_session_memory::memory::memory_application_error)
        },
        async move {
            privacy_session_db
                .lcm_privacy_rescan_raw_messages()
                .await
                .map(Into::into)
        },
        now_micros,
    );

    // Once-per-project-open adoption-eligibility census over the composed
    // capability catalog, recorded through the project-bound session
    // authority. Fire-and-forget telemetry: project open never blocks or
    // fails on observation storage.
    let census_db = session_db.clone();
    let census_project_root = project_root.to_path_buf();
    let _adoption_census_admitted = server.spawn_background_task(async move {
        tracedecay_daemon_service::adoption_observation::record_project_open_adoption_census(
            census_db.as_ref(),
            &census_project_root,
        )
        .await;
    });

    tracing::info!(
        event = "project_open_owner_phase",
        project = %project_root.display(),
        phase = "independent_owners_registered",
    );

    Ok(ProjectOpenDependentOwnerState {
        database,
        session_db,
        graph,
        code_graph,
        scope,
        access,
        scout_configuration,
        requester,
        mounted_providers,
        admitted_root_uri,
        diagnostic_broker,
        lsp_session_factory,
    })
        }).await
}

/// Mounts the checked-in exact/lexical/graph query authority for the admitted
/// scope. A missing generation parks a deferred mount on the project owner;
/// every other refusal leaves the non-search surfaces mounted.
#[hotpath::measure(label = "daemon.project.activate.query_authority", future = true)]
async fn register_project_query_authority(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    server: &McpServer,
    session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    scope: ResolvedScope,
) {
    let cursor_keys = match session_db.load_session_cursor_key_provider_result().await {
        Ok(cursor_keys) => cursor_keys,
        Err(error) => {
            tracing::debug!(
                event = "query_authority_mount",
                outcome = "unavailable",
                project_id = %scope.project_id,
                reason = %error,
                "durable query cursor key is unavailable; project admission continues"
            );
            return;
        }
    };
    let Err(error) = invocation
        .mount_core_query_authority_for_project(project_root, &scope, &cursor_keys)
        .await
    else {
        return;
    };
    tracing::debug!(
        event = "query_authority_mount",
        outcome = "unavailable",
        project_id = %scope.project_id,
        reason = %error,
        "query search authority unavailable; non-search project surfaces remain mounted"
    );
    if matches!(
        error,
        tracedecay_code_index_runtime::code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1::GenerationUnavailable
    ) {
        query_authority_upgrade::spawn_deferred_query_authority_mount(
            server,
            invocation.clone(),
            project_root.to_path_buf(),
            scope,
            session_db,
        );
    }
}

#[hotpath::measure(label = "daemon.project.activate.lsp", future = true)]
#[allow(
    clippy::too_many_arguments,
    reason = "Composition supplies distinct capability, database, analyzer and diagnostic owners without merging their authority."
)]
async fn register_production_lsp_owner(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    scope_grant: tracedecay_contracts::CapabilityGrantSnapshot,
    registered_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    database: tracedecay_runtime_core::db::Database,
    diagnostic_broker: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    admitted_providers: &[AdmittedLspProvider],
    root_uri: String,
) -> Result<Arc<DaemonLspSessionFactory>> {
    let (languages, gateway_capabilities) = production_lsp_registration(admitted_providers);
    invocation
        .lsp_owner_registrar()
        .build_and_register(
            project_root.to_path_buf(),
            scope_grant,
            registered_database,
            database,
            Arc::new(invocation.code_index_schedulers.clone()),
            tokio::runtime::Handle::current(),
            diagnostic_broker,
            &languages,
            root_uri,
            LspRefreshTimeouts::from_diagnostics_quiet_window(LSP_DIAGNOSTICS_QUIET),
            LSP_DIAGNOSTICS_QUIET,
            gateway_capabilities,
        )
        .await
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

pub(super) fn project_open_retained_grant(
    access: &ProjectSourceAccessSnapshot,
    observed_at: UtcMicros,
) -> std::result::Result<tracedecay_contracts::CapabilityGrantSnapshot, ApplicationContractError> {
    let operations = tracedecay_contracts::RetainedSurfaceOperation::CALLABLE
        .into_iter()
        .map(tracedecay_contracts::retained_surface_application_operation)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let capabilities = operations
        .iter()
        .map(|operation| operation.capability_id().clone())
        .collect::<BTreeSet<_>>();
    if observed_at >= access.grant_expires_at
        || !capabilities
            .iter()
            .all(|capability| access.effective_capabilities.contains(capability))
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open retained capability grant",
        });
    }
    let use_cases = operations
        .iter()
        .map(|operation| operation.use_case_id().clone())
        .collect::<BTreeSet<_>>();
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.retained-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
        &capabilities,
        &use_cases,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open retained grant digest",
    })?;
    tracedecay_contracts::CapabilityGrantSnapshot::new(
        tracedecay_contracts::CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.project-open.retained.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        access.grant_expires_at,
        access.scope.clone(),
        capabilities,
        use_cases,
        tracedecay_contracts::DisclosureClass::Sensitive,
    )
}

#[cfg(test)]
#[path = "project_open_owners/git_catalog_tests.rs"]
mod git_catalog_tests;

//! Daemon-retained automation execution for dashboard project states.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
use tracedecay_agent_hosts::automation::config::{AutomationConfig, from_configuration_snapshot};
use tracedecay_agent_hosts::automation::managed_skills::{
    ManagedSkill, apply_managed_skill_update, archive_managed_skill, disable_managed_skill,
    load_managed_skill, managed_skill_dir, preview_managed_skill_update, restore_managed_skill,
    save_managed_skill,
};
use tracedecay_agent_hosts::automation::run_ledger::AutomationTrigger;
use tracedecay_agent_hosts::automation::runner::{
    MemoryCuratorAutomationOptions, SessionReflectorAutomationOptions,
    SkillWriterAutomationOptions, run_memory_curator_with_backend,
    run_session_reflector_with_backend, run_skill_writer_with_backend,
};
use tracedecay_agent_hosts::automation::skill_writer::deploy_managed_skills_to_project;
use tracedecay_agent_hosts::automation::{AutomationRunControl, AutomationRunError};
use tracedecay_application::now_micros;
use tracedecay_automation::managed_skills::validate_skill_id;
use tracedecay_dashboard_api::{
    DashboardAutomationAuthorityErrorV1, DashboardAutomationAuthorityV1,
    DashboardAutomationObservationRecorderV1, DashboardAutomationRunOutcomeV1,
    DashboardAutomationRunPortV1, DashboardAutomationRunRequestV1, DashboardAutomationWriter,
    DashboardHttpRequestControlV1, DashboardManagedSkillCommandOutcomeV1,
    DashboardManagedSkillCommandPortV1, DashboardManagedSkillCommandV1,
};
use tracedecay_domain::configuration::UserProfileId;

use crate::errors::{Result, TraceDecayError};
use crate::mcp::server::{RetainedProjectGraphRequest, RetainedProjectServerResolver};
use crate::tracedecay::TraceDecay;

type DashboardAutomationResult<T> = std::result::Result<T, DashboardAutomationAuthorityErrorV1>;
type DashboardAutomationProjectFuture = std::pin::Pin<
    Box<dyn Future<Output = DashboardAutomationResult<Arc<TraceDecay>>> + Send + 'static>,
>;
type DashboardAutomationProjectResolver =
    Arc<dyn Fn(PathBuf) -> DashboardAutomationProjectFuture + Send + Sync + 'static>;

const MEMORY_CURATOR_REQUEST_TIMEOUT_SECS: u64 = 80;
const SESSION_REFLECTOR_REQUEST_TIMEOUT_SECS: u64 = 120;
const SKILL_WRITER_REQUEST_TIMEOUT_SECS: u64 = 120;

struct DashboardAutomationRequestRuntime {
    config: AutomationConfig,
    backend: CodexAppServerBackend,
}

impl DashboardAutomationRequestRuntime {
    fn new(configured: &AutomationConfig, request: &DashboardAutomationRunRequestV1) -> Self {
        let timeout_cap = match request {
            DashboardAutomationRunRequestV1::MemoryCurator { .. } => {
                MEMORY_CURATOR_REQUEST_TIMEOUT_SECS
            }
            DashboardAutomationRunRequestV1::SessionReflection { .. } => {
                SESSION_REFLECTOR_REQUEST_TIMEOUT_SECS
            }
            DashboardAutomationRunRequestV1::SkillWriting { .. } => {
                SKILL_WRITER_REQUEST_TIMEOUT_SECS
            }
        };
        let mut config = configured.clone();
        config.timeout_secs = config.timeout_secs.min(timeout_cap);
        let backend = CodexAppServerBackend::from_automation_config(&config);
        Self { config, backend }
    }

    fn execution(&self) -> (&AutomationConfig, &CodexAppServerBackend) {
        (&self.config, &self.backend)
    }
}

pub(crate) fn dashboard_automation_observation_port(
    invocation_service: crate::daemon::service::invocation::DaemonInvocationService,
) -> tracedecay_dashboard_api::DashboardAutomationObservationPortV1 {
    Arc::new(move |project_root| {
        let invocation_service = invocation_service.clone();
        Box::pin(async move {
            let producer = crate::daemon::project_automation_observation_producer(
                &invocation_service,
                &project_root,
            )
            .await
            .ok_or_else(|| {
                "dashboard automation observation authority is unavailable".to_owned()
            })?;
            Ok(Arc::new(move |record| {
                crate::daemon::record_project_automation_run(
                    producer.as_ref(),
                    &project_root,
                    &record,
                    "dashboard_user_job",
                );
            }) as DashboardAutomationObservationRecorderV1)
        })
    })
}

/// Builds the single exact-profile authority used by production dashboard
/// states and their host-admission integration journeys.
pub(crate) fn compose_dashboard_automation_authority(
    profile_root: PathBuf,
    daemon_user_profile_id: UserProfileId,
    retained_project_server_resolver: RetainedProjectServerResolver,
    writer: DashboardAutomationWriter,
    invocation_service: crate::daemon::service::invocation::DaemonInvocationService,
) -> Result<DashboardAutomationAuthorityV1> {
    let project_resolver = dashboard_automation_project_resolver(
        daemon_user_profile_id,
        retained_project_server_resolver,
    );
    compose_dashboard_automation_authority_with_resolver(
        profile_root,
        project_resolver,
        writer,
        invocation_service,
    )
}

fn compose_dashboard_automation_authority_with_resolver(
    profile_root: PathBuf,
    project_resolver: DashboardAutomationProjectResolver,
    writer: DashboardAutomationWriter,
    invocation_service: crate::daemon::service::invocation::DaemonInvocationService,
) -> Result<DashboardAutomationAuthorityV1> {
    let run_port = dashboard_automation_run_port(
        profile_root.clone(),
        Arc::clone(&project_resolver),
        invocation_service,
    );
    let skill_port =
        dashboard_managed_skill_command_port(profile_root.clone(), project_resolver, writer);
    DashboardAutomationAuthorityV1::new(profile_root, run_port, skill_port).map_err(|error| {
        TraceDecayError::Config {
            message: error.detail().to_owned(),
        }
    })
}

#[cfg(feature = "test-transport")]
pub(crate) fn compose_dashboard_automation_authority_for_test(
    profile_root: PathBuf,
    retained: Arc<TraceDecay>,
    writer: DashboardAutomationWriter,
    invocation_service: crate::daemon::service::invocation::DaemonInvocationService,
) -> Result<DashboardAutomationAuthorityV1> {
    let retained_root = retained.project_root().to_path_buf();
    let project_resolver: DashboardAutomationProjectResolver =
        Arc::new(move |requested_project_root| {
            let retained = Arc::clone(&retained);
            let retained_root = retained_root.clone();
            Box::pin(async move {
                let requested = super::authority::canonical_identity_path(&requested_project_root)
                    .map_err(|error| DashboardAutomationAuthorityErrorV1::Unavailable {
                        detail: format!("dashboard automation project is unavailable: {error}"),
                    })?;
                let retained_identity = super::authority::canonical_identity_path(&retained_root)
                    .map_err(|error| {
                    DashboardAutomationAuthorityErrorV1::Unavailable {
                        detail: format!(
                            "retained dashboard automation project is unavailable: {error}"
                        ),
                    }
                })?;
                if requested != retained_identity {
                    return Err(DashboardAutomationAuthorityErrorV1::Denied {
                        detail: "dashboard automation project authority resolved a different root"
                            .to_owned(),
                    });
                }
                validate_dashboard_automation_project(retained, &requested_project_root)
            })
        });
    compose_dashboard_automation_authority_with_resolver(
        profile_root,
        project_resolver,
        writer,
        invocation_service,
    )
}

fn dashboard_automation_run_port(
    profile_root: PathBuf,
    project_resolver: DashboardAutomationProjectResolver,
    invocation_service: crate::daemon::service::invocation::DaemonInvocationService,
) -> DashboardAutomationRunPortV1 {
    Arc::new(move |invocation| {
        let profile_root = profile_root.clone();
        let project_resolver = Arc::clone(&project_resolver);
        let invocation_service = invocation_service.clone();
        Box::pin(async move {
            let cg = project_resolver(invocation.project_root.clone()).await?;
            let run_control = dashboard_automation_run_control(&invocation.control);
            // The canonical runner owns task locking, cooldowns, run-ledger
            // publication, and curation CAS. Holding the daemon's broad store
            // writer across a model turn would serialize unrelated projects.
            execute_dashboard_automation_run(
                cg.as_ref(),
                profile_root,
                invocation.request,
                invocation.control,
                &run_control,
                &invocation_service,
            )
            .await
        })
    })
}

fn dashboard_automation_run_control(
    control: &DashboardHttpRequestControlV1,
) -> AutomationRunControl {
    let cancellation = control.cancellation().clone();
    let deadline = control.deadline();
    AutomationRunControl::from_interrupted(Arc::new(move || {
        cancellation.is_cancelled() || deadline.is_elapsed_at(now_micros())
    }))
}

fn dashboard_managed_skill_command_port(
    profile_root: PathBuf,
    project_resolver: DashboardAutomationProjectResolver,
    writer: DashboardAutomationWriter,
) -> DashboardManagedSkillCommandPortV1 {
    Arc::new(move |invocation| {
        let profile_root = profile_root.clone();
        let project_resolver = Arc::clone(&project_resolver);
        let writer = Arc::clone(&writer);
        Box::pin(async move {
            execute_serialized_dashboard_automation(&writer, move || async move {
                let cg = project_resolver(invocation.project_root.clone()).await?;
                execute_dashboard_managed_skill_command(
                    &profile_root,
                    cg.project_root(),
                    invocation.command,
                )
                .await
            })
            .await
        })
    })
}

fn dashboard_automation_project_resolver(
    daemon_user_profile_id: UserProfileId,
    retained_project_server_resolver: RetainedProjectServerResolver,
) -> DashboardAutomationProjectResolver {
    Arc::new(move |requested_project_root| {
        let daemon_user_profile_id = daemon_user_profile_id.clone();
        let retained_project_server_resolver = Arc::clone(&retained_project_server_resolver);
        Box::pin(async move {
            let retained_server = retained_project_server_resolver(
                RetainedProjectGraphRequest::for_mounted_root(requested_project_root.clone()),
            )
            .await
            .map_err(|error| DashboardAutomationAuthorityErrorV1::Unavailable {
                detail: format!("dashboard automation project authority is unavailable: {error}"),
            })?
            .ok_or_else(|| DashboardAutomationAuthorityErrorV1::Unavailable {
                detail: format!(
                    "dashboard automation project '{}' is not retained by the daemon",
                    requested_project_root.display()
                ),
            })?;
            if retained_server
                .profile_identity()
                .is_none_or(|identity| identity.profile_id() != &daemon_user_profile_id)
            {
                return Err(DashboardAutomationAuthorityErrorV1::Denied {
                    detail: "dashboard automation project belongs to another profile".to_owned(),
                });
            }
            let retained = retained_server.cg_snapshot().await;
            validate_dashboard_automation_project(retained, &requested_project_root)
        })
    })
}

fn validate_dashboard_automation_project(
    retained: Arc<TraceDecay>,
    requested_project_root: &Path,
) -> DashboardAutomationResult<Arc<TraceDecay>> {
    let requested = requested_project_root.canonicalize().map_err(|error| {
        DashboardAutomationAuthorityErrorV1::Unavailable {
            detail: format!(
                "dashboard automation project '{}' cannot be resolved: {error}",
                requested_project_root.display()
            ),
        }
    })?;
    let retained_root = retained.project_root().canonicalize().map_err(|error| {
        DashboardAutomationAuthorityErrorV1::Unavailable {
            detail: format!(
                "retained dashboard automation project '{}' cannot be resolved: {error}",
                retained.project_root().display()
            ),
        }
    })?;
    if requested != retained_root {
        return Err(DashboardAutomationAuthorityErrorV1::Denied {
            detail: "dashboard automation project authority resolved a different root".to_owned(),
        });
    }
    Ok(retained)
}

async fn execute_serialized_dashboard_automation<T, Operation, OperationFuture>(
    writer: &DashboardAutomationWriter,
    operation: Operation,
) -> DashboardAutomationResult<T>
where
    T: Send + 'static,
    Operation: FnOnce() -> OperationFuture + Send + 'static,
    OperationFuture: Future<Output = DashboardAutomationResult<T>> + Send + 'static,
{
    let outcome = Arc::new(tokio::sync::Mutex::new(None));
    let written_outcome = Arc::clone(&outcome);
    let writer_result = writer(Box::new(move || {
        Box::pin(async move {
            let result = operation().await;
            let serialized_result = match result.as_ref() {
                Ok(_) => Ok(Value::Null),
                Err(error) => Err(error.detail().to_owned()),
            };
            *written_outcome.lock().await = Some(result);
            serialized_result
        })
    }))
    .await;
    let operation_result = outcome.lock().await.take();
    match operation_result {
        Some(result) => result,
        None => Err(DashboardAutomationAuthorityErrorV1::Failed {
            detail: match writer_result {
                Ok(_) => {
                    "dashboard automation writer returned without an operation outcome".to_owned()
                }
                Err(detail) => detail,
            },
        }),
    }
}

async fn execute_dashboard_automation_run(
    cg: &TraceDecay,
    profile_root: PathBuf,
    request: DashboardAutomationRunRequestV1,
    request_control: DashboardHttpRequestControlV1,
    run_control: &AutomationRunControl,
    invocation_service: &crate::daemon::service::invocation::DaemonInvocationService,
) -> DashboardAutomationResult<DashboardAutomationRunOutcomeV1> {
    let producer = crate::daemon::project_automation_observation_producer(
        invocation_service,
        cg.project_root(),
    )
    .await
    .ok_or_else(|| DashboardAutomationAuthorityErrorV1::Unavailable {
        detail: "dashboard automation observation authority is unavailable".to_owned(),
    })?;
    let pinned = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| DashboardAutomationAuthorityErrorV1::Unavailable {
            detail: format!("automation configuration authority is unavailable: {error}"),
        })?;
    let config = from_configuration_snapshot(&pinned.snapshot).map_err(automation_failed)?;
    let configuration_digest =
        crate::daemon::automation_effect::pinned_automation_configuration_digest(
            &pinned.revision_id,
            &pinned.snapshot.effective_behavior_digest,
            &pinned.snapshot.resolution_provenance_digest,
        )
        .map_err(automation_failed)?;
    let runtime = DashboardAutomationRequestRuntime::new(&config, &request);
    let (config, backend) = runtime.execution();
    let run = match request {
        DashboardAutomationRunRequestV1::MemoryCurator {
            fact_review_limit,
            min_confidence,
        } => {
            let run_id = request_control.request_id().as_str().to_owned();
            let options = MemoryCuratorAutomationOptions {
                trigger: AutomationTrigger::Dashboard,
                run_id: Some(run_id.clone()),
                fact_review_limit,
                min_confidence,
            };
            let admission = crate::daemon::automation_effect::AutomationEffectAuthority::prepare(
                invocation_service,
                cg,
                cg.project_root(),
                &cg.store_layout().dashboard_root,
                request_control.request_id(),
                request_control.deadline(),
                request_control.cancellation(),
                request_control.observed_at(),
                configuration_digest.clone(),
                crate::daemon::automation_effect::memory_curator_run_request(
                    &run_id,
                    fact_review_limit,
                    min_confidence,
                )
                .map_err(automation_failed)?,
            )
            .await
            .map_err(automation_failed)?;
            let effect = match admission {
                crate::daemon::automation_effect::AutomationEffectAdmission::Execute(effect) => {
                    effect
                }
                crate::daemon::automation_effect::AutomationEffectAdmission::PreAdmissionProblem(
                    envelope,
                ) => {
                    return Err(DashboardAutomationAuthorityErrorV1::ApplicationProblem(envelope));
                }
                crate::daemon::automation_effect::AutomationEffectAdmission::Replay(terminal) => {
                    let run = automation_terminal_run(&terminal)?;
                    return Ok(DashboardAutomationRunOutcomeV1::Memory(run));
                }
            };
            let run = match run_memory_curator_with_backend(
                cg,
                config,
                &pinned.revision_id,
                backend,
                options,
                run_control,
            )
            .await
            {
                Ok(run) => run,
                Err(error) => return Err(automation_run_failed(error, effect).await),
            };
            let terminal = effect
                .settle_run(&run.ledger_record, run.committed_receipt.as_ref())
                .await
                .map_err(automation_failed)?;
            let settled_run = automation_terminal_run(&terminal)?;
            crate::daemon::record_project_automation_run(
                producer.as_ref(),
                cg.project_root(),
                &run.ledger_record,
                "dashboard",
            );
            DashboardAutomationRunOutcomeV1::Memory(settled_run)
        }
        DashboardAutomationRunRequestV1::SessionReflection {
            provider,
            query,
            evidence_limit,
            scope,
            session_id,
            include_summaries,
            sort,
            source,
            role,
            start_time,
            end_time,
        } => {
            let mut options = SessionReflectorAutomationOptions {
                trigger: AutomationTrigger::Dashboard,
                run_id: None,
                session_id,
                source,
                role,
                start_time,
                end_time,
                ..SessionReflectorAutomationOptions::default()
            };
            if let Some(provider) = provider {
                options.provider = provider;
            }
            if let Some(query) = query {
                options.query = query;
            }
            if let Some(evidence_limit) = evidence_limit {
                options.evidence_limit = evidence_limit;
            }
            if let Some(scope) = scope {
                options.scope = scope;
            }
            if let Some(include_summaries) = include_summaries {
                options.include_summaries = include_summaries;
            }
            if let Some(sort) = sort {
                options.sort = sort;
            }
            let run_id = request_control.request_id().as_str().to_owned();
            options.run_id = Some(run_id.clone());
            let admission = crate::daemon::automation_effect::AutomationEffectAuthority::prepare(
                invocation_service,
                cg,
                cg.project_root(),
                &cg.store_layout().dashboard_root,
                request_control.request_id(),
                request_control.deadline(),
                request_control.cancellation(),
                request_control.observed_at(),
                configuration_digest,
                crate::daemon::automation_effect::session_reflector_run_request(&run_id, &options)
                    .map_err(automation_failed)?,
            )
            .await
            .map_err(automation_failed)?;
            let effect = match admission {
                crate::daemon::automation_effect::AutomationEffectAdmission::Execute(effect) => {
                    effect
                }
                crate::daemon::automation_effect::AutomationEffectAdmission::PreAdmissionProblem(
                    envelope,
                ) => {
                    return Err(DashboardAutomationAuthorityErrorV1::ApplicationProblem(envelope));
                }
                crate::daemon::automation_effect::AutomationEffectAdmission::Replay(terminal) => {
                    let run = automation_terminal_run(&terminal)?;
                    return Ok(DashboardAutomationRunOutcomeV1::Memory(run));
                }
            };
            let run = match run_session_reflector_with_backend(
                cg,
                config,
                run_control,
                &pinned.revision_id,
                backend,
                options,
            )
            .await
            {
                Ok(run) => run,
                Err(error) => return Err(automation_run_failed(error, effect).await),
            };
            let terminal = effect
                .settle_run(&run.ledger_record, run.committed_receipt.as_ref())
                .await
                .map_err(automation_failed)?;
            let settled_run = automation_terminal_run(&terminal)?;
            crate::daemon::record_project_automation_run(
                producer.as_ref(),
                cg.project_root(),
                &run.ledger_record,
                "dashboard",
            );
            DashboardAutomationRunOutcomeV1::Memory(settled_run)
        }
        DashboardAutomationRunRequestV1::SkillWriting {
            provider,
            query,
            evidence_limit,
        } => {
            let mut options = SkillWriterAutomationOptions {
                trigger: AutomationTrigger::Dashboard,
                run_id: None,
                profile_root: Some(profile_root),
                ..SkillWriterAutomationOptions::default()
            };
            if let Some(provider) = provider {
                options.provider = provider;
            }
            if let Some(query) = query {
                options.query = query;
            }
            if let Some(evidence_limit) = evidence_limit {
                options.evidence_limit = evidence_limit;
            }
            let run =
                run_skill_writer_with_backend(cg, config, &pinned.revision_id, backend, options)
                    .await
                    .map_err(automation_failed)?;
            crate::daemon::record_project_automation_run(
                producer.as_ref(),
                cg.project_root(),
                &run.ledger_record,
                "dashboard",
            );
            DashboardAutomationRunOutcomeV1::SkillWriting(
                serde_json::to_value(run).map_err(automation_failed)?,
            )
        }
    };
    Ok(run)
}

async fn execute_dashboard_managed_skill_command(
    profile_root: &Path,
    project_root: &Path,
    command: DashboardManagedSkillCommandV1,
) -> DashboardAutomationResult<DashboardManagedSkillCommandOutcomeV1> {
    let skill = match command {
        DashboardManagedSkillCommandV1::Create { draft, pinned } => {
            let mut skill = draft.materialize().map_err(automation_invalid)?;
            if let Some(pinned) = pinned {
                skill.set_pinned(pinned);
            }
            match save_managed_skill(profile_root, &skill).await {
                Ok(()) => skill,
                Err(error)
                    if managed_skill_dir(profile_root, &skill.metadata.id)
                        .is_ok_and(|path| path.join("skill.json").is_file()) =>
                {
                    return Err(DashboardAutomationAuthorityErrorV1::Conflict {
                        detail: error.to_string(),
                    });
                }
                Err(error) => return Err(automation_failed(error)),
            }
        }
        DashboardManagedSkillCommandV1::Update {
            id,
            base_checksum,
            update,
        } => {
            let current = load_exact_managed_skill(profile_root, &id).await?;
            if current.metadata.checksum != base_checksum {
                return Err(DashboardAutomationAuthorityErrorV1::Conflict {
                    detail: format!("base checksum for managed skill '{id}' is stale"),
                });
            }
            preview_managed_skill_update(&current, &update).map_err(automation_invalid)?;
            match apply_managed_skill_update(profile_root, &id, &base_checksum, update).await {
                Ok(skill) => skill,
                Err(error) => {
                    return match load_exact_managed_skill(profile_root, &id).await {
                        Ok(skill) if skill.metadata.checksum != base_checksum => {
                            Err(DashboardAutomationAuthorityErrorV1::Conflict {
                                detail: format!("base checksum for managed skill '{id}' is stale"),
                            })
                        }
                        Err(error @ DashboardAutomationAuthorityErrorV1::NotFound { .. }) => {
                            Err(error)
                        }
                        _ => Err(automation_failed(error)),
                    };
                }
            }
        }
        DashboardManagedSkillCommandV1::Disable { id } => {
            load_exact_managed_skill(profile_root, &id).await?;
            disable_managed_skill(profile_root, &id)
                .await
                .map_err(|error| managed_skill_lifecycle_error(profile_root, &id, error))?
        }
        DashboardManagedSkillCommandV1::Archive { id } => {
            load_exact_managed_skill(profile_root, &id).await?;
            archive_managed_skill(profile_root, &id)
                .await
                .map_err(|error| managed_skill_lifecycle_error(profile_root, &id, error))?
        }
        DashboardManagedSkillCommandV1::Restore { id } => {
            load_exact_managed_skill(profile_root, &id).await?;
            restore_managed_skill(profile_root, &id)
                .await
                .map_err(|error| managed_skill_lifecycle_error(profile_root, &id, error))?
        }
    };
    let deployment = deploy_managed_skills_to_project(profile_root, project_root);
    Ok(DashboardManagedSkillCommandOutcomeV1 { skill, deployment })
}

async fn load_exact_managed_skill(
    profile_root: &Path,
    id: &str,
) -> DashboardAutomationResult<ManagedSkill> {
    validate_skill_id(id).map_err(automation_invalid)?;
    let record_path = managed_skill_dir(profile_root, id)
        .map_err(automation_invalid)?
        .join("skill.json");
    if !record_path.is_file() {
        return Err(DashboardAutomationAuthorityErrorV1::NotFound {
            detail: format!("managed skill '{id}' was not found"),
        });
    }
    match load_managed_skill(profile_root, id).await {
        Ok(skill) => Ok(skill),
        Err(_) if !record_path.is_file() => Err(DashboardAutomationAuthorityErrorV1::NotFound {
            detail: format!("managed skill '{id}' was not found"),
        }),
        Err(error) => Err(automation_failed(error)),
    }
}

fn managed_skill_lifecycle_error(
    profile_root: &Path,
    id: &str,
    error: impl std::fmt::Display,
) -> DashboardAutomationAuthorityErrorV1 {
    if managed_skill_dir(profile_root, id).is_ok_and(|path| !path.join("skill.json").is_file()) {
        DashboardAutomationAuthorityErrorV1::NotFound {
            detail: format!("managed skill '{id}' was not found"),
        }
    } else {
        automation_failed(error)
    }
}

fn automation_invalid(error: impl std::fmt::Display) -> DashboardAutomationAuthorityErrorV1 {
    DashboardAutomationAuthorityErrorV1::Invalid {
        detail: error.to_string(),
    }
}

fn automation_failed(error: impl std::fmt::Display) -> DashboardAutomationAuthorityErrorV1 {
    DashboardAutomationAuthorityErrorV1::Failed {
        detail: error.to_string(),
    }
}

async fn automation_run_failed(
    error: AutomationRunError,
    effect: crate::daemon::automation_effect::AutomationEffectAuthority,
) -> DashboardAutomationAuthorityErrorV1 {
    match effect.settle_problem(&error).await {
        Ok(Some(problem)) => memory_automation_problem(problem),
        Ok(None) => automation_failed(error),
        Err(settlement_error) => automation_failed(settlement_error),
    }
}

fn automation_terminal_run(
    terminal: &crate::daemon::automation_effect::AutomationSettledTerminal,
) -> DashboardAutomationResult<tracedecay_application::retained_surfaces::MemoryAutomationRunResultV1>
{
    if let Some(run) = terminal.run_result() {
        return Ok(run.clone());
    }
    if let Some(problem) = terminal.problem() {
        return Err(memory_automation_problem(problem.clone()));
    }
    Err(automation_failed(
        "memory automation terminal has neither a run nor a problem",
    ))
}

fn memory_automation_problem(
    problem: crate::daemon::automation_effect::AutomationSettledProblem,
) -> DashboardAutomationAuthorityErrorV1 {
    DashboardAutomationAuthorityErrorV1::MemoryAutomationProblem(problem)
}

#[cfg(test)]
mod tests {
    use super::{DashboardAutomationRequestRuntime, DashboardAutomationRunRequestV1};
    use tracedecay_agent_hosts::automation::config::AutomationConfig;

    fn session_reflection_request() -> DashboardAutomationRunRequestV1 {
        DashboardAutomationRunRequestV1::SessionReflection {
            provider: None,
            query: None,
            evidence_limit: None,
            scope: None,
            session_id: None,
            include_summaries: None,
            sort: None,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        }
    }

    #[test]
    fn dashboard_memory_curator_caps_each_backend_call_for_the_wall_budget() {
        let configured = AutomationConfig {
            timeout_secs: 300,
            ..AutomationConfig::default()
        };
        let request = DashboardAutomationRunRequestV1::MemoryCurator {
            fact_review_limit: 25,
            min_confidence: 0.85,
        };

        let runtime = DashboardAutomationRequestRuntime::new(&configured, &request);
        let (effective, _backend) = runtime.execution();

        assert_eq!(effective.timeout_secs, 80);
        assert_eq!(configured.timeout_secs, 300);
        assert_eq!(effective.model_id, configured.model_id);
        assert_eq!(effective.tasks, configured.tasks);
    }

    #[test]
    fn dashboard_session_reflector_caps_initial_and_repair_calls_for_the_wall_budget() {
        let configured = AutomationConfig {
            timeout_secs: 300,
            ..AutomationConfig::default()
        };

        let runtime =
            DashboardAutomationRequestRuntime::new(&configured, &session_reflection_request());

        assert_eq!(runtime.execution().0.timeout_secs, 120);
    }

    #[test]
    fn dashboard_skill_writer_caps_initial_and_repair_calls_for_the_wall_budget() {
        let configured = AutomationConfig {
            timeout_secs: 300,
            ..AutomationConfig::default()
        };
        let skill_writing = DashboardAutomationRunRequestV1::SkillWriting {
            provider: None,
            query: None,
            evidence_limit: None,
        };

        let runtime = DashboardAutomationRequestRuntime::new(&configured, &skill_writing);

        assert_eq!(runtime.execution().0.timeout_secs, 120);
    }

    #[test]
    fn dashboard_request_timeout_never_increases_a_stricter_configuration() {
        let configured = AutomationConfig {
            timeout_secs: 45,
            ..AutomationConfig::default()
        };

        let runtime =
            DashboardAutomationRequestRuntime::new(&configured, &session_reflection_request());

        assert_eq!(runtime.execution().0.timeout_secs, 45);
    }
}

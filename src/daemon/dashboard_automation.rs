//! Daemon-retained automation execution for dashboard project states.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay_agent_hosts::automation::backend::CodexAppServerBackend;
use tracedecay_agent_hosts::automation::config::from_configuration_snapshot;
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
use tracedecay_automation::managed_skills::validate_skill_id;
use tracedecay_dashboard_api::{
    DashboardAutomationAuthorityErrorV1, DashboardAutomationAuthorityV1,
    DashboardAutomationRunInvocationV1, DashboardAutomationRunPortV1,
    DashboardAutomationRunRequestV1, DashboardAutomationWriter,
    DashboardManagedSkillCommandInvocationV1, DashboardManagedSkillCommandOutcomeV1,
    DashboardManagedSkillCommandPortV1, DashboardManagedSkillCommandV1,
};
use tracedecay_domain::configuration::UserProfileId;

use crate::errors::{Result, TraceDecayError};
use crate::mcp::server::{RetainedProjectGraphRequest, RetainedProjectGraphResolver};
use crate::tracedecay::TraceDecay;

type DashboardAutomationResult<T> = std::result::Result<T, DashboardAutomationAuthorityErrorV1>;

/// Builds the single exact-profile authority used by production dashboard
/// states and their host-admission integration journeys.
pub(crate) fn compose_dashboard_automation_authority(
    profile_root: PathBuf,
    daemon_user_profile_id: UserProfileId,
    project_graph_resolver: RetainedProjectGraphResolver,
    writer: DashboardAutomationWriter,
) -> Result<DashboardAutomationAuthorityV1> {
    let run_port = dashboard_automation_run_port(
        profile_root.clone(),
        daemon_user_profile_id.clone(),
        Arc::clone(&project_graph_resolver),
    );
    let skill_port = dashboard_managed_skill_command_port(
        profile_root.clone(),
        daemon_user_profile_id,
        project_graph_resolver,
        writer,
    );
    DashboardAutomationAuthorityV1::new(profile_root, run_port, skill_port).map_err(|error| {
        TraceDecayError::Config {
            message: error.detail().to_owned(),
        }
    })
}

fn dashboard_automation_run_port(
    profile_root: PathBuf,
    daemon_user_profile_id: UserProfileId,
    project_graph_resolver: RetainedProjectGraphResolver,
) -> DashboardAutomationRunPortV1 {
    Arc::new(move |invocation| {
        let profile_root = profile_root.clone();
        let daemon_user_profile_id = daemon_user_profile_id.clone();
        let project_graph_resolver = Arc::clone(&project_graph_resolver);
        Box::pin(async move {
            let cg = resolve_dashboard_automation_project(
                project_graph_resolver,
                &daemon_user_profile_id,
                &invocation.project_root,
            )
            .await?;
            // The canonical runner owns task locking, cooldowns, run-ledger
            // publication, and curation CAS. Holding the daemon's broad store
            // writer across a model turn would serialize unrelated projects.
            execute_dashboard_automation_run(cg.as_ref(), profile_root, invocation.request).await
        })
    })
}

fn dashboard_managed_skill_command_port(
    profile_root: PathBuf,
    daemon_user_profile_id: UserProfileId,
    project_graph_resolver: RetainedProjectGraphResolver,
    writer: DashboardAutomationWriter,
) -> DashboardManagedSkillCommandPortV1 {
    Arc::new(move |invocation| {
        let profile_root = profile_root.clone();
        let daemon_user_profile_id = daemon_user_profile_id.clone();
        let project_graph_resolver = Arc::clone(&project_graph_resolver);
        let writer = Arc::clone(&writer);
        Box::pin(async move {
            execute_serialized_dashboard_automation(&writer, move || async move {
                let cg = resolve_dashboard_automation_project(
                    project_graph_resolver,
                    &daemon_user_profile_id,
                    &invocation.project_root,
                )
                .await?;
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

async fn resolve_dashboard_automation_project(
    resolver: RetainedProjectGraphResolver,
    daemon_user_profile_id: &UserProfileId,
    requested_project_root: &Path,
) -> DashboardAutomationResult<Arc<TraceDecay>> {
    let retained = resolver(RetainedProjectGraphRequest::for_mounted_root(
        requested_project_root.to_path_buf(),
    ))
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
    if retained.store_runtime_registry().profile_id() != daemon_user_profile_id {
        return Err(DashboardAutomationAuthorityErrorV1::Denied {
            detail: "dashboard automation project belongs to another profile".to_owned(),
        });
    }
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
) -> DashboardAutomationResult<Value> {
    let pinned = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .map_err(|error| DashboardAutomationAuthorityErrorV1::Unavailable {
            detail: format!("automation configuration authority is unavailable: {error}"),
        })?;
    let config = from_configuration_snapshot(&pinned.snapshot).map_err(automation_failed)?;
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let run = match request {
        DashboardAutomationRunRequestV1::MemoryCurator {
            max_clusters,
            min_confidence,
        } => serde_json::to_value(
            run_memory_curator_with_backend(
                cg,
                &config,
                &pinned.revision_id,
                &backend,
                MemoryCuratorAutomationOptions {
                    trigger: AutomationTrigger::Dashboard,
                    run_id: None,
                    max_clusters,
                    min_confidence,
                },
            )
            .await
            .map_err(automation_failed)?,
        )
        .map_err(automation_failed)?,
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
        } => serde_json::to_value(
            run_session_reflector_with_backend(
                cg,
                &config,
                &pinned.revision_id,
                &backend,
                SessionReflectorAutomationOptions {
                    trigger: AutomationTrigger::Dashboard,
                    run_id: None,
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
                    ..SessionReflectorAutomationOptions::default()
                },
            )
            .await
            .map_err(automation_failed)?,
        )
        .map_err(automation_failed)?,
        DashboardAutomationRunRequestV1::SkillWriting {
            provider,
            query,
            evidence_limit,
        } => serde_json::to_value(
            run_skill_writer_with_backend(
                cg,
                &config,
                &pinned.revision_id,
                &backend,
                SkillWriterAutomationOptions {
                    trigger: AutomationTrigger::Dashboard,
                    run_id: None,
                    provider,
                    query,
                    evidence_limit,
                    profile_root: Some(profile_root),
                    ..SkillWriterAutomationOptions::default()
                },
            )
            .await
            .map_err(automation_failed)?,
        )
        .map_err(automation_failed)?,
    };
    Ok(json!({ "run": run }))
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

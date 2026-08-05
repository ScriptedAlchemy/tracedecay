//! Unadvertised daemon-owned operations used by one-shot CLI commands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_application::session_sync::{
    SessionGitSyncV1, SessionSyncCommandV1, SessionSyncControlV1, SessionSyncOutcomeV1,
    SessionSyncRequestV1, SessionSyncScopeV1, SessionSyncServicePort, SessionTranscriptImportV1,
};
use tracedecay_application::{CancellationSignal, Deadline, IdempotencyKey, RequestId, now_micros};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::json_result;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum AdminCliAction {
    CostSummary {
        range: String,
    },
    SessionsImport,
    SessionsGitSync {
        since: i64,
        limit_sessions: usize,
        dry_run: bool,
    },
    SessionsSyncStatus {
        idempotency_key: String,
    },
    SessionsSyncCancel {
        idempotency_key: String,
    },
    SessionsUnfinished {
        limit: usize,
    },
    AnalyticsSync,
    AnalyticsDiagnostics {
        all: bool,
        no_sync: bool,
    },
    RegistryUpdate {
        tokens: u64,
    },
    RegistryList {
        limit: usize,
        query: Option<String>,
    },
    RegistryContext {
        project_arg: Option<PathBuf>,
    },
    RegistryEmpty,
    RegistryProjectTokens {
        project_args: Vec<PathBuf>,
    },
    RegistryGc {
        prefix: Option<String>,
        apply: bool,
    },
    MigrationInventory {
        roots: Vec<PathBuf>,
        follow_symlinks: bool,
        include_all_registered: bool,
        #[serde(default)]
        verify_integrity: bool,
    },
    StorageReport {
        project_id: Option<String>,
        project_root: Option<PathBuf>,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default = "default_storage_report_page_limit")]
        limit: usize,
    },
    GainQuery {
        project_arg: Option<PathBuf>,
        since: i64,
        history: bool,
    },
}

const fn default_storage_report_page_limit() -> usize {
    8
}

struct AdminCliContext<'a> {
    global_db: &'a Arc<RegisteredGlobalDb>,
    accounting_db: Option<&'a RegisteredGlobalDb>,
    profile_root: Option<&'a Path>,
    project: Option<&'a TraceDecay>,
    registered_project_session_db: Option<&'a Arc<RegisteredGlobalDb>>,
    registered_user_session_db: Option<&'a Arc<RegisteredGlobalDb>>,
    profile_identity: Option<&'a crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    session_sync: Option<&'a dyn SessionSyncServicePort>,
    request_id: Option<RequestId>,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
}

impl<'a> AdminCliContext<'a> {
    fn with_project(
        cg: &'a TraceDecay,
        global_db: &'a Arc<RegisteredGlobalDb>,
        accounting_db: Option<&'a RegisteredGlobalDb>,
        profile_root: Option<&'a Path>,
        session_authorities: super::SessionAuthorities<'a>,
        session_sync: Option<&'a dyn SessionSyncServicePort>,
        request_id: Option<RequestId>,
        deadline: Option<Deadline>,
        cancellation: Option<CancellationSignal>,
    ) -> Self {
        Self {
            global_db,
            accounting_db,
            profile_root,
            project: Some(cg),
            registered_project_session_db: session_authorities.project_registered,
            registered_user_session_db: session_authorities.profile_registered,
            profile_identity: session_authorities.profile_identity,
            session_sync,
            request_id,
            deadline,
            cancellation,
        }
    }

    fn projectless(
        global_db: &'a Arc<RegisteredGlobalDb>,
        accounting_db: Option<&'a RegisteredGlobalDb>,
        profile_root: &'a Path,
    ) -> Self {
        Self {
            global_db,
            accounting_db,
            profile_root: Some(profile_root),
            project: None,
            registered_project_session_db: None,
            registered_user_session_db: None,
            profile_identity: None,
            session_sync: None,
            request_id: None,
            deadline: None,
            cancellation: None,
        }
    }

    fn require_project(&self) -> Result<&'a TraceDecay> {
        self.project.ok_or_else(|| TraceDecayError::Config {
            message: "requested admin action requires an initialized project".to_string(),
        })
    }

    fn require_accounting_db(&self) -> Result<&'a RegisteredGlobalDb> {
        self.accounting_db.ok_or_else(|| TraceDecayError::Config {
            message: "daemon registered accounting database is unavailable".to_string(),
        })
    }

    fn require_profile_root(&self) -> Result<&'a Path> {
        self.profile_root.ok_or_else(|| TraceDecayError::Config {
            message: "daemon TraceDecay profile root is unavailable".to_string(),
        })
    }

    fn project_root(&self) -> Option<&'a Path> {
        self.project.map(TraceDecay::project_root)
    }

    fn require_registered_project_session_db(&self) -> Result<&'a Arc<RegisteredGlobalDb>> {
        self.registered_project_session_db
            .ok_or_else(|| TraceDecayError::Config {
                message: "daemon registered project session database is unavailable".to_string(),
            })
    }

    fn require_profile_identity(
        &self,
    ) -> Result<&'a crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1> {
        self.profile_identity
            .ok_or_else(|| TraceDecayError::Config {
                message: "daemon durable profile identity is unavailable".to_string(),
            })
    }
}

pub(super) async fn handle_admin_cli(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&Arc<RegisteredGlobalDb>>,
    accounting_db: Option<&RegisteredGlobalDb>,
    profile_root: Option<&Path>,
    session_authorities: super::SessionAuthorities<'_>,
    session_sync: Option<&dyn SessionSyncServicePort>,
    request_id: Option<RequestId>,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<ToolResult> {
    let action = parse_admin_cli_action(args)?;
    let global_db = global_db.ok_or_else(|| TraceDecayError::Config {
        message: "daemon global database is unavailable".to_string(),
    })?;
    dispatch_admin_cli(
        AdminCliContext::with_project(
            cg,
            global_db,
            accounting_db,
            profile_root,
            session_authorities,
            session_sync,
            request_id,
            deadline,
            cancellation,
        ),
        action,
    )
    .await
}

pub(crate) async fn handle_projectless_admin_cli(
    args: Value,
    global_db: &Arc<RegisteredGlobalDb>,
    accounting_db: Option<&RegisteredGlobalDb>,
    profile_root: &Path,
) -> Result<ToolResult> {
    let action = parse_admin_cli_action(args)?;
    dispatch_admin_cli(
        AdminCliContext::projectless(global_db, accounting_db, profile_root),
        action,
    )
    .await
}

fn parse_admin_cli_action(args: Value) -> Result<AdminCliAction> {
    serde_json::from_value(args).map_err(|error| TraceDecayError::Config {
        message: format!("invalid tracedecay_admin_cli arguments: {error}"),
    })
}

async fn dispatch_admin_cli(
    context: AdminCliContext<'_>,
    action: AdminCliAction,
) -> Result<ToolResult> {
    let global_db = context.global_db;
    let value = match action {
        AdminCliAction::CostSummary { range } => {
            cost_summary(context.require_accounting_db()?, &range).await?
        }
        AdminCliAction::SessionsImport => {
            execute_session_sync(
                &context,
                SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
            )
            .await?
        }
        AdminCliAction::SessionsGitSync {
            since,
            limit_sessions,
            dry_run,
        } => {
            let options =
                SessionGitSyncV1::new(since, limit_sessions, dry_run).map_err(|error| {
                    TraceDecayError::Config {
                        message: error.to_string(),
                    }
                })?;
            execute_session_sync(&context, SessionSyncCommandV1::SynchronizeGit(options)).await?
        }
        AdminCliAction::SessionsSyncStatus { idempotency_key } => {
            control_session_sync(&context, idempotency_key, false).await?
        }
        AdminCliAction::SessionsSyncCancel { idempotency_key } => {
            control_session_sync(&context, idempotency_key, true).await?
        }
        AdminCliAction::SessionsUnfinished { limit } => {
            sessions_unfinished(context.require_registered_project_session_db()?, limit).await?
        }
        AdminCliAction::AnalyticsSync => {
            crate::analytics_bridge::analytics_sync_with_db(
                context.require_accounting_db()?,
                context.project_root(),
            )
            .await
        }
        AdminCliAction::AnalyticsDiagnostics { all, no_sync } => {
            crate::analytics_bridge::analytics_diagnostics_with_db(
                context.require_accounting_db()?,
                context.registered_project_session_db.map(Arc::as_ref),
                context.registered_user_session_db.map(Arc::as_ref),
                context.project_root(),
                all,
                no_sync,
            )
            .await?
        }
        AdminCliAction::RegistryUpdate { tokens } => {
            let cg = context.require_project()?;
            let previous = global_db.get_project_tokens(cg.project_root()).await;
            global_db.upsert(cg.project_root(), tokens).await;
            json!({ "previous": previous, "current": tokens })
        }
        AdminCliAction::RegistryList { limit, query } => {
            registry_list(context.project, global_db, limit, query.as_deref()).await?
        }
        AdminCliAction::RegistryContext { project_arg } => {
            registry_context(context.project, global_db, project_arg.as_deref()).await?
        }
        AdminCliAction::RegistryEmpty => registry_empty(global_db).await?,
        AdminCliAction::RegistryProjectTokens { project_args } => {
            registry_project_tokens(global_db, &project_args).await
        }
        AdminCliAction::RegistryGc { prefix, apply } => {
            let profile_root = context.require_profile_root()?;
            let report = if apply {
                crate::migrate::registry::apply_registry_gc(global_db, profile_root, prefix).await?
            } else {
                crate::migrate::registry::registry_gc_report(global_db, profile_root, prefix)
                    .await?
            };
            serde_json::to_value(report)?
        }
        AdminCliAction::MigrationInventory {
            roots,
            follow_symlinks,
            include_all_registered,
            verify_integrity,
        } => serde_json::to_value(
            crate::migrate::inventory::build_inventory_for_daemon(
                crate::migrate::inventory::MigrationInventoryOptions {
                    roots,
                    global_db_path: None,
                    follow_symlinks,
                    include_all_registered,
                    integrity: if verify_integrity {
                        crate::migrate::inventory::InventoryIntegrityMode::Full
                    } else {
                        crate::migrate::inventory::InventoryIntegrityMode::MetadataOnly
                    },
                },
                global_db,
            )
            .await?,
        )?,
        AdminCliAction::StorageReport {
            project_id,
            project_root,
            cursor,
            limit,
        } => {
            let profile_root = context.require_profile_root()?;
            let report = match (project_id, project_root) {
                (Some(project_id), Some(project_root)) => {
                    if cursor.is_some() {
                        return Err(TraceDecayError::Config {
                            message: "project-scoped storage_report does not accept a cursor"
                                .to_owned(),
                        });
                    }
                    crate::retention::storage_report::build_project_storage_report_from_daemon(
                        profile_root,
                        &project_id,
                        &project_root,
                    )
                    .await?
                }
                (None, None) => {
                    crate::retention::storage_report::build_storage_report_page_from_registered_global_db(
                        profile_root,
                        global_db,
                        cursor.as_deref(),
                        limit,
                    )
                    .await?
                }
                _ => {
                    return Err(TraceDecayError::Config {
                        message:
                            "storage_report requires project_id and project_root together"
                                .to_string(),
                    });
                }
            };
            serde_json::to_value(report)?
        }
        AdminCliAction::GainQuery {
            project_arg,
            since,
            history,
        } => gain_query(global_db, project_arg.as_deref(), since, history).await,
    };
    Ok(json_result(&value))
}

async fn registry_empty(global_db: &RegisteredGlobalDb) -> Result<Value> {
    Ok(json!({ "empty": global_db.list_code_projects(1).await?.is_empty() }))
}

async fn registry_project_tokens(
    global_db: &RegisteredGlobalDb,
    project_args: &[PathBuf],
) -> Value {
    let mut projects = Vec::with_capacity(project_args.len());
    for project in project_args {
        // A project the accounting store could not be read for reports a null
        // total and the reason, never a measured zero.
        projects.push(match global_db.try_get_project_tokens(project).await {
            Ok(tokens) => json!({ "project": project, "tokens": tokens }),
            Err(error) => json!({
                "project": project,
                "tokens": Value::Null,
                "error": error,
            }),
        });
    }
    json!({ "projects": projects })
}

async fn gain_query(
    global_db: &RegisteredGlobalDb,
    project_arg: Option<&Path>,
    since: i64,
    history: bool,
) -> Value {
    let project = project_arg.map(|path| path.to_string_lossy().to_string());
    if history {
        let rows = global_db.savings_history(project.as_deref(), since).await;
        return json!({
            "history": rows.iter().map(|row| json!({
                "day": row.day,
                "saved_tokens": row.saved_tokens,
                "calls": row.calls,
            })).collect::<Vec<_>>(),
        });
    }
    let total = global_db.sum_savings(project.as_deref(), since).await;
    json!({ "saved_tokens": total.saved_tokens, "calls": total.calls })
}

async fn registry_list(
    cg: Option<&TraceDecay>,
    global_db: &RegisteredGlobalDb,
    limit: usize,
    query: Option<&str>,
) -> Result<Value> {
    use crate::project_registry::{PublicCodeProject, build_project_registry_view};

    let limit = limit.clamp(1, 100_000);
    let mut projects = match query {
        Some(query) => global_db.try_search_code_projects(query, limit + 1).await?,
        None => global_db.list_code_projects(limit + 1).await?,
    };
    let truncated = projects.len() > limit;
    projects.truncate(limit);
    let active_id = match cg {
        Some(cg) => active_project_id(cg, global_db).await?,
        None => None,
    };
    let contexts = global_db
        .project_registry_contexts_for_projects(&projects)
        .await?;
    let view = build_project_registry_view(&contexts, active_id.as_deref(), truncated);
    let public = projects
        .iter()
        .map(|project| PublicCodeProject::from_record(project, active_id.as_deref()))
        .collect::<Vec<_>>();
    Ok(json!({
        "status": "ok",
        "limit": limit,
        "query": query,
        "truncated": truncated,
        "summary": view.summary,
        "project_tree": view.project_tree,
        "projects": public,
    }))
}

async fn active_project_id(
    cg: &TraceDecay,
    global_db: &RegisteredGlobalDb,
) -> Result<Option<String>> {
    let git_common_dir = crate::worktree::git_common_dir(cg.project_root());
    Ok(global_db
        .project_registry_context_by_identity(cg.project_root(), git_common_dir.as_deref())
        .await?
        .map(|context| context.project.project_id))
}

async fn registry_context(
    cg: Option<&TraceDecay>,
    global_db: &RegisteredGlobalDb,
    project_arg: Option<&Path>,
) -> Result<Value> {
    use crate::project_registry::PublicProjectRegistryContext;

    let Some(selector) = project_arg.or_else(|| cg.map(TraceDecay::project_root)) else {
        return Ok(json!({ "status": "invalid", "project": null }));
    };
    let selector_text = selector.to_string_lossy();
    let context = if RegisteredGlobalDb::is_explicit_project_path_selector(&selector_text) {
        None
    } else {
        global_db
            .project_registry_context_by_id(&selector_text)
            .await?
    };
    let context = match context {
        Some(context) => Some(context),
        None => match global_db
            .project_registry_context_by_alias(selector)
            .await?
        {
            Some(context) => Some(context),
            None if RegisteredGlobalDb::is_explicit_project_path_selector(&selector_text) => {
                let git_common_dir = crate::worktree::git_common_dir(selector);
                global_db
                    .project_registry_context_by_identity(selector, git_common_dir.as_deref())
                    .await?
            }
            None => None,
        },
    };
    let Some(context) = context else {
        return Ok(json!({ "status": "not_found", "project": null }));
    };
    let active_id = match cg {
        Some(cg) => active_project_id(cg, global_db).await?,
        None => None,
    };
    let public = PublicProjectRegistryContext::new(&context, active_id.as_deref());
    Ok(json!({
        "status": "ok",
        "project": public.project,
        "aliases": context.aliases,
        "stores": context.stores,
    }))
}

async fn cost_summary(global_db: &RegisteredGlobalDb, range: &str) -> Result<Value> {
    let accounting_error = |message| TraceDecayError::Config { message };
    crate::accounting::pricing::refresh_if_stale();
    let ingest = crate::accounting::parser::ingest(global_db).await;
    let since = crate::accounting::metrics::parse_range(range);
    let tokens_saved = global_db
        .try_global_tokens_saved()
        .await
        .map_err(accounting_error)?;
    let summary = crate::accounting::metrics::cost_summary(global_db, since, tokens_saved)
        .await
        .map_err(accounting_error)?;
    let today_since = crate::accounting::metrics::parse_range("today");
    let today_cost = global_db
        .try_total_cost_since(today_since)
        .await
        .map_err(accounting_error)?;
    let today_breakdown = global_db
        .try_token_breakdown_since(today_since)
        .await
        .map_err(accounting_error)?;
    let costs =
        crate::application::observability::costs_read_model(global_db, None, since as i64).await;
    Ok(json!({
        "range": range,
        "ingest": {
            "turns_inserted": ingest.turns_inserted,
            "cost_usd": ingest.cost_usd,
            "tokens_consumed": ingest.tokens_consumed,
        },
        "summary": {
            "total_cost": summary.total_cost,
            "total_input_tokens": summary.total_input_tokens,
            "total_output_tokens": summary.total_output_tokens,
            "total_cache_read_tokens": summary.total_cache_read_tokens,
            "by_model": summary.by_model,
            "by_category": summary.by_category,
            "tokens_saved": summary.tokens_saved,
            "efficiency_ratio": summary.efficiency_ratio,
        },
        "today": {
            "cost": today_cost,
            "input_tokens": today_breakdown.0,
            "output_tokens": today_breakdown.1,
            "cache_read_tokens": today_breakdown.2,
        },
        "costs": costs,
    }))
}

async fn execute_session_sync(
    context: &AdminCliContext<'_>,
    command: SessionSyncCommandV1,
) -> Result<Value> {
    let Some(service) = context.session_sync else {
        return Ok(render_session_sync_outcome(
            SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_authority_unavailable",
            },
        ));
    };
    let scope = session_sync_scope(context)?;
    let project_id = scope.project_id().clone();
    let identity = context.require_profile_identity()?;
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.session-sync.v1\0");
    digest.update(project_id.as_str().as_bytes());
    digest.update(identity.profile_id().as_str().as_bytes());
    let request_id =
        match context.request_id.clone() {
            Some(request_id) => request_id,
            None => RequestId::new(format!("session-sync.request.{}", now_micros().0)).map_err(
                |error| TraceDecayError::Config {
                    message: error.to_string(),
                },
            )?,
        };
    digest.update(request_id.as_str().as_bytes());
    match command {
        SessionSyncCommandV1::ImportTranscripts(_) => digest.update(b"import-transcripts"),
        SessionSyncCommandV1::SynchronizeGit(options) => {
            digest.update(b"synchronize-git");
            digest.update(options.since_unix().to_be_bytes());
            digest.update(options.max_sessions().to_be_bytes());
            digest.update([u8::from(options.dry_run())]);
        }
    }
    let stable_id = hex::encode(digest.finalize());
    let operation_id = RequestId::new(format!("session-sync.{stable_id}")).map_err(|error| {
        TraceDecayError::Config {
            message: error.to_string(),
        }
    })?;
    let idempotency_key =
        IdempotencyKey::new(format!("session-sync.{stable_id}")).map_err(|error| {
            TraceDecayError::Config {
                message: error.to_string(),
            }
        })?;
    let deadline = match context.deadline.clone() {
        Some(deadline) => deadline,
        None => Deadline::new(tracedecay_domain::UtcMicros(
            now_micros().0.saturating_add(30_000_000),
        ))
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?,
    };
    let cancellation =
        match context.cancellation.clone() {
            Some(cancellation) => cancellation,
            None => CancellationSignal::active(format!("session-sync.{stable_id}")).map_err(
                |error| TraceDecayError::Config {
                    message: error.to_string(),
                },
            )?,
        };
    let request = SessionSyncRequestV1::new(
        operation_id,
        idempotency_key,
        scope,
        deadline,
        cancellation,
        command,
    );
    Ok(render_session_sync_outcome(service.execute(request).await))
}

fn session_sync_scope(context: &AdminCliContext<'_>) -> Result<SessionSyncScopeV1> {
    let project = context.require_project()?;
    let identity = context.require_profile_identity()?;
    let project_id = project
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "daemon project identity is unavailable".to_string(),
        })
        .and_then(|value| {
            tracedecay_domain::ProjectId::new(value).map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })
        })?;
    Ok(SessionSyncScopeV1::new(
        project_id,
        identity.profile_id().clone(),
    ))
}

async fn control_session_sync(
    context: &AdminCliContext<'_>,
    idempotency_key: String,
    cancel: bool,
) -> Result<Value> {
    let Some(service) = context.session_sync else {
        return Ok(render_session_sync_outcome(
            SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_authority_unavailable",
            },
        ));
    };
    let control = SessionSyncControlV1::new(
        session_sync_scope(context)?,
        IdempotencyKey::new(idempotency_key).map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?,
    );
    let outcome = if cancel {
        service.cancel(control).await
    } else {
        service.status(control).await
    };
    Ok(render_session_sync_outcome(outcome))
}

fn render_session_sync_outcome(outcome: SessionSyncOutcomeV1) -> Value {
    match outcome {
        SessionSyncOutcomeV1::Accepted(receipt) => json!({
            "status": "accepted",
            "operation_id": receipt.operation_id,
            "idempotency_key": receipt.idempotency_key,
            "accepted_at": receipt.accepted_at,
        }),
        SessionSyncOutcomeV1::Joined(receipt) => json!({
            "status": "joined",
            "operation_id": receipt.operation_id,
            "idempotency_key": receipt.idempotency_key,
            "accepted_at": receipt.accepted_at,
        }),
        SessionSyncOutcomeV1::Complete(receipt) => json!({
            "status": "complete",
            "operation_id": receipt.admission.operation_id,
            "idempotency_key": receipt.admission.idempotency_key,
            "termination": receipt.termination,
            "stats": receipt.stats,
            "failure_codes": receipt.failure_codes,
            "completed_at": receipt.completed_at,
        }),
        SessionSyncOutcomeV1::Cancelled => json!({"status": "cancelled"}),
        SessionSyncOutcomeV1::DeadlineExceeded => json!({"status": "deadline_exceeded"}),
        SessionSyncOutcomeV1::WrongScope => json!({"status": "wrong_scope"}),
        SessionSyncOutcomeV1::Unavailable { reason_code } => {
            json!({"status": "unavailable", "reason_code": reason_code})
        }
    }
}

async fn sessions_unfinished(db: &Arc<RegisteredGlobalDb>, limit: usize) -> Result<Value> {
    let items = crate::store::GlobalDbWorkflowStore::new(Arc::clone(db))
        .list_unfinished_workflows(limit)
        .await
        .map_err(|message| TraceDecayError::Config { message })?;
    Ok(json!({ "items": items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_projectless_and_project_scoped_actions() {
        assert!(matches!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "sessions_import",
            })),
            Ok(AdminCliAction::SessionsImport)
        ));
        assert!(matches!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "storage_report",
                "project_id": null,
                "project_root": null,
            })),
            Ok(AdminCliAction::StorageReport {
                project_id: None,
                project_root: None,
                ..
            })
        ));
        assert!(matches!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "registry_context",
                "project_arg": "/repo",
            })),
            Ok(AdminCliAction::RegistryContext { .. })
        ));
        assert!(matches!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "sessions_git_sync",
                "since": 1,
                "limit_sessions": 50,
                "dry_run": true,
            })),
            Ok(AdminCliAction::SessionsGitSync { dry_run: true, .. })
        ));
        assert!(matches!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "sessions_sync_status",
                "idempotency_key": "session-sync.fixture",
            })),
            Ok(AdminCliAction::SessionsSyncStatus { .. })
        ));
        assert!(matches!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "sessions_sync_cancel",
                "idempotency_key": "session-sync.fixture",
            })),
            Ok(AdminCliAction::SessionsSyncCancel { .. })
        ));
    }

    #[test]
    fn rejects_unknown_admin_action() {
        assert!(serde_json::from_value::<AdminCliAction>(json!({ "action": "vacuum" })).is_err());
        assert!(
            serde_json::from_value::<AdminCliAction>(json!({"action": "sessions_ingest"})).is_err()
        );
        assert!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "sessions_git_backfill",
                "since": 1,
                "limit_sessions": 50,
                "dry_run": false,
            }))
            .is_err()
        );
    }

    #[test]
    fn missing_daemon_session_sync_owner_is_typed_unavailable() {
        let rendered = render_session_sync_outcome(SessionSyncOutcomeV1::Unavailable {
            reason_code: "session_sync_authority_unavailable",
        });

        assert_eq!(rendered["status"], "unavailable");
        assert_eq!(
            rendered["reason_code"],
            "session_sync_authority_unavailable"
        );
    }
}

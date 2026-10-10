//! `tracedecay_admin_cli`: the profile maintenance actions one-shot CLI
//! commands ask the daemon owner for. It is never advertised.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_contracts::retrieval::{
    AdminCliCostSummaryV1, AdminCliCostTodayV1, AdminCliCostTotalsV1, AdminCliGainDayV1,
    AdminCliGainHistoryV1, AdminCliGainTotalV1, AdminCliProjectTokenTotalV1,
    AdminCliProjectTokensV1, AdminCliRegistryContextV1, AdminCliRegistryEmptyV1,
    AdminCliRegistryListV1, AdminCliRegistryUpdateV1, AdminCliResultV1, AdminCliScopeV1,
    AdminCliSessionSyncV1, AdminCliSurfaceRequestV1, AdminCliUnfinishedSessionsV1,
};
use tracedecay_contracts::session_sync::{
    SessionGitSyncV1, SessionSyncCommandV1, SessionSyncControlV1, SessionSyncOutcomeV1,
    SessionSyncRequestV1, SessionSyncScopeV1, SessionSyncServicePort, SessionTranscriptImportV1,
};
use tracedecay_contracts::{CancellationSignal, Deadline, IdempotencyKey, RequestId, now_micros};
use tracedecay_dashboard_api::project_registry::{
    PublicProjectRegistryContext, align_public_checkout_branches, build_project_registry_view,
    public_code_project_from_record,
};
use tracedecay_domain::{ObservationScopeV1, ProjectId};

use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_project::project::TraceDecay;

use crate::handlers::SessionAuthorities;

struct AdminCliContext<'a> {
    global_db: &'a RegisteredGlobalDbLeaseV1,
    accounting_db: Option<&'a RegisteredGlobalDb>,
    profile_root: Option<&'a Path>,
    project: Option<&'a TraceDecay>,
    /// The caller's checkout: the served project, or the project a profile
    /// owner's caller is connected to. It only marks registry rows active.
    active_checkout: Option<&'a Path>,
    registered_project_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    registered_user_session_db: Option<&'a RegisteredGlobalDbLeaseV1>,
    profile_identity: Option<std::sync::Arc<dyn tracedecay_contracts::ProfileIdentityReadPort>>,
    session_sync: Option<&'a dyn SessionSyncServicePort>,
    request_id: Option<RequestId>,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
    /// The daemon's code-index scheduler registry, which resolves the
    /// retention protection set a storage report plans against.
    code_index_schedulers: Option<&'a CodeIndexSchedulerRegistryV1>,
}

impl<'a> AdminCliContext<'a> {
    #[allow(
        clippy::too_many_arguments,
        reason = "Binds independently admitted project, profile, session, and request authorities at the CLI composition boundary"
    )]
    fn with_project(
        cg: &'a TraceDecay,
        global_db: &'a RegisteredGlobalDbLeaseV1,
        accounting_db: Option<&'a RegisteredGlobalDb>,
        profile_root: Option<&'a Path>,
        session_authorities: SessionAuthorities<'a>,
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
            active_checkout: Some(cg.project_root()),
            registered_project_session_db: session_authorities.project,
            registered_user_session_db: session_authorities.user,
            profile_identity: session_authorities.profile_identity,
            session_sync,
            request_id,
            deadline,
            cancellation,
            code_index_schedulers: None,
        }
    }

    fn projectless(
        global_db: &'a RegisteredGlobalDbLeaseV1,
        accounting_db: Option<&'a RegisteredGlobalDb>,
        profile_root: &'a Path,
        active_checkout: Option<&'a Path>,
        code_index_schedulers: Option<&'a CodeIndexSchedulerRegistryV1>,
    ) -> Self {
        Self {
            global_db,
            accounting_db,
            profile_root: Some(profile_root),
            project: None,
            active_checkout,
            registered_project_session_db: None,
            registered_user_session_db: None,
            profile_identity: None,
            session_sync: None,
            request_id: None,
            deadline: None,
            cancellation: None,
            code_index_schedulers,
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

    /// The project a cost or analytics action reads under `scope`: the served
    /// one, or none for the whole profile.
    fn scoped_project(&self, scope: AdminCliScopeV1) -> Result<Option<&'a TraceDecay>> {
        match scope {
            AdminCliScopeV1::Project => self.require_project().map(Some),
            AdminCliScopeV1::Profile => Ok(None),
        }
    }

    /// The served project's session store, when `project` is the served one.
    fn scoped_project_sessions(
        &self,
        project: Option<&TraceDecay>,
    ) -> Option<&'a RegisteredGlobalDb> {
        project
            .and(self.registered_project_session_db)
            .map(std::convert::AsRef::as_ref)
    }

    async fn registered_project_session_db(&self) -> Result<RegisteredGlobalDbLeaseV1> {
        let project = self.require_project()?;
        // Core servers do not retain session clients. The registered runtime
        // issues an exact project lease and enforces recovery/retirement fences.
        project
            .store_runtime_registry()
            .mount_registered_project_sessions(served_project_id(project)?)
            .await
    }

    fn require_profile_identity(
        &self,
    ) -> Result<&dyn tracedecay_contracts::ProfileIdentityReadPort> {
        self.profile_identity
            .as_deref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "daemon durable profile identity is unavailable".to_string(),
            })
    }
}

fn served_project_id(project: &TraceDecay) -> Result<ProjectId> {
    let project_id = project
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "daemon project identity is unavailable".to_owned(),
        })?;
    ProjectId::new(project_id).map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })
}

fn provider_usage_scope(project: Option<&TraceDecay>) -> Result<Option<ObservationScopeV1>> {
    project
        .map(|project| {
            Ok(ObservationScopeV1::Project {
                project_id: served_project_id(project)?,
            })
        })
        .transpose()
}

#[allow(
    clippy::too_many_arguments,
    reason = "CLI dispatch carries independently admitted store and sync authorities plus protocol request identity and controls"
)]
pub async fn compute_admin_cli(
    cg: &TraceDecay,
    request: AdminCliSurfaceRequestV1,
    global_db: Option<&RegisteredGlobalDbLeaseV1>,
    accounting_db: Option<&RegisteredGlobalDb>,
    profile_root: Option<&Path>,
    session_authorities: SessionAuthorities<'_>,
    session_sync: Option<&dyn SessionSyncServicePort>,
    request_id: Option<RequestId>,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<AdminCliResultV1> {
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
        request,
    )
    .await
}

pub async fn compute_projectless_admin_cli(
    request: AdminCliSurfaceRequestV1,
    global_db: &RegisteredGlobalDbLeaseV1,
    accounting_db: Option<&RegisteredGlobalDb>,
    profile_root: &Path,
    active_checkout: Option<&Path>,
    code_index_schedulers: Option<&CodeIndexSchedulerRegistryV1>,
) -> Result<AdminCliResultV1> {
    dispatch_admin_cli(
        AdminCliContext::projectless(
            global_db,
            accounting_db,
            profile_root,
            active_checkout,
            code_index_schedulers,
        ),
        request,
    )
    .await
}

#[tracing::instrument(name = "mcp.admin.cli.total", level = "trace", skip_all)]
#[expect(
    clippy::too_many_lines,
    reason = "Admin CLI dispatch is one subcommand match onto the owning composition-root action."
)]
async fn dispatch_admin_cli(
    context: AdminCliContext<'_>,
    request: AdminCliSurfaceRequestV1,
) -> Result<AdminCliResultV1> {
    let global_db = context.global_db;
    Ok(match request {
        AdminCliSurfaceRequestV1::CostSummary { range, scope } => {
            let project = context.scoped_project(scope)?;
            let provider_scope = provider_usage_scope(project)?;
            AdminCliResultV1::CostSummary(
                tracing::Instrument::instrument(
                    cost_summary(
                        context.require_accounting_db()?,
                        context.scoped_project_sessions(project),
                        provider_scope.as_ref(),
                        project.map(TraceDecay::project_root),
                        range,
                    ),
                    tracing::trace_span!("mcp.admin.cli.cost"),
                )
                .await?,
            )
        }
        AdminCliSurfaceRequestV1::SessionsImport {} => AdminCliResultV1::SessionSync(
            execute_session_sync(
                &context,
                SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
            )
            .await?,
        ),
        AdminCliSurfaceRequestV1::SessionsGitSync {
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
            AdminCliResultV1::SessionSync(
                execute_session_sync(&context, SessionSyncCommandV1::SynchronizeGit(options))
                    .await?,
            )
        }
        AdminCliSurfaceRequestV1::SessionsSyncStatus { idempotency_key } => {
            AdminCliResultV1::SessionSync(
                control_session_sync(&context, idempotency_key, false).await?,
            )
        }
        AdminCliSurfaceRequestV1::SessionsSyncCancel { idempotency_key } => {
            AdminCliResultV1::SessionSync(
                control_session_sync(&context, idempotency_key, true).await?,
            )
        }
        AdminCliSurfaceRequestV1::SessionsUnfinished { limit } => {
            let database = context.registered_project_session_db().await?;
            AdminCliResultV1::SessionsUnfinished(sessions_unfinished(&database, limit).await?)
        }
        AdminCliSurfaceRequestV1::SessionsUnusedContext {
            example_limit,
            session_limit,
        } => {
            let database = context.registered_project_session_db().await?;
            AdminCliResultV1::SessionsUnusedContext(
                crate::handlers::unused_context::sessions_unused_context(
                    &database,
                    example_limit,
                    session_limit,
                )
                .await?,
            )
        }
        AdminCliSurfaceRequestV1::AnalyticsSync { scope } => {
            let project = context.scoped_project(scope)?;
            AdminCliResultV1::AnalyticsSync(serde_json::from_value(
                tracedecay_application::analytics_bridge::analytics_sync_with_db(
                    context.require_accounting_db()?,
                    context.require_profile_root()?,
                    project.map(TraceDecay::project_root),
                )
                .await?,
            )?)
        }
        AdminCliSurfaceRequestV1::AnalyticsDiagnostics {
            scope,
            all,
            no_sync,
        } => {
            let project = context.scoped_project(scope)?;
            AdminCliResultV1::AnalyticsDiagnostics(
                tracedecay_application::analytics_bridge::analytics_diagnostics_with_db(
                    context.require_accounting_db()?,
                    context.require_profile_root()?,
                    context.scoped_project_sessions(project),
                    context
                        .registered_user_session_db
                        .map(std::convert::AsRef::as_ref),
                    project.map(TraceDecay::project_root),
                    all,
                    no_sync,
                )
                .await?,
            )
        }
        AdminCliSurfaceRequestV1::RegistryUpdate { tokens } => {
            let cg = context.require_project()?;
            // The previous total is informational; an unreadable ledger is
            // reported beside the update rather than blocking the write or
            // being shown as zero. The write itself fails closed.
            let previous = global_db.try_get_project_tokens(cg.project_root()).await;
            global_db
                .try_upsert_project_tokens(cg.project_root(), tokens)
                .await?;
            let (previous, previous_error) = match previous {
                Ok(previous) => (Some(previous), None),
                Err(error) => (None, Some(error)),
            };
            AdminCliResultV1::RegistryUpdate(AdminCliRegistryUpdateV1 {
                previous,
                previous_error,
                current: tokens,
            })
        }
        AdminCliSurfaceRequestV1::RegistryList {
            limit,
            query,
            project_arg,
        } => AdminCliResultV1::RegistryList(
            registry_list(
                context.active_checkout,
                global_db,
                limit,
                query,
                project_arg.as_deref(),
            )
            .await?,
        ),
        AdminCliSurfaceRequestV1::RegistryContext { project_arg } => {
            AdminCliResultV1::RegistryContext(
                registry_context(context.active_checkout, global_db, project_arg.as_deref())
                    .await?,
            )
        }
        AdminCliSurfaceRequestV1::RegistryEmpty {} => {
            AdminCliResultV1::RegistryEmpty(AdminCliRegistryEmptyV1 {
                empty: global_db.list_code_projects(1).await?.is_empty(),
            })
        }
        AdminCliSurfaceRequestV1::RegistryProjectTokens { project_args } => {
            AdminCliResultV1::RegistryProjectTokens(
                registry_project_tokens(global_db, project_args).await,
            )
        }
        AdminCliSurfaceRequestV1::RegistryGc { prefix, apply } => {
            let profile_root = context.require_profile_root()?;
            let report = if apply {
                tracedecay_global_db::registry_maintenance::apply_registry_gc(
                    global_db,
                    profile_root,
                    prefix,
                )
                .await?
            } else {
                tracedecay_global_db::registry_maintenance::registry_gc_report(
                    global_db,
                    profile_root,
                    prefix,
                )
                .await?
            };
            AdminCliResultV1::RegistryGc(serde_json::from_value(serde_json::to_value(report)?)?)
        }
        AdminCliSurfaceRequestV1::StorageReport {
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
                    tracedecay_maintenance::retention::storage_report::build_project_storage_report_from_daemon(
                        profile_root,
                        &project_id,
                        &project_root,
                        global_db,
                        context.code_index_schedulers,
                    )
                    .await?
                }
                (None, None) => {
                    tracedecay_maintenance::retention::storage_report::build_storage_report_page_from_registered_global_db(
                        profile_root,
                        global_db,
                        context.code_index_schedulers,
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
            AdminCliResultV1::StorageReport(serde_json::from_value(serde_json::to_value(report)?)?)
        }
        AdminCliSurfaceRequestV1::GainQuery {
            project_arg,
            since,
            history,
        } => gain_query(global_db, project_arg.as_deref(), since, history).await?,
    })
}

async fn registry_project_tokens(
    global_db: &RegisteredGlobalDb,
    project_args: Vec<PathBuf>,
) -> AdminCliProjectTokensV1 {
    let mut projects = Vec::with_capacity(project_args.len());
    for project in project_args {
        // A project the accounting store could not be read for reports a null
        // total and the reason, never a measured zero.
        let (tokens, error) = match global_db.try_get_project_tokens(&project).await {
            Ok(tokens) => (Some(tokens), None),
            Err(error) => (None, Some(error)),
        };
        projects.push(AdminCliProjectTokenTotalV1 {
            project,
            tokens,
            error,
        });
    }
    AdminCliProjectTokensV1 { projects }
}

/// Gain queries fail closed: an unreadable savings ledger is an error the
/// caller sees, never an empty history or a measured zero.
async fn gain_query(
    global_db: &RegisteredGlobalDb,
    project_arg: Option<&Path>,
    since: i64,
    history: bool,
) -> Result<AdminCliResultV1> {
    let accounting_error = |message| TraceDecayError::Config { message };
    let project = project_arg.map(|path| path.to_string_lossy().to_string());
    if history {
        let rows = global_db
            .savings_history(project.as_deref(), since)
            .await
            .map_err(accounting_error)?;
        return Ok(AdminCliResultV1::GainHistory(AdminCliGainHistoryV1 {
            history: rows
                .into_iter()
                .map(|row| AdminCliGainDayV1 {
                    day: row.day,
                    saved_tokens: row.saved_tokens,
                    calls: row.calls,
                })
                .collect(),
        }));
    }
    let total = global_db
        .sum_savings(project.as_deref(), since)
        .await
        .map_err(accounting_error)?;
    Ok(AdminCliResultV1::GainTotal(AdminCliGainTotalV1 {
        saved_tokens: total.saved_tokens,
        calls: total.calls,
    }))
}

async fn registry_list(
    active_checkout: Option<&Path>,
    global_db: &RegisteredGlobalDb,
    limit: usize,
    query: Option<String>,
    project_arg: Option<&Path>,
) -> Result<AdminCliRegistryListV1> {
    let limit = limit.clamp(1, 100_000);
    let mut projects = match query.as_deref() {
        Some(query) => global_db.try_search_code_projects(query, limit + 1).await?,
        None => global_db.list_code_projects(limit + 1).await?,
    };
    let truncated = projects.len() > limit;
    projects.truncate(limit);
    let active_checkout = active_checkout.or(project_arg);
    let active_id = match active_checkout {
        Some(project_root) => active_project_id(project_root, global_db).await?,
        None => None,
    };
    let contexts = global_db
        .project_registry_contexts_for_projects(&projects)
        .await?;
    let view =
        build_project_registry_view(&contexts, active_id.as_deref(), active_checkout, truncated);
    let mut public = projects
        .iter()
        .map(|project| public_code_project_from_record(project, active_id.as_deref()))
        .collect::<Vec<_>>();
    align_public_checkout_branches(&mut public, &view);
    Ok(AdminCliRegistryListV1::Ok {
        limit,
        query,
        truncated,
        summary: view.summary,
        project_tree: view.project_tree,
        projects: public,
    })
}

async fn active_project_id(
    project_root: &Path,
    global_db: &RegisteredGlobalDb,
) -> Result<Option<String>> {
    let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(project_root);
    Ok(global_db
        .project_registry_context_by_identity(project_root, git_common_dir.as_deref())
        .await?
        .map(|context| context.project.project_id))
}

async fn registry_context(
    active_checkout: Option<&Path>,
    global_db: &RegisteredGlobalDb,
    project_arg: Option<&Path>,
) -> Result<AdminCliRegistryContextV1> {
    let Some(selector) = project_arg.or(active_checkout) else {
        return Ok(AdminCliRegistryContextV1::Invalid { project: () });
    };
    let Some(context) = global_db
        .project_registry_context_by_selector(selector)
        .await?
    else {
        return Ok(AdminCliRegistryContextV1::NotFound { project: () });
    };
    let active_id = match active_checkout {
        Some(checkout) => active_project_id(checkout, global_db).await?,
        None => None,
    };
    let public =
        PublicProjectRegistryContext::at_checkout(&context, active_id.as_deref(), Some(selector));
    Ok(AdminCliRegistryContextV1::Ok {
        profile_id: global_db.binding().shard_id.profile_id.as_str().to_owned(),
        project: Box::new(public.project),
        aliases: rows_as_values(&context.aliases)?,
        stores: rows_as_values(&context.stores)?,
    })
}

fn rows_as_values<T: serde::Serialize>(rows: &[T]) -> Result<Vec<Value>> {
    rows.iter()
        .map(|row| serde_json::to_value(row).map_err(Into::into))
        .collect()
}

async fn cost_summary(
    savings_db: &RegisteredGlobalDb,
    provider_usage_db: Option<&RegisteredGlobalDb>,
    provider_scope: Option<&ObservationScopeV1>,
    project_root: Option<&Path>,
    range: String,
) -> Result<AdminCliCostSummaryV1> {
    let accounting_error = |message| TraceDecayError::Config { message };
    let since = tracedecay_session_memory::provider_usage::provider_usage_range_start(&range)
        .map_err(accounting_error)?;
    let since_seconds = i64::try_from(since).map_err(|_| TraceDecayError::Config {
        message: "provider usage range exceeds the supported timestamp domain".to_owned(),
    })?;
    let tokens_saved = match project_root {
        Some(project_root) => savings_db
            .try_get_project_tokens(project_root)
            .await
            .map_err(accounting_error)?,
        None => savings_db
            .try_global_tokens_saved()
            .await
            .map_err(accounting_error)?,
    };
    let summary = match (provider_usage_db, provider_scope) {
        (Some(db), Some(scope)) => {
            tracedecay_session_memory::provider_usage::provider_usage_cost_summary(
                db,
                scope,
                None,
                None,
                since_seconds,
            )
            .await
        }
        _ => unavailable_provider_usage_cost_summary(),
    };
    let consumed = summary
        .total_input_tokens
        .zip(summary.total_output_tokens)
        .and_then(|(input, output)| input.checked_add(output));
    let efficiency_ratio = consumed.and_then(|consumed| {
        let denominator = tokens_saved.checked_add(consumed)?;
        (denominator > 0).then_some(tokens_saved as f64 / denominator as f64)
    });
    let today_since =
        tracedecay_session_memory::provider_usage::provider_usage_range_start("today")
            .map_err(accounting_error)?;
    let today_since_seconds = i64::try_from(today_since).map_err(|_| TraceDecayError::Config {
        message: "provider usage range exceeds the supported timestamp domain".to_owned(),
    })?;
    let today = match (provider_usage_db, provider_scope) {
        (Some(db), Some(scope)) => {
            tracedecay_session_memory::provider_usage::provider_usage_cost_summary(
                db,
                scope,
                None,
                None,
                today_since_seconds,
            )
            .await
        }
        _ => unavailable_provider_usage_cost_summary(),
    };
    Ok(AdminCliCostSummaryV1 {
        range,
        summary: AdminCliCostTotalsV1 {
            provider_usage: serde_json::to_value(summary)?,
            tokens_saved,
            efficiency_ratio,
        },
        today: AdminCliCostTodayV1 {
            provider_usage: serde_json::to_value(today)?,
        },
    })
}

fn unavailable_provider_usage_cost_summary()
-> tracedecay_session_memory::provider_usage::ProviderUsageCostSummaryV1 {
    tracedecay_session_memory::provider_usage::ProviderUsageCostSummaryV1 {
        coverage: tracedecay_session_memory::provider_usage::ProviderUsageCoverageV1::Unavailable,
        pricing_revision: tracedecay_session_memory::provider_pricing::load_table()
            .revision
            .clone(),
        usage_events: 0,
        unpriced_events: 0,
        total_cost_usd: None,
        total_input_tokens: None,
        total_output_tokens: None,
        total_cache_read_tokens: None,
        total_cache_write_tokens: None,
        by_model: Vec::new(),
    }
}

#[tracing::instrument(name = "mcp.admin.cli.session_sync", level = "trace", skip_all)]
async fn execute_session_sync(
    context: &AdminCliContext<'_>,
    command: SessionSyncCommandV1,
) -> Result<AdminCliSessionSyncV1> {
    let Some(service) = context.session_sync else {
        return Ok(SESSION_SYNC_UNAVAILABLE.into());
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
    Ok(service.execute(request).await.into())
}

fn session_sync_scope(context: &AdminCliContext<'_>) -> Result<SessionSyncScopeV1> {
    let project = context.require_project()?;
    let identity = context.require_profile_identity()?;
    Ok(SessionSyncScopeV1::new(
        served_project_id(project)?,
        identity.profile_id().clone(),
    ))
}

#[tracing::instrument(name = "mcp.admin.cli.session_control", level = "trace", skip_all)]
async fn control_session_sync(
    context: &AdminCliContext<'_>,
    idempotency_key: String,
    cancel: bool,
) -> Result<AdminCliSessionSyncV1> {
    let Some(service) = context.session_sync else {
        return Ok(SESSION_SYNC_UNAVAILABLE.into());
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
    Ok(outcome.into())
}

async fn sessions_unfinished(
    db: &RegisteredGlobalDbLeaseV1,
    limit: usize,
) -> Result<AdminCliUnfinishedSessionsV1> {
    let items = tracedecay_global_db::GlobalDbWorkflowStore::new(db.clone())
        .list_unfinished_workflows(limit)
        .await
        .map_err(|message| TraceDecayError::Config { message })?;
    Ok(AdminCliUnfinishedSessionsV1 {
        items: rows_as_values(&items)?,
    })
}

const SESSION_SYNC_UNAVAILABLE: SessionSyncOutcomeV1 = SessionSyncOutcomeV1::Unavailable {
    reason_code: "session_sync_authority_unavailable",
};

#[cfg(test)]
mod tests {
    use super::*;

    async fn project_fixture(
        root: &Path,
        profile: &Path,
        project_id: &str,
    ) -> (
        TraceDecay,
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1,
    ) {
        std::fs::create_dir_all(root).unwrap();
        let runtime =
            tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
                profile,
                root,
                ProjectId::new(project_id).unwrap(),
            )
            .await
            .unwrap();
        let graph = runtime
            .initialize_project_graph_for_test(
                root,
                tracedecay_project::project::TraceDecayOpenOptions {
                    profile_root: Some(profile.to_owned()),
                    global_db_path: None,
                },
            )
            .await
            .unwrap();
        (graph, runtime)
    }

    #[tokio::test]
    async fn unfinished_session_read_uses_exact_registered_project_authority() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("profile");
        let (project, _runtime) = project_fixture(
            &directory.path().join("project"),
            &profile,
            "project.unfinished",
        )
        .await;
        let (foreign, _foreign_runtime) = project_fixture(
            &directory.path().join("foreign"),
            &directory.path().join("foreign-profile"),
            "project.foreign",
        )
        .await;
        let foreign_database = foreign
            .store_runtime_registry()
            .mount_registered_project_sessions(ProjectId::new("project.foreign").unwrap())
            .await
            .unwrap();
        let profile_database = project
            .store_runtime_registry()
            .profile_sessions()
            .await
            .unwrap();
        let mut context =
            AdminCliContext::projectless(&profile_database, None, &profile, None, None);
        context.project = Some(&project);
        // A supplied foreign session pointer must never substitute for the
        // current project's registered runtime authority.
        context.registered_project_session_db = Some(&foreign_database);
        let selected = context.registered_project_session_db().await.unwrap();
        let expected = project
            .store_runtime_registry()
            .mount_registered_project_sessions(ProjectId::new("project.unfinished").unwrap())
            .await
            .unwrap();
        assert_eq!(selected.binding(), expected.binding());
        assert_eq!(selected.verified_locator(), expected.verified_locator());
        assert_ne!(selected.binding(), foreign_database.binding());
        assert!(!selected.shares_client_with(&expected));

        context.project = None;
        let error = context.registered_project_session_db().await.err().unwrap();
        assert!(matches!(error, TraceDecayError::Config { .. }));
        assert!(
            error
                .to_string()
                .contains("requires an initialized project")
        );
    }

    /// The registry collection plan and the storage report cross the owner
    /// boundary as their typed results: the store crates' reports convert
    /// with every field, and serialize back to the same body.
    #[test]
    fn store_reports_convert_to_their_typed_results_without_loss() {
        let record = tracedecay_global_db::CodeProjectRecord {
            project_id: "project.gone".to_owned(),
            canonical_root: "/gone".to_owned(),
            display_root: "/gone".to_owned(),
            git_common_dir: None,
            git_remote_url: None,
            default_branch: Some("main".to_owned()),
            created_at: 1,
            last_seen_at: 2,
        };
        let gc = tracedecay_global_db::registry_maintenance::RegistryGcReport {
            apply: true,
            prefix: Some("/gone".to_owned()),
            candidate_count: 2,
            metadata_candidate_count: 1,
            code_project_candidate_count: 1,
            storage_project_candidate_count: 1,
            protected_code_project_count: 0,
            deleted_count: 2,
            deleted_code_project_count: 1,
            deleted_storage_project_count: 1,
            candidate_paths: vec!["/gone".to_owned()],
            candidates: vec![record],
            protected_code_projects: Vec::new(),
            storage_project_candidates: vec![PathBuf::from("/profile/stores/gone")],
        };
        let body = serde_json::to_value(&gc).unwrap();
        let typed: tracedecay_contracts::retrieval::AdminCliRegistryGcV1 =
            serde_json::from_value(body.clone()).unwrap();
        assert_eq!(serde_json::to_value(&typed).unwrap(), body);
        assert_eq!(
            typed.candidates,
            vec![serde_json::json!({
                "project_id": "project.gone",
                "canonical_root": "/gone",
                "display_root": "/gone",
                "git_common_dir": null,
                "git_remote_url": null,
                "default_branch": "main",
                "created_at": 1,
                "last_seen_at": 2,
            })]
        );

        let mut storage = tracedecay_maintenance::retention::storage_report::StorageReport {
            profile_root: "/profile".to_owned(),
            unregistered_dir_count: 3,
            unregistered_bytes: 4096,
            global_db_bytes: 8192,
            ..Default::default()
        };
        storage.full_profile_size = Some(
            tracedecay_maintenance::retention::storage_report::FullProfileSizeV1 {
                state: tracedecay_maintenance::retention::storage_report::ProfileTotalCoverageStateV1::Complete,
                total_bytes: 12_288,
                unavailable_entry_count: 0,
            },
        );
        let body = serde_json::to_value(&storage).unwrap();
        let typed: tracedecay_contracts::retrieval::AdminCliStorageReportV1 =
            serde_json::from_value(body.clone()).unwrap();
        assert_eq!(serde_json::to_value(&typed).unwrap(), body);
        assert_eq!(typed.unregistered_bytes, 4096);
        assert_eq!(
            typed.coverage,
            serde_json::json!({"state": "complete", "next_cursor": null})
        );
    }
}

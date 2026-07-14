//! Unadvertised daemon-owned operations used by one-shot CLI commands.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::{AnalyticsEventQuery, GlobalDb};
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;

const GIT_BACKFILL_ANALYTICS_LIMIT: usize = 500_000;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum AdminCliAction {
    CostSummary {
        range: String,
    },
    SessionsIngest,
    SessionsGitBackfill {
        since: i64,
        limit_sessions: usize,
        dry_run: bool,
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
    GainQuery {
        project_arg: Option<PathBuf>,
        since: i64,
        history: bool,
    },
}

struct AdminCliContext<'a> {
    global_db: &'a GlobalDb,
    project: Option<&'a TraceDecay>,
}

impl<'a> AdminCliContext<'a> {
    fn with_project(cg: &'a TraceDecay, global_db: &'a GlobalDb) -> Self {
        Self {
            global_db,
            project: Some(cg),
        }
    }

    fn projectless(global_db: &'a GlobalDb) -> Self {
        Self {
            global_db,
            project: None,
        }
    }

    fn require_project(&self) -> Result<&'a TraceDecay> {
        self.project.ok_or_else(|| TraceDecayError::Config {
            message: "requested admin action requires an initialized project".to_string(),
        })
    }

    fn project_root(&self) -> Option<&'a Path> {
        self.project.map(|cg| cg.project_root())
    }
}

pub(super) async fn handle_admin_cli(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
) -> Result<ToolResult> {
    let action = parse_admin_cli_action(args)?;
    let global_db = global_db.ok_or_else(|| TraceDecayError::Config {
        message: "daemon global database is unavailable".to_string(),
    })?;
    dispatch_admin_cli(AdminCliContext::with_project(cg, global_db), action).await
}

pub(crate) async fn handle_projectless_admin_cli(
    args: Value,
    global_db: &GlobalDb,
) -> Result<ToolResult> {
    let action = parse_admin_cli_action(args)?;
    dispatch_admin_cli(AdminCliContext::projectless(global_db), action).await
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
        AdminCliAction::CostSummary { range } => cost_summary(global_db, &range).await,
        AdminCliAction::SessionsIngest => sessions_ingest(context.require_project()?).await?,
        AdminCliAction::SessionsGitBackfill {
            since,
            limit_sessions,
            dry_run,
        } => {
            sessions_git_backfill(
                context.require_project()?,
                global_db,
                since,
                limit_sessions,
                dry_run,
            )
            .await?
        }
        AdminCliAction::SessionsUnfinished { limit } => {
            sessions_unfinished(context.require_project()?, limit).await?
        }
        AdminCliAction::AnalyticsSync => {
            crate::analytics_bridge::analytics_sync_with_db(global_db, context.project_root()).await
        }
        AdminCliAction::AnalyticsDiagnostics { all, no_sync } => {
            crate::analytics_bridge::analytics_diagnostics_with_db(
                global_db,
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
            registry_list(context.project, global_db, limit, query.as_deref()).await
        }
        AdminCliAction::RegistryContext { project_arg } => {
            registry_context(context.project, global_db, project_arg.as_deref()).await
        }
        AdminCliAction::RegistryEmpty => registry_empty(global_db).await,
        AdminCliAction::RegistryProjectTokens { project_args } => {
            registry_project_tokens(global_db, &project_args).await
        }
        AdminCliAction::GainQuery {
            project_arg,
            since,
            history,
        } => gain_query(global_db, project_arg.as_deref(), since, history).await,
    };
    Ok(json_result(&value))
}

async fn registry_empty(global_db: &GlobalDb) -> Value {
    json!({ "empty": global_db.list_code_projects(1).await.is_empty() })
}

async fn registry_project_tokens(global_db: &GlobalDb, project_args: &[PathBuf]) -> Value {
    let mut projects = Vec::with_capacity(project_args.len());
    for project in project_args {
        projects.push(json!({
            "project": project,
            "tokens": global_db.get_project_tokens(project).await,
        }));
    }
    json!({ "projects": projects })
}

async fn gain_query(
    global_db: &GlobalDb,
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
    global_db: &GlobalDb,
    limit: usize,
    query: Option<&str>,
) -> Value {
    use crate::project_registry::{PublicCodeProject, build_project_registry_view};

    let limit = limit.clamp(1, 100_000);
    let mut projects = match query {
        Some(query) => global_db.search_code_projects(query, limit + 1).await,
        None => global_db.list_code_projects(limit + 1).await,
    };
    let truncated = projects.len() > limit;
    projects.truncate(limit);
    let active_id = match cg {
        Some(cg) => active_project_id(cg, global_db).await,
        None => None,
    };
    let contexts = global_db
        .project_registry_contexts_for_projects(&projects)
        .await;
    let view = build_project_registry_view(&contexts, active_id.as_deref(), truncated);
    let public = projects
        .iter()
        .map(|project| PublicCodeProject::from_record(project, active_id.as_deref()))
        .collect::<Vec<_>>();
    json!({
        "status": "ok",
        "limit": limit,
        "query": query,
        "truncated": truncated,
        "summary": view.summary,
        "project_tree": view.project_tree,
        "projects": public,
    })
}

async fn active_project_id(cg: &TraceDecay, global_db: &GlobalDb) -> Option<String> {
    let git_common_dir = crate::worktree::git_common_dir(cg.project_root());
    global_db
        .project_registry_context_by_identity(cg.project_root(), git_common_dir.as_deref())
        .await
        .map(|context| context.project.project_id)
}

async fn registry_context(
    cg: Option<&TraceDecay>,
    global_db: &GlobalDb,
    project_arg: Option<&Path>,
) -> Value {
    use crate::project_registry::PublicProjectRegistryContext;

    let Some(selector) = project_arg.or_else(|| cg.map(TraceDecay::project_root)) else {
        return json!({ "status": "invalid", "project": null });
    };
    let selector_text = selector.to_string_lossy();
    let context = if GlobalDb::is_explicit_project_path_selector(&selector_text) {
        None
    } else {
        global_db
            .project_registry_context_by_id(&selector_text)
            .await
    };
    let context = match context {
        Some(context) => Some(context),
        None => match global_db.project_registry_context_by_alias(selector).await {
            Some(context) => Some(context),
            None if GlobalDb::is_explicit_project_path_selector(&selector_text) => {
                let git_common_dir = crate::worktree::git_common_dir(selector);
                global_db
                    .project_registry_context_by_identity(selector, git_common_dir.as_deref())
                    .await
            }
            None => None,
        },
    };
    let Some(context) = context else {
        return json!({ "status": "not_found", "project": null });
    };
    let active_id = match cg {
        Some(cg) => active_project_id(cg, global_db).await,
        None => None,
    };
    let public = PublicProjectRegistryContext::new(&context, active_id.as_deref());
    json!({
        "status": "ok",
        "project": public.project,
        "aliases": context.aliases,
        "stores": context.stores,
    })
}

async fn cost_summary(global_db: &GlobalDb, range: &str) -> Value {
    crate::accounting::pricing::refresh_if_stale();
    let ingest = crate::accounting::parser::ingest(global_db).await;
    let since = crate::accounting::metrics::parse_range(range);
    let tokens_saved = global_db.global_tokens_saved().await.unwrap_or(0);
    let summary = crate::accounting::metrics::cost_summary(global_db, since, tokens_saved).await;
    let today_since = crate::accounting::metrics::parse_range("today");
    let today_cost = global_db.total_cost_since(today_since).await.unwrap_or(0.0);
    let today_breakdown = global_db
        .token_breakdown_since(today_since)
        .await
        .unwrap_or((0, 0, 0));
    json!({
        "range": range,
        "ingest": {
            "turns_inserted": ingest.turns_inserted,
            "cost_usd": ingest.cost_usd,
            "tokens_consumed": ingest.tokens_consumed,
        },
        "summary": summary.map(|summary| json!({
            "total_cost": summary.total_cost,
            "total_input_tokens": summary.total_input_tokens,
            "total_output_tokens": summary.total_output_tokens,
            "total_cache_read_tokens": summary.total_cache_read_tokens,
            "by_model": summary.by_model,
            "by_category": summary.by_category,
            "tokens_saved": summary.tokens_saved,
            "efficiency_ratio": summary.efficiency_ratio,
        })),
        "today": {
            "cost": today_cost,
            "input_tokens": today_breakdown.0,
            "output_tokens": today_breakdown.1,
            "cache_read_tokens": today_breakdown.2,
        },
    })
}

async fn open_session_db(cg: &TraceDecay) -> Result<GlobalDb> {
    GlobalDb::open_at(&cg.store_layout().sessions_db_path)
        .await
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "daemon could not open project session database for {}",
                cg.project_root().display()
            ),
        })
}

async fn sessions_ingest(cg: &TraceDecay) -> Result<Value> {
    let db = open_session_db(cg).await?;
    let stats = crate::sessions::ingest_global_sources(&db, cg.project_root()).await;
    Ok(json!({
        "sessions_upserted": stats.sessions_upserted,
        "messages_upserted": stats.messages_upserted,
    }))
}

async fn sessions_git_backfill(
    cg: &TraceDecay,
    global_db: &GlobalDb,
    since: i64,
    limit_sessions: usize,
    dry_run: bool,
) -> Result<Value> {
    use crate::sessions::git_correlation::{
        BackfillOptions, DEFAULT_SPAN_MERGE_GAP_SECS, SystemGit, run_backfill,
    };

    let session_db = open_session_db(cg).await?;
    let project_id = GlobalDb::canonical_project_key(cg.project_root());
    let analytics_events = global_db
        .query_analytics_events(&AnalyticsEventQuery {
            project_id: Some(project_id),
            since: Some(since),
            limit: GIT_BACKFILL_ANALYTICS_LIMIT,
            ..Default::default()
        })
        .await
        .unwrap_or_default();
    let stats = run_backfill(
        &session_db,
        &analytics_events,
        &SystemGit,
        &BackfillOptions {
            since,
            limit_sessions,
            merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
            max_commits_per_repo: 5_000,
            dry_run,
        },
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("git backfill failed: {error}"),
    })?;
    Ok(json!({
        "dry_run": dry_run,
        "sessions_scanned": stats.sessions_scanned,
        "spans_written": stats.spans_written,
        "commits_attributed": stats.commits_attributed,
        "skipped_no_window": stats.skipped_no_window,
        "skipped_not_worktree": stats.skipped_not_worktree,
        "skipped_git_error": stats.skipped_git_error,
        "skipped_total": stats.skipped_total(),
    }))
}

async fn sessions_unfinished(cg: &TraceDecay, limit: usize) -> Result<Value> {
    let db = open_session_db(cg).await?;
    let items = crate::sessions::workflow_state::list_unfinished(&db, limit)
        .await
        .map_err(|message| TraceDecayError::Config { message })?;
    Ok(json!({ "items": items }))
}

fn json_result(value: &Value) -> ToolResult {
    ToolResult::new(
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&value).unwrap_or_default(),
            }]
        }),
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_projectless_and_project_scoped_actions() {
        assert!(matches!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "registry_context",
                "project_arg": "/repo",
            })),
            Ok(AdminCliAction::RegistryContext { .. })
        ));
        assert!(matches!(
            serde_json::from_value::<AdminCliAction>(json!({
                "action": "sessions_git_backfill",
                "since": 1,
                "limit_sessions": 50,
                "dry_run": true,
            })),
            Ok(AdminCliAction::SessionsGitBackfill { dry_run: true, .. })
        ));
    }

    #[test]
    fn rejects_unknown_admin_action() {
        assert!(serde_json::from_value::<AdminCliAction>(json!({ "action": "vacuum" })).is_err());
    }
}

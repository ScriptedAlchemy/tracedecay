use std::path::Path;

use crate::{
    cli::{SessionsAction, SessionsSearchArgs},
    resolve_cli_project_root,
};
use tracedecay::sessions::{ProviderScope, SessionSearchFilters, SessionSearchTimeRange};
use tracedecay::timeutil::SearchTimeBound;

pub(crate) async fn handle_sessions_action(
    action: SessionsAction,
) -> tracedecay::errors::Result<()> {
    match action {
        SessionsAction::Ingest {
            provider,
            project_id,
            project_path,
        } => {
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            let db = tracedecay::sessions::cursor::open_project_session_db(&project_path)
                .await
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "could not open project session database for {}",
                        project_path.display()
                    ),
                })?;
            let _ = session_provider_scope(provider.as_deref())?;
            let stats = ingest_selected_session_sources(&db, &project_path).await;
            println!(
                "ingested {} session(s), {} message(s)",
                stats.sessions_upserted, stats.messages_upserted
            );
        }
        SessionsAction::Search(args) => {
            let SessionsSearchArgs {
                query,
                provider,
                scope,
                message_type,
                parent_session_id,
                limit,
                since,
                until,
                project_id,
                project_path,
                branch,
                worktree,
                commit,
            } = *args;
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            let db = tracedecay::sessions::cursor::open_project_session_db(&project_path)
                .await
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "could not open project session database for {}",
                        project_path.display()
                    ),
                })?;
            let provider_scope = session_provider_scope(provider.as_deref())?;
            let scope =
                tracedecay::sessions::SessionSearchScope::parse(&scope).ok_or_else(|| {
                    tracedecay::errors::TraceDecayError::Config {
                        message: "scope must be one of all, parents_only, subagents_only"
                            .to_string(),
                    }
                })?;
            let message_type = tracedecay::sessions::SessionMessageType::parse(&message_type)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: "message_type must be one of all, direct_user, tool_result"
                        .to_string(),
                })?;
            let git_filter = tracedecay::sessions::git_correlation::GitScopeFilter::from_args(
                branch.as_deref(),
                worktree.as_deref(),
                commit.as_deref(),
            )
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: err.to_string(),
            })?;
            let now = tracedecay::tracedecay::current_timestamp();
            let time_range = SessionSearchTimeRange {
                start_time: parse_time_filter_arg(
                    "since",
                    since.as_deref(),
                    now,
                    SearchTimeBound::Start,
                )?,
                end_time: parse_time_filter_arg(
                    "until",
                    until.as_deref(),
                    now,
                    SearchTimeBound::End,
                )?,
            };
            let _ = tracedecay::sessions::ingest_global_sources_for_provider(
                &db,
                &project_path,
                provider_scope.provider(),
            )
            .await;
            let results = if !git_filter.is_empty() {
                db.search_session_messages_git_scoped(
                    provider_scope.provider_id(),
                    None,
                    &query,
                    limit,
                    SessionSearchFilters {
                        scope,
                        message_type,
                        parent_session_id: parent_session_id.as_deref(),
                        time_range,
                    },
                    &git_filter,
                )
                .await
            } else if let Some(provider) = provider_scope.provider() {
                db.search_session_messages_filtered(
                    provider.id(),
                    None,
                    &query,
                    limit,
                    SessionSearchFilters {
                        scope,
                        message_type,
                        parent_session_id: parent_session_id.as_deref(),
                        time_range,
                    },
                )
                .await
            } else {
                db.search_session_messages_all_providers_filtered(
                    None,
                    &query,
                    limit,
                    SessionSearchFilters {
                        scope,
                        message_type,
                        parent_session_id: parent_session_id.as_deref(),
                        time_range,
                    },
                )
                .await
            };
            for result in results {
                println!(
                    "[{}] {} {}: {}",
                    result.session.provider,
                    result.session.project_key,
                    result.message.role,
                    result.message.text.replace('\n', " ")
                );
            }
        }
        SessionsAction::GitBackfill {
            project_id,
            project_path,
            since,
            limit_sessions,
            dry_run,
        } => {
            run_git_backfill(project_id, project_path, since, limit_sessions, dry_run).await?;
        }
        SessionsAction::Unfinished {
            limit,
            json,
            project_id,
            project_path,
        } => {
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            let db = tracedecay::sessions::cursor::open_project_session_db(&project_path)
                .await
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "could not open project session database for {}",
                        project_path.display()
                    ),
                })?;
            let items = tracedecay::sessions::workflow_state::list_unfinished(&db, limit)
                .await
                .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&items).map_err(|e| {
                        tracedecay::errors::TraceDecayError::Config {
                            message: e.to_string(),
                        }
                    })?
                );
            } else {
                for item in items {
                    let task_id = item.task_id.as_deref().unwrap_or("-");
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        item.status,
                        item.provider,
                        item.session_id,
                        task_id,
                        item.message_id,
                        item.evidence
                    );
                }
            }
        }
    }
    Ok(())
}

/// Default lower bound for `git-backfill`: 90 days before now.
const GIT_BACKFILL_DEFAULT_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;

/// Cap on analytics rows loaded for `git-backfill`. The query is already scoped
/// to one project within the backfill window; this bounds the worst case rather
/// than materializing every event ever recorded.
const GIT_BACKFILL_ANALYTICS_LIMIT: usize = 500_000;

async fn run_git_backfill(
    project_id: Option<String>,
    project_path: Option<String>,
    since: Option<String>,
    limit_sessions: usize,
    dry_run: bool,
) -> tracedecay::errors::Result<()> {
    use tracedecay::sessions::git_correlation::{
        BackfillOptions, DEFAULT_SPAN_MERGE_GAP_SECS, SystemGit, run_backfill,
    };

    let project_root = resolve_cli_project_root(None, project_id, project_path).await?;
    let since_ts = resolve_backfill_since(since.as_deref())?;

    let session_db = tracedecay::sessions::cursor::open_project_session_db(&project_root)
        .await
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "could not open project session database for {}",
                project_root.display()
            ),
        })?;

    // Global analytics rows are a finer-grained timestamp signal. Missing or
    // unreadable analytics is non-fatal: the backfill still runs on session
    // timestamps and the reflog. Scope the query to this project and the
    // backfill window: only rows for the target project within [since_ts, now]
    // are ever consumed, so pulling every event from every project (these
    // stores reach tens of GB) would be pure waste.
    let analytics_events = match tracedecay::global_db::GlobalDb::open().await {
        Some(global) => global
            .query_analytics_events(&tracedecay::global_db::AnalyticsEventQuery {
                project_id: Some(tracedecay::global_db::GlobalDb::canonical_project_key(
                    &project_root,
                )),
                since: Some(since_ts),
                limit: GIT_BACKFILL_ANALYTICS_LIMIT,
                ..Default::default()
            })
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };

    let opts = BackfillOptions {
        since: since_ts,
        limit_sessions,
        merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
        max_commits_per_repo: 5_000,
        dry_run,
    };

    let git = SystemGit;
    let stats = run_backfill(&session_db, &analytics_events, &git, &opts)
        .await
        .map_err(|err| tracedecay::errors::TraceDecayError::Config {
            message: format!("git backfill failed: {err}"),
        })?;

    if dry_run {
        println!("git-backfill (dry-run): no rows written");
    }
    println!("sessions scanned:    {}", stats.sessions_scanned);
    println!("spans written:       {}", stats.spans_written);
    println!("commits attributed:  {}", stats.commits_attributed);
    println!(
        "skipped:             {} (no-window {}, not-worktree {}, git-error {})",
        stats.skipped_total(),
        stats.skipped_no_window,
        stats.skipped_not_worktree,
        stats.skipped_git_error
    );
    Ok(())
}

/// Resolves the `--since` argument (ISO-8601 or unix seconds) to a unix-second
/// lower bound, defaulting to 90 days before now when unset.
fn resolve_backfill_since(since: Option<&str>) -> tracedecay::errors::Result<i64> {
    let Some(raw) = since.map(str::trim).filter(|value| !value.is_empty()) else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        return Ok((now - GIT_BACKFILL_DEFAULT_WINDOW_SECS).max(0));
    };
    if let Ok(unix) = raw.parse::<i64>() {
        if unix >= 0 {
            return Ok(unix);
        }
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "--since must be >= 0".to_string(),
        });
    }
    tracedecay::timeutil::parse_rfc3339_timestamp(raw).ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "--since must be a non-negative Unix timestamp or ISO/RFC3339 string (got `{raw}`)"
            ),
        }
    })
}

async fn ingest_selected_session_sources(
    db: &tracedecay::global_db::GlobalDb,
    project_root: &Path,
) -> tracedecay::sessions::source::TranscriptIngestStats {
    tracedecay::sessions::ingest_global_sources(db, project_root).await
}

fn session_provider_scope(provider: Option<&str>) -> tracedecay::errors::Result<ProviderScope> {
    ProviderScope::parse_optional(provider)
        .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })
}

fn parse_time_filter_arg(
    name: &str,
    value: Option<&str>,
    now: i64,
    bound: SearchTimeBound,
) -> tracedecay::errors::Result<Option<i64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    tracedecay::timeutil::parse_search_time_filter_bound(value, now, bound)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "{name} must be a non-negative Unix timestamp, timezone-aware ISO/RFC3339 string, YYYY-MM-DD date, or relative time like 'last hour'"
            ),
        })
        .map(Some)
}

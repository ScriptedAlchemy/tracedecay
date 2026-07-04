use std::path::Path;

use crate::{cli::SessionsAction, resolve_cli_project_root};
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
        SessionsAction::Search {
            query,
            provider,
            limit,
            since,
            until,
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
            let provider_scope = session_provider_scope(provider.as_deref())?;
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
            let results = if let Some(provider) = provider_scope.provider() {
                db.search_session_messages_filtered(
                    provider.id(),
                    None,
                    &query,
                    limit,
                    SessionSearchFilters {
                        scope: tracedecay::sessions::SessionSearchScope::All,
                        parent_session_id: None,
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
                        scope: tracedecay::sessions::SessionSearchScope::All,
                        parent_session_id: None,
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
    }
    Ok(())
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

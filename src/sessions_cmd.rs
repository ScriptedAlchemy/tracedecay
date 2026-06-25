use std::path::Path;

use crate::{cli::SessionsAction, resolve_cli_project_root};

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
            let _ = optional_session_provider_scope(provider.as_deref())?;
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
            let selected_provider = optional_session_provider_scope(provider.as_deref())?;
            let _ = tracedecay::sessions::ingest_global_sources(&db, &project_path).await;
            let results = if let Some(provider) = selected_provider {
                db.search_session_messages(provider, None, &query, limit)
                    .await
            } else {
                db.search_session_messages_all_providers_filtered(
                    None,
                    &query,
                    limit,
                    tracedecay::sessions::SessionSearchScope::All,
                    None,
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

fn optional_session_provider_scope(
    provider: Option<&str>,
) -> tracedecay::errors::Result<Option<&str>> {
    match provider.map(str::trim).filter(|provider| !provider.is_empty()) {
        None | Some("all") => Ok(None),
        Some(
            provider @ ("cursor" | "claude" | "codex" | "vibe" | "cline" | "roo-code" | "kilo"
            | "kiro" | "hermes"),
        ) => Ok(Some(provider)),
        other => Err(tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "unknown session provider '{}' (expected all, cursor, claude, codex, vibe, cline, roo-code, kilo, kiro, or hermes)",
                other.unwrap_or_default()
            ),
        }),
    }
}

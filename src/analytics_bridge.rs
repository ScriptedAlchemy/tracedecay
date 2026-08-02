//! Bridges hook telemetry into the durable `analytics_events` table.
//!
//! Hooks append JSONL rows to `hook_analytics.jsonl` (project store when the
//! hook can resolve a project root, user-level profile root otherwise), while
//! the MCP server writes `mcp_tool_call` / `hook_route` rows straight into the
//! user-level global DB. This module imports the JSONL side into
//! `analytics_events` so one durable table answers adoption questions, using
//! per-file byte cursors in `parse_offsets` to stay idempotent across runs.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::global_db::RegisteredGlobalDb;

// The shared hook-analytics core (durable JSONL importer plus its data types)
// lives in `tracedecay-usecases`. The root binary keeps only the CLI/daemon
// orchestration below, plus the `Vec`-by-value `import_hook_analytics` wrapper
// whose owned argument keeps the spawned startup catch-up future `Send`.
use tracedecay_usecases::analytics_bridge::import_source;
pub use tracedecay_usecases::analytics_bridge::{
    HookImportOutcome, HookImportSource, HookImportSourceOutcome, hook_import_sources,
};

/// Imports new hook JSONL rows into `analytics_events`, advancing a byte
/// cursor per source file so re-runs only ingest the appended tail.
// Takes the source list by value: a borrowed slice iterator held across the
// per-source awaits trips rustc's higher-ranked Send leak check when this
// future runs inside the spawned startup catch-up task.
pub(crate) async fn import_hook_analytics(
    gdb: &RegisteredGlobalDb,
    sources: Vec<HookImportSource>,
) -> HookImportOutcome {
    let mut outcome = HookImportOutcome::default();
    for source in sources {
        outcome.sources.push(import_source(gdb, &source).await);
    }
    outcome
}

// ── CLI entry points (`tracedecay analytics …`) ────────────────────────

fn cli_error(message: impl std::fmt::Display) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: message.to_string(),
    }
}

fn cli_project_root() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::config::discover_project_root(&cwd))
}

async fn registered_diagnostics_message_count(
    project_sessions: Option<&RegisteredGlobalDb>,
    user_sessions: Option<&RegisteredGlobalDb>,
    all_projects: bool,
) -> crate::errors::Result<i64> {
    let mut total = match project_sessions {
        Some(database) => database.session_message_count().await.map_err(cli_error)?,
        None => 0,
    };
    if all_projects
        && let Some(database) = user_sessions
        && project_sessions.is_none_or(|project| project.db_path() != database.db_path())
    {
        total += database.session_message_count().await.map_err(cli_error)?;
    }
    Ok(total)
}

/// `tracedecay analytics sync`: import hook JSONL rows into the durable
/// `analytics_events` table and print what happened.
pub async fn run_analytics_sync() -> crate::errors::Result<()> {
    let project_root = cli_project_root();
    let outcome = call_admin_cli(project_root, json!({ "action": "analytics_sync" })).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&outcome).unwrap_or_default()
    );
    Ok(())
}

/// `tracedecay analytics diagnostics`: the CLI wrapper around the dashboard
/// diagnostics summary — durable `analytics_events` plus merged hook JSONL.
pub async fn run_analytics_diagnostics(
    all_projects: bool,
    no_sync: bool,
) -> crate::errors::Result<()> {
    let project_root = cli_project_root();
    let summary = call_admin_cli(
        project_root,
        json!({
            "action": "analytics_diagnostics",
            "all": all_projects,
            "no_sync": no_sync,
        }),
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    Ok(())
}

async fn call_admin_cli(
    project_root: Option<PathBuf>,
    arguments: Value,
) -> crate::errors::Result<Value> {
    let handshake =
        crate::daemon::DaemonHandshake::for_current_client(project_root, None, false, false)?;
    let result =
        crate::daemon::call_default_tool(&handshake, "tracedecay_admin_cli", arguments).await?;
    crate::daemon::tool_json_payload(&result, "tracedecay_admin_cli")
}

pub(crate) async fn analytics_sync_with_db(
    gdb: &RegisteredGlobalDb,
    project_root: Option<&Path>,
) -> Value {
    let sources = hook_import_sources(project_root);
    import_hook_analytics(gdb, sources).await.as_json()
}

pub(crate) async fn analytics_diagnostics_with_db(
    gdb: &RegisteredGlobalDb,
    project_sessions: Option<&RegisteredGlobalDb>,
    user_sessions: Option<&RegisteredGlobalDb>,
    project_root: Option<&Path>,
    all_projects: bool,
    no_sync: bool,
) -> crate::errors::Result<Value> {
    const EVENT_SAMPLE_LIMIT: usize = 10_000;

    let import = if no_sync {
        Value::Null
    } else {
        analytics_sync_with_db(gdb, project_root).await
    };

    let project_filter = if all_projects {
        None
    } else {
        project_root.map(RegisteredGlobalDb::canonical_project_key)
    };
    let events = gdb
        .query_analytics_events(&crate::global_db::AnalyticsEventQuery {
            provider: None,
            project_id: project_filter.clone(),
            session_id: None,
            event_kind: None,
            since: None,
            until: None,
            before_id: None,
            limit: EVENT_SAMPLE_LIMIT,
        })
        .await
        .map_err(cli_error)?;
    let observatory = crate::application::observability::observatory_read_model(
        gdb,
        project_filter.as_deref(),
        0,
    )
    .await;
    let observatory = crate::application::observability::observatory_cli_value(&observatory)
        .map_err(cli_error)?;
    let costs =
        crate::application::observability::costs_read_model(gdb, project_filter.as_deref(), 0)
            .await;
    let costs = crate::application::observability::costs_cli_value(&costs).map_err(cli_error)?;
    let event_rows: Vec<Value> = events
        .iter()
        .map(crate::dashboard::analytics_api::durable_analytics_event_row)
        .collect();

    let store_root = project_root.and_then(|root| {
        crate::storage::resolve_layout_for_current_profile(root)
            .ok()
            .map(|layout| layout.data_root)
    });
    let hook_filter_root = if all_projects { None } else { project_root };
    let hook_analytics = crate::dashboard::analytics_api::read_hook_analytics_rows_at(
        store_root.as_deref(),
        hook_filter_root,
    );

    let message_count =
        registered_diagnostics_message_count(project_sessions, user_sessions, all_projects).await?;

    let durable = if event_rows.is_empty() {
        None
    } else {
        Some(event_rows.as_slice())
    };
    let mut summary = crate::dashboard::analytics_api::diagnostics_summary_from_parts(
        message_count,
        &hook_analytics,
        durable,
    );
    if let Some(summary) = summary.as_object_mut() {
        summary.insert(
            "project_id".to_string(),
            project_filter.clone().map_or(Value::Null, Value::String),
        );
        summary.insert("observatory".to_string(), observatory);
        summary.insert("costs".to_string(), costs);
        summary.insert("import".to_string(), import);
        summary.insert(
            "global_db".to_string(),
            json!(gdb.db_path().display().to_string()),
        );
        summary.insert("event_sample_limit".to_string(), json!(EVENT_SAMPLE_LIMIT));
        summary.insert(
            "event_count_may_be_truncated".to_string(),
            json!(event_rows.len() >= EVENT_SAMPLE_LIMIT),
        );
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tracedecay_usecases::analytics_bridge::hook_row_to_analytics_event;

    use super::analytics_diagnostics_with_db;

    #[test]
    fn maps_hook_invoked_row_with_attribution() {
        let line = r#"{"agent":"claude","event":"hook_invoked","hook_name":"preToolUse","project_root":"/repo","session_id":"s1","tool_name":"Agent","ts_unix_ms":1783000000000}"#;
        let Some(event) = hook_row_to_analytics_event(line, None) else {
            panic!("row should map");
        };
        assert_eq!(event.provider, "hook_claude");
        assert_eq!(event.event_kind, "hook_invoked");
        assert_eq!(event.hook_name.as_deref(), Some("preToolUse"));
        assert_eq!(event.session_id.as_deref(), Some("s1"));
        assert_eq!(event.timestamp, 1_783_000_000);
        assert!(event.project_id.ends_with("repo"));
    }

    #[test]
    fn maps_hint_row_with_hint_id() {
        let line = r#"{"agent":"cursor","event":"hint_emitted","category":"search","hint_id":"h-abc","project_root":"/repo","session_id":"s1","ts_unix_ms":1783000000000}"#;
        let Some(event) = hook_row_to_analytics_event(line, None) else {
            panic!("row should map");
        };
        assert_eq!(event.hint_category.as_deref(), Some("search"));
        assert_eq!(event.hint_id.as_deref(), Some("h-abc"));

        let line = r#"{"agent":"cursor","event":"hint_emitted","category":"search","project_root":"/repo","session_id":"s1","ts_unix_ms":1783000000000}"#;
        let Some(event) = hook_row_to_analytics_event(line, None) else {
            panic!("row should map");
        };
        assert!(event.hint_id.is_none());
    }

    #[test]
    fn unattributed_row_falls_back_to_default_project() {
        let line = r#"{"agent":"cursor","event":"hook_invoked","hook_name":"postToolUse","ts_unix_ms":1783000000000}"#;
        let Some(event) = hook_row_to_analytics_event(line, Some(Path::new("/repo"))) else {
            panic!("row should map");
        };
        assert!(event.project_id.ends_with("repo"));
        let Some(event) = hook_row_to_analytics_event(line, None) else {
            panic!("row should map");
        };
        assert_eq!(event.project_id, "");
    }

    #[test]
    fn rows_without_event_field_are_skipped() {
        assert!(hook_row_to_analytics_event("{}", None).is_none());
        assert!(hook_row_to_analytics_event("not json", None).is_none());
    }

    #[tokio::test]
    async fn cli_diagnostics_exposes_canonical_observatory_and_costs_coverage() {
        crate::daemon::store_runtime::session_registry::register_profile_sessions_port();
        let harness = crate::global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "analytics-cli-observability-parity",
        )
        .await;
        let output =
            analytics_diagnostics_with_db(&harness.registered, None, None, None, true, true)
                .await
                .expect("CLI diagnostics");

        assert!(
            output["observatory"]["metrics"]
                .as_array()
                .is_some_and(|metrics| !metrics.is_empty())
        );
        assert!(
            output["costs"]["usage"]
                .as_array()
                .is_some_and(|metrics| !metrics.is_empty())
        );
        assert_eq!(output["observatory"]["metrics"][0]["value"], 0.0);
        assert_eq!(output["observatory"]["metrics"][0]["denominator_value"], 0);
        assert_eq!(
            output["observatory"]["metrics"][0]["coverage"]["state"],
            "known"
        );
    }
}

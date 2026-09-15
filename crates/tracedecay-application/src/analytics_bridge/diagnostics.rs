//! Daemon-side analytics diagnostics composition.
//!
//! Syncs hook JSONL through the parent module, then folds durable events,
//! observatory/costs read models, and the shared diagnostics summary.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_domain::ObservationScopeV1;
use tracedecay_store::StoreShardScopeV1;

use tracedecay_global_db::RegisteredGlobalDb;

use super::analytics_sync_with_db;
use super::summary::{
    diagnostics_summary_from_parts, durable_analytics_event_row, read_hook_analytics_rows_at,
};

fn cli_error(message: impl std::fmt::Display) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: message.to_string(),
    }
}

#[hotpath::measure(label = "analytics.messages")]
async fn registered_diagnostics_message_count(
    project_sessions: Option<&RegisteredGlobalDb>,
    user_sessions: Option<&RegisteredGlobalDb>,
    all_projects: bool,
) -> tracedecay_domain::errors::Result<i64> {
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

#[hotpath::measure(label = "analytics.diagnostics")]
pub async fn analytics_diagnostics_with_db(
    gdb: &RegisteredGlobalDb,
    project_sessions: Option<&RegisteredGlobalDb>,
    user_sessions: Option<&RegisteredGlobalDb>,
    project_root: Option<&Path>,
    all_projects: bool,
    no_sync: bool,
) -> tracedecay_domain::errors::Result<Value> {
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
    let events = hotpath::measure_block!(
        "analytics.events",
        gdb.query_analytics_events(&tracedecay_global_db::AnalyticsEventQuery {
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
        .map_err(cli_error)?
    );
    let observatory = hotpath::measure_block!("analytics.observatory", {
        let observatory =
            crate::observability::observatory_read_model(gdb, project_filter.as_deref(), 0).await;
        crate::observability::observatory_cli_value(&observatory).map_err(cli_error)?
    });
    let provider_scope = if all_projects {
        None
    } else {
        project_sessions.and_then(|sessions| match &sessions.binding().shard_id.scope {
            StoreShardScopeV1::ProjectSessions { project_id } => {
                Some(ObservationScopeV1::Project {
                    project_id: project_id.clone(),
                })
            }
            _ => None,
        })
    };
    let provider_usage_db = if all_projects { None } else { project_sessions };
    let costs = hotpath::measure_block!("analytics.costs", {
        let costs = crate::observability::costs_read_model(
            gdb,
            provider_usage_db,
            provider_scope.as_ref(),
            project_filter.as_deref(),
            0,
        )
        .await;
        crate::observability::costs_cli_value(&costs).map_err(cli_error)?
    });
    let event_rows: Vec<Value> = events.iter().map(durable_analytics_event_row).collect();

    let store_root = project_root.and_then(|root| {
        tracedecay_runtime_core::storage::resolve_layout_for_current_profile(root)
            .ok()
            .map(|layout| layout.data_root)
    });
    let hook_filter_root = if all_projects { None } else { project_root };
    let hook_analytics = hotpath::measure_block!(
        "analytics.hooks",
        read_hook_analytics_rows_at(store_root.as_deref(), hook_filter_root)
    );

    let message_count =
        registered_diagnostics_message_count(project_sessions, user_sessions, all_projects).await?;

    let durable = if event_rows.is_empty() {
        None
    } else {
        Some(event_rows.as_slice())
    };
    let mut summary = hotpath::measure_block!(
        "analytics.assemble",
        diagnostics_summary_from_parts(message_count, &hook_analytics, durable)
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
    use super::analytics_diagnostics_with_db;

    #[tokio::test]
    async fn cli_diagnostics_exposes_canonical_observatory_and_costs_coverage() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
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

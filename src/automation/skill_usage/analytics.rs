use std::collections::BTreeSet;
use std::path::Path;

use crate::analytics::{infer_usage_events, UsageKind};
use crate::errors::Result;
use crate::global_db::{AnalyticsEventQuery, AnalyticsEventRecord, GlobalDb};

use super::{
    config_error, ledger_skill_id, load_skill_usage_ledger, save_skill_usage_ledger,
    SkillUsageAction, SkillUsageEvent, SkillUsageRecord,
};

pub async fn ingest_analytics_events(
    profile_root: &Path,
    events: &[AnalyticsEventRecord],
) -> Result<Vec<SkillUsageRecord>> {
    let mut ledger = load_skill_usage_ledger(profile_root).await?;
    let mut touched = Vec::new();
    let mut seen = BTreeSet::new();
    for event in events {
        if should_skip_analytics_event(event) {
            continue;
        }
        for usage in skill_usage_events_from_analytics(event) {
            if usage.kind != UsageKind::Skill {
                continue;
            }
            let action = analytics_action(event);
            let skill_id = ledger_skill_id(&usage.name)?;
            let dedupe = analytics_import_key(event, &skill_id, action);
            if !seen.insert(dedupe.clone()) {
                continue;
            }
            if !ledger.imported_analytics_events.insert(dedupe) {
                continue;
            }
            let record = ledger
                .records
                .entry(skill_id.clone())
                .or_insert_with(|| SkillUsageRecord::new(skill_id, event.timestamp));
            record.record(&SkillUsageEvent {
                skill_name: usage.name,
                action,
                timestamp: event.timestamp,
                target: Some(event.provider.clone()),
            });
            touched.push(record.clone());
        }
    }
    if !touched.is_empty() {
        save_skill_usage_ledger(profile_root, &ledger).await?;
    }
    Ok(touched)
}

pub async fn ingest_project_analytics_events(
    profile_root: &Path,
    project_root: &Path,
    global_db: Option<&GlobalDb>,
    limit: usize,
) -> Result<Vec<SkillUsageRecord>> {
    let Some(global_db) = global_db else {
        return Ok(Vec::new());
    };
    let events = global_db
        .query_analytics_events(&AnalyticsEventQuery {
            provider: None,
            project_id: Some(GlobalDb::canonical_project_key(project_root)),
            session_id: None,
            event_kind: None,
            limit,
        })
        .await
        .map_err(|message| {
            config_error(format!(
                "failed to import project analytics into skill usage ledger: {message}"
            ))
        })?;
    ingest_analytics_events(profile_root, &events).await
}

fn skill_usage_events_from_analytics(
    event: &AnalyticsEventRecord,
) -> Vec<crate::analytics::UsageEvent> {
    let mut events = infer_usage_events(
        event.tool_name.as_deref(),
        event.metadata_json.as_deref(),
        None,
    );
    if let Some(skill_name) = event.skill_name.as_deref() {
        events.extend(infer_usage_events(
            None,
            Some(&serde_json::json!({ "skill": skill_name }).to_string()),
            None,
        ));
    }
    events
}

fn analytics_import_key(
    event: &AnalyticsEventRecord,
    skill_id: &str,
    action: SkillUsageAction,
) -> String {
    if let Some(request_id) = analytics_request_id(event) {
        return format!(
            "{}:{}:request:{request_id}:{}:{:?}",
            event.project_id, event.provider, skill_id, action
        );
    }
    format!(
        "{}:{}:{}:{}:{:?}",
        event.project_id, event.provider, event.id, skill_id, action
    )
}

fn should_skip_analytics_event(event: &AnalyticsEventRecord) -> bool {
    event.event_kind == "mcp_tool_call"
        && event
            .tool_name
            .as_deref()
            .is_some_and(crate::analytics::is_skill_view_tool)
        && event.outcome.as_deref().is_some_and(|outcome| {
            !matches!(
                outcome.to_ascii_lowercase().as_str(),
                "success" | "ok" | "succeeded"
            )
        })
}

fn analytics_request_id(event: &AnalyticsEventRecord) -> Option<String> {
    let metadata =
        serde_json::from_str::<serde_json::Value>(event.metadata_json.as_deref()?).ok()?;
    metadata
        .get("request_id")
        .or_else(|| metadata.pointer("/metadata/request_id"))
        .or_else(|| metadata.pointer("/runtime/request_id"))
        .or_else(|| metadata.pointer("/function/request_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|request_id| !request_id.is_empty())
        .map(ToOwned::to_owned)
}

fn analytics_action(event: &AnalyticsEventRecord) -> SkillUsageAction {
    match event.event_kind.as_str() {
        "tool" | "mcp_tool_call"
            if event
                .tool_name
                .as_deref()
                .is_some_and(crate::analytics::is_skill_view_tool) =>
        {
            SkillUsageAction::View
        }
        "skill_patch" | "skill_update" | "skill" if event.outcome.as_deref() == Some("patched") => {
            SkillUsageAction::Patch
        }
        _ => SkillUsageAction::Use,
    }
}

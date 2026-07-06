//! Handlers for read-only managed-skill MCP tools.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use crate::automation::hermes_bridge::{HermesSkillBridgeOptions, load_hermes_skill_bridge};
use crate::automation::managed_skills::{
    ManagedSkill, ManagedSkillState, list_managed_skills, load_managed_skill,
};
use crate::automation::run_ledger::{find_run_record, read_run_artifact_payload};
use crate::automation::skill_usage::{
    SkillUsageAction, analytics_import_key_for_request, ingest_project_analytics_events,
    record_skill_usage, skill_improvement_recommendations, stale_skill_recommendations,
    summarize_skill_usage, summarize_skill_usage_for,
};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::ToolResult;
use crate::tracedecay::TraceDecay;

use super::super::render::{self, Md};

const SKILL_ANALYTICS_IMPORT_LIMIT: usize = 10_000;
const STALE_SKILL_AFTER_SECS: i64 = 60 * 60 * 24 * 90;

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

fn tool_json(cg: &TraceDecay, args: &Value, value: &Value) -> ToolResult {
    tool_json_with_md(cg, args, value, || render::generic_md(value))
}

fn tool_json_with_md<F>(cg: &TraceDecay, args: &Value, value: &Value, md: F) -> ToolResult
where
    F: FnOnce() -> String,
{
    let text = render::finalize(Some(cg.project_root()), args, value, || md());
    ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        vec![],
    )
}

fn value_str<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn value_i64(value: &Value, pointer: &str) -> Option<i64> {
    value.pointer(pointer).and_then(Value::as_i64)
}

fn string_array(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn append_skill_item(md: &mut Md, skill: &Value) {
    let metadata = skill.get("metadata").unwrap_or(skill);
    let id = value_str(metadata, "/id");
    let title = value_str(metadata, "/title");
    let state = value_str(metadata, "/state");
    let mut line = if title.is_empty() || title == id {
        format!("**{id}**")
    } else {
        format!("**{id}** - {title}")
    };
    if !state.is_empty() {
        line.push_str(&format!(" ({state})"));
    }
    md.bullet(&line);

    let summary = value_str(metadata, "/summary");
    if !summary.is_empty() {
        md.line(&format!(
            "  summary: {}",
            summary.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    let category = value_str(metadata, "/category");
    let targets = string_array(metadata.get("targets"));
    let support_count = skill
        .get("support_file_count")
        .and_then(Value::as_i64)
        .or_else(|| {
            skill
                .get("support_files")
                .and_then(Value::as_array)
                .map(|files| files.len() as i64)
        });
    let mut details = Vec::new();
    if !category.is_empty() {
        details.push(format!("category: {category}"));
    }
    if !targets.is_empty() {
        details.push(format!("targets: {targets}"));
    }
    if let Some(count) = support_count {
        details.push(format!("support_files: {count}"));
    }
    if !details.is_empty() {
        md.line(&format!("  {}", details.join("; ")));
    }
}

fn render_skill_list_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Managed Skills");
    md.field("status", render::field_str(value, "status"));
    md.field("count", &render::field_i64(value, "count").to_string());
    let profile_root = render::field_str(value, "profile_root");
    if !profile_root.is_empty() {
        md.field("profile_root", profile_root);
    }
    md.blank().heading(3, "Skills");
    let Some(skills) = value.get("skills").and_then(Value::as_array) else {
        md.empty_note("No skills field returned.");
        return md.render();
    };
    if skills.is_empty() {
        md.empty_note("No managed skills.");
    } else {
        for skill in skills {
            append_skill_item(&mut md, skill);
        }
    }
    md.render()
}

fn render_skill_view_md(value: &Value) -> String {
    let mut md = Md::new();
    let skill = value.get("skill").unwrap_or(value);
    let metadata = skill.get("metadata").unwrap_or(skill);
    let id = value_str(metadata, "/id");
    md.heading(2, &format!("Managed Skill: {id}"));
    md.field("status", render::field_str(value, "status"));
    for (label, pointer) in [
        ("title", "/title"),
        ("state", "/state"),
        ("category", "/category"),
        ("checksum", "/checksum"),
    ] {
        let text = value_str(metadata, pointer);
        if !text.is_empty() {
            md.field(label, text);
        }
    }
    let targets = string_array(metadata.get("targets"));
    if !targets.is_empty() {
        md.field("targets", &targets);
    }
    if let Some(included) = value.get("support_files_included").and_then(Value::as_bool) {
        md.field(
            "support_files_included",
            if included { "true" } else { "false" },
        );
    }
    let summary = value_str(metadata, "/summary");
    if !summary.is_empty() {
        md.blank().heading(3, "Summary").line(summary);
    }
    let body = value_str(skill, "/body_markdown");
    if !body.is_empty() {
        md.blank().heading(3, "Body").line(body);
    }
    if let Some(files) = skill.get("support_files").and_then(Value::as_array) {
        md.blank().heading(3, "Support Files");
        if files.is_empty() {
            md.empty_note("No support files.");
        } else {
            for file in files {
                let path = value_str(file, "/path");
                let bytes = file
                    .get("bytes")
                    .and_then(Value::as_array)
                    .map(|bytes| bytes.len())
                    .unwrap_or_default();
                md.bullet(&format!("**{path}** - {bytes} bytes"));
            }
        }
    }
    md.render()
}

fn render_automation_artifact_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Automation Run Artifact");
    md.field("status", render::field_str(value, "status"));
    md.field("run_id", render::field_str(value, "run_id"));
    let artifact = value.get("artifact").unwrap_or(&Value::Null);
    for key in ["kind", "path", "sha256"] {
        let text = render::field_str(artifact, key);
        if !text.is_empty() {
            md.field(key, text);
        }
    }
    if let Some(size) = value_i64(artifact, "/size_bytes") {
        md.field("size_bytes", &size.to_string());
    }
    if let Some(payload) = value.get("payload") {
        md.blank().heading(3, "Payload");
        md.line(render::generic_md(payload).trim());
    }
    md.render()
}

fn optional_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| config_error(format!("missing required parameter: {key}")))
}

fn parse_state(args: &Value) -> Result<Option<ManagedSkillState>> {
    let Some(state) = args.get("state").and_then(Value::as_str) else {
        return Ok(None);
    };
    match state {
        "pending_approval" => Ok(Some(ManagedSkillState::PendingApproval)),
        "active" => Ok(Some(ManagedSkillState::Active)),
        "disabled" => Ok(Some(ManagedSkillState::Disabled)),
        "archived" => Ok(Some(ManagedSkillState::Archived)),
        other => Err(config_error(format!(
            "unknown managed skill state: {other}"
        ))),
    }
}

fn skill_summary(skill: &ManagedSkill, include_body: bool, usage_summary: &Value) -> Value {
    let mut summary = json!({
        "metadata": skill.metadata,
        "support_file_count": skill.support_files.len(),
        "support_file_paths": skill
            .support_files
            .iter()
            .map(|support| support.path.display().to_string())
            .collect::<Vec<_>>(),
        "usage_summary": usage_summary,
    });
    if include_body {
        summary["body_markdown"] = json!(skill.body_markdown);
    }
    summary
}

fn json_by_skill<T: Serialize>(
    items: &[T],
    skill_id: impl Fn(&T) -> &str,
) -> BTreeMap<String, Value> {
    items
        .iter()
        .map(|item| (skill_id(item).to_string(), json!(item)))
        .collect()
}

pub(super) async fn handle_skill_list(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let profile_root = crate::storage::default_profile_root()?;
    sync_project_skill_analytics(cg, &profile_root).await?;
    let state = parse_state(&args)?;
    let include_body = optional_bool(&args, "include_body", false);
    let mut skills = list_managed_skills(&profile_root).await?;
    if let Some(state) = state {
        skills.retain(|skill| skill.metadata.state == state);
    }
    let usage_summaries = summarize_skill_usage(&profile_root, &skills).await?;
    let recommendations = stale_skill_recommendations(
        &usage_summaries,
        crate::tracedecay::current_timestamp(),
        STALE_SKILL_AFTER_SECS,
    );
    let improvement_recommendations = skill_improvement_recommendations(&usage_summaries);
    let usage_by_skill = json_by_skill(&usage_summaries, |summary| &summary.skill_id);
    let recommendation_by_skill =
        json_by_skill(&recommendations, |recommendation| &recommendation.skill_id);
    let improvement_by_skill = json_by_skill(&improvement_recommendations, |recommendation| {
        &recommendation.skill_id
    });
    let payload = json!({
        "status": "ok",
        "profile_root": profile_root,
        "count": skills.len(),
        "skills": skills
            .iter()
            .map(|skill| {
                let skill_id = &skill.metadata.id;
                let usage_summary = usage_by_skill
                    .get(skill_id)
                    .cloned()
                    .unwrap_or(Value::Null);
                let stale_recommendation = recommendation_by_skill
                    .get(skill_id)
                    .cloned()
                    .unwrap_or(Value::Null);
                let improvement_recommendation = improvement_by_skill
                    .get(skill_id)
                    .cloned()
                    .unwrap_or(Value::Null);
                let mut summary = skill_summary(skill, include_body, &usage_summary);
                summary["stale_recommendation"] = stale_recommendation;
                summary["improvement_recommendation"] = improvement_recommendation;
                summary
            })
            .collect::<Vec<_>>(),
    });
    Ok(tool_json_with_md(cg, &args, &payload, || {
        render_skill_list_md(&payload)
    }))
}

pub(super) async fn handle_skill_view(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let profile_root = crate::storage::default_profile_root()?;
    sync_project_skill_analytics(cg, &profile_root).await?;
    let include_support_files = optional_bool(&args, "include_support_files", true);
    let mut skill = load_managed_skill(&profile_root, required_str(&args, "id")?).await?;
    let targets = skill
        .metadata
        .targets
        .iter()
        .map(|target| target.prompt_label().to_string())
        .collect::<Vec<_>>();
    record_skill_usage(
        &profile_root,
        &skill,
        SkillUsageAction::View,
        "mcp",
        targets,
        Some("mcp".to_string()),
        Some(json!({
            "tool": "tracedecay_skill_view",
            "include_support_files": include_support_files,
            "imported_analytics_event_key": args
                .get("__mcp_request_id")
                .and_then(Value::as_str)
                .map(|request_id| analytics_import_key_for_request(
                    &crate::global_db::GlobalDb::canonical_project_key(cg.project_root()),
                    "mcp",
                    request_id,
                    &skill.metadata.id,
                    SkillUsageAction::View,
                )),
        })),
    )
    .await?;
    let usage_summary = summarize_skill_usage_for(&profile_root, &skill).await?;
    let stale_recommendation = stale_skill_recommendations(
        std::slice::from_ref(&usage_summary),
        crate::tracedecay::current_timestamp(),
        STALE_SKILL_AFTER_SECS,
    )
    .into_iter()
    .next();
    let improvement_recommendation =
        skill_improvement_recommendations(std::slice::from_ref(&usage_summary))
            .into_iter()
            .next();
    if !include_support_files {
        skill.support_files.clear();
    }
    let payload = json!({
        "status": "ok",
        "profile_root": profile_root,
        "skill": skill,
        "usage_summary": usage_summary,
        "stale_recommendation": stale_recommendation,
        "improvement_recommendation": improvement_recommendation,
        "support_files_included": include_support_files,
    });
    Ok(tool_json_with_md(cg, &args, &payload, || {
        render_skill_view_md(&payload)
    }))
}

pub(super) async fn handle_automation_run_artifact_view(
    cg: &TraceDecay,
    args: Value,
) -> Result<ToolResult> {
    let run_id = required_str(&args, "run_id")?;
    let kind = required_str(&args, "kind")?;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let record = find_run_record(&dashboard_root, run_id)
        .await?
        .ok_or_else(|| config_error(format!("automation run not found: {run_id}")))?;
    let artifact = record
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == kind)
        .ok_or_else(|| {
            config_error(format!(
                "automation run artifact not found: {run_id}/{kind}"
            ))
        })?;
    let payload = read_run_artifact_payload(&dashboard_root, &record.run_id, artifact).await?;
    let payload = json!({
        "status": "ok",
        "run_id": record.run_id,
        "artifact": artifact,
        "payload": payload,
    });
    Ok(tool_json_with_md(cg, &args, &payload, || {
        render_automation_artifact_md(&payload)
    }))
}

async fn sync_project_skill_analytics(cg: &TraceDecay, profile_root: &Path) -> Result<()> {
    let global_db = crate::global_db::GlobalDb::open().await;
    ingest_project_analytics_events(
        profile_root,
        cg.project_root(),
        global_db.as_ref(),
        SKILL_ANALYTICS_IMPORT_LIMIT,
    )
    .await
    .map(|_| ())
}

pub(super) fn handle_hermes_skill_bridge(cg: &TraceDecay, args: &Value) -> Result<ToolResult> {
    let hermes_home = required_str(args, "hermes_home")?;
    let snapshot = load_hermes_skill_bridge(
        Path::new(hermes_home),
        HermesSkillBridgeOptions {
            include_skill_bodies: optional_bool(args, "include_skill_bodies", false),
            include_pending_payloads: optional_bool(args, "include_pending_payloads", false),
        },
    )?;
    let payload = json!({
        "status": "ok",
        "bridge": snapshot,
    });
    Ok(tool_json(cg, args, &payload))
}

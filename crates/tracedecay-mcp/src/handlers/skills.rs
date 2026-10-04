//! Read-only managed-skill and Hermes-inventory reads.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_automation_runtime::automation::hermes_skill_bridge::{
    HermesSkillBridgeOptions, load_standard_hermes_skill_bridge,
};
use tracedecay_automation_runtime::automation::managed_skills::{
    ManagedSkill, list_managed_skills, load_managed_skill,
};
use tracedecay_automation_runtime::automation::skill_usage::{
    SkillUsageAction, analytics_import_key_for_request, ingest_project_analytics_events,
    record_skill_usage, skill_improvement_recommendations, stale_skill_recommendations,
    summarize_skill_usage, summarize_skill_usage_for,
};
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    AutomationReadStatusV1, HermesSkillBridgeResultV1, HermesSkillBridgeSurfaceRequestV1,
    SkillListEntryV1, SkillListResultV1, SkillListSurfaceRequestV1, SkillSupportFileSummaryV1,
    SkillViewResultV1, SkillViewSurfaceRequestV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;

use crate::handlers::graph::graph_tool_completion;
use crate::handlers::support::decode_primitive_request;

const SKILL_ANALYTICS_IMPORT_LIMIT: usize = 10_000;
const STALE_SKILL_AFTER_SECS: i64 = 60 * 60 * 24 * 90;

/// The profile and project authorities a managed-skill read runs under.
pub struct SkillReadAuthority<'a> {
    pub profile_root: Option<&'a Path>,
    pub project_root: &'a Path,
    pub analytics_db: Option<&'a RegisteredGlobalDb>,
}

impl SkillReadAuthority<'_> {
    fn profile_root(&self) -> Result<&Path> {
        self.profile_root.ok_or_else(|| TraceDecayError::Config {
            message: "managed skills require the daemon's profile root".to_string(),
        })
    }

    async fn sync_project_analytics(&self, profile_root: &Path) -> Result<()> {
        ingest_project_analytics_events(
            profile_root,
            self.project_root,
            self.analytics_db,
            SKILL_ANALYTICS_IMPORT_LIMIT,
        )
        .await
        .map(|_| ())
    }
}

fn support_file_paths(skill: &ManagedSkill) -> Vec<String> {
    skill
        .support_files
        .iter()
        .map(|file| file.path.display().to_string())
        .collect()
}

#[tracing::instrument(name = "mcp.automation.skill_list.total", level = "trace", skip_all)]
pub async fn compute_skill_list(
    authority: &SkillReadAuthority<'_>,
    args: &Value,
) -> Result<GraphToolCompletionV1> {
    let request: SkillListSurfaceRequestV1 =
        decode_primitive_request(args, "tracedecay_skill_list")?;
    let profile_root = authority.profile_root()?;
    authority.sync_project_analytics(profile_root).await?;
    let mut skills = tracing::Instrument::instrument(
        list_managed_skills(profile_root),
        tracing::trace_span!("mcp.automation.skill_list.load"),
    )
    .await?;
    if let Some(state) = request.state {
        skills.retain(|skill| skill.metadata.state == state);
    }
    let usage_summaries = summarize_skill_usage(profile_root, &skills).await?;
    let now = tracedecay_runtime_core::tracedecay::current_timestamp();
    let mut stale: BTreeMap<String, _> =
        stale_skill_recommendations(&usage_summaries, now, STALE_SKILL_AFTER_SECS)
            .into_iter()
            .map(|recommendation| (recommendation.skill_id.clone(), recommendation))
            .collect();
    let mut improvements: BTreeMap<String, _> = skill_improvement_recommendations(&usage_summaries)
        .into_iter()
        .map(|recommendation| (recommendation.skill_id.clone(), recommendation))
        .collect();
    let entries = skills
        .into_iter()
        .zip(usage_summaries)
        .map(|(skill, usage_summary)| SkillListEntryV1 {
            support_file_count: skill.support_files.len(),
            support_file_paths: support_file_paths(&skill),
            usage_summary,
            stale_recommendation: stale.remove(&skill.metadata.id),
            improvement_recommendation: improvements.remove(&skill.metadata.id),
            body_markdown: request.include_body.then_some(skill.body_markdown),
            metadata: skill.metadata,
        })
        .collect::<Vec<_>>();
    Ok(graph_tool_completion(
        GraphToolResultV1::SkillList(SkillListResultV1 {
            status: AutomationReadStatusV1::Ok,
            profile_root: tracedecay_runtime_core::path_safety::plain_host_path(profile_root),
            count: entries.len(),
            skills: entries,
        }),
        Vec::new(),
    ))
}

#[tracing::instrument(name = "mcp.automation.skill_view.total", level = "trace", skip_all)]
pub async fn compute_skill_view(
    authority: &SkillReadAuthority<'_>,
    args: &Value,
) -> Result<GraphToolCompletionV1> {
    let request: SkillViewSurfaceRequestV1 =
        decode_primitive_request(args, "tracedecay_skill_view")?;
    let profile_root = authority.profile_root()?;
    authority.sync_project_analytics(profile_root).await?;
    let mut skill = tracing::Instrument::instrument(
        load_managed_skill(profile_root, &request.id),
        tracing::trace_span!("mcp.automation.skill_view.load"),
    )
    .await?;
    let targets = skill
        .metadata
        .targets
        .iter()
        .map(|target| target.prompt_label().to_string())
        .collect::<Vec<_>>();
    // The MCP server stamps the JSON-RPC id its analytics event records, so
    // the later import of that event does not count this view twice.
    let imported_analytics_event_key =
        args.get("__mcp_request_id")
            .and_then(Value::as_str)
            .map(|request_id| {
                analytics_import_key_for_request(
                    &RegisteredGlobalDb::canonical_project_key(authority.project_root),
                    "mcp",
                    request_id,
                    &skill.metadata.id,
                    SkillUsageAction::View,
                )
            });
    record_skill_usage(
        profile_root,
        &skill,
        SkillUsageAction::View,
        "mcp",
        targets,
        Some("mcp".to_string()),
        Some(json!({
            "tool": "tracedecay_skill_view",
            "include_support_files": request.include_support_files,
            "imported_analytics_event_key": imported_analytics_event_key,
        })),
    )
    .await?;
    let usage_summary = summarize_skill_usage_for(profile_root, &skill).await?;
    let stale_recommendation = stale_skill_recommendations(
        std::slice::from_ref(&usage_summary),
        tracedecay_runtime_core::tracedecay::current_timestamp(),
        STALE_SKILL_AFTER_SECS,
    )
    .into_iter()
    .next();
    let improvement_recommendation =
        skill_improvement_recommendations(std::slice::from_ref(&usage_summary))
            .into_iter()
            .next();
    // Path summaries stay in the response either way. Byte payloads are a
    // separate read the caller opts into, so a view does not inline unused
    // support files into the context window.
    let support_file_summaries = skill
        .support_files
        .iter()
        .map(|file| SkillSupportFileSummaryV1 {
            path: file.path.display().to_string(),
            byte_len: file.bytes.len(),
        })
        .collect();
    if !request.include_support_files {
        skill.support_files.clear();
    }
    Ok(graph_tool_completion(
        GraphToolResultV1::SkillView(Box::new(SkillViewResultV1 {
            status: AutomationReadStatusV1::Ok,
            profile_root: tracedecay_runtime_core::path_safety::plain_host_path(profile_root),
            skill,
            usage_summary,
            stale_recommendation,
            improvement_recommendation,
            support_files_included: request.include_support_files,
            support_file_summaries,
        })),
        Vec::new(),
    ))
}

#[tracing::instrument(name = "mcp.automation.hermes_bridge.total", level = "trace", skip_all)]
pub fn compute_hermes_skill_bridge(
    user_home: Option<&Path>,
    args: &Value,
) -> Result<GraphToolCompletionV1> {
    let request: HermesSkillBridgeSurfaceRequestV1 =
        decode_primitive_request(args, "tracedecay_hermes_skill_bridge")?;
    let bridge = load_standard_hermes_skill_bridge(
        user_home,
        HermesSkillBridgeOptions {
            include_skill_bodies: request.include_skill_bodies,
            include_pending_payloads: request.include_pending_payloads,
        },
    )?;
    Ok(graph_tool_completion(
        GraphToolResultV1::HermesSkillBridge(Box::new(HermesSkillBridgeResultV1 {
            status: AutomationReadStatusV1::Ok,
            bridge,
        })),
        Vec::new(),
    ))
}

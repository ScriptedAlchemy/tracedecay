//! Handler for the `tracedecay_analytics` MCP tool.
//!
//! Read-only adoption/telemetry rollup so an agent can see its own usage
//! without querying `.tracedecay` databases directly. Reuses the same
//! durable-analytics service functions the `tracedecay analytics
//! diagnostics` CLI and dashboard analytics API use
//! ([`crate::global_db::GlobalDb::query_analytics_tool_counts`],
//! [`crate::global_db::GlobalDb::query_analytics_hint_counts`],
//! [`crate::dashboard::analytics_api::hint_summary_from_counts`],
//! [`crate::automation::run_ledger::load_run_records`]) rather than
//! re-implementing queries against those tables.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::automation::run_ledger::load_run_records;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{AnalyticsToolCounts, GlobalDb};
use crate::timeutil::parse_rfc3339_timestamp;
use crate::tracedecay::TraceDecay;
use crate::tracedecay::current_timestamp;

use super::super::{ToolResult, renderers};
use super::memory::open_target_memory_db;
use super::support::{project_registry_context, tool_json_with_md};

/// Bound on how many automation run-ledger rows a single call will scan.
const AUTOMATION_RECORD_LIMIT: usize = 200;
/// Top-N tools shown by call volume.
const TOP_TOOLS_LIMIT: usize = 10;
/// Bound on how many zero-call tool names are listed inline.
const ZERO_CALL_SAMPLE_LIMIT: usize = 30;

const NAVIGATION_TOOLS: &[&str] = &[
    "search",
    "grep",
    "context",
    "callers",
    "callees",
    "impact",
    "node",
    "similar",
    "rename_preview",
    "implementations",
    "callers_for",
    "by_qualified_name",
    "call_chain",
    "file_dependents",
    "find_exact_symbol",
    "signature",
    "impls",
    "derives",
    "status",
    "active_project",
    "storage_status",
    "project_list",
    "project_search",
    "project_context",
    "body",
    "todos",
    "read",
    "outline",
    "config",
    "signature_search",
    "port_status",
    "port_order",
    "simplify_scan",
    "files",
    "type_hierarchy",
    "affected",
    "diff_context",
    "changelog",
    "commit_context",
    "pr_context",
    "branch_search",
    "branch_diff",
    "branch_list",
    "retrieve",
];
const ANALYSIS_TOOLS: &[&str] = &[
    "dead_code",
    "module_api",
    "circular",
    "hotspots",
    "unused_imports",
    "rank",
    "largest",
    "coupling",
    "inheritance_depth",
    "distribution",
    "recursion",
    "complexity",
    "doc_coverage",
    "god_class",
    "unsafe_patterns",
    "diagnostics",
    "constructors",
    "field_sites",
    "test_map",
    "gini",
    "dependency_depth",
    "health",
    "runtime",
    "test_risk",
    "redundancy",
    "dsm",
];
const SESSION_TOOLS: &[&str] = &[
    "session_start",
    "session_end",
    "message_search",
    "sessions_for",
    "workflows",
    "lcm_status",
    "lcm_doctor",
    "lcm_load_session",
    "lcm_grep",
    "lcm_describe",
    "lcm_expand",
    "lcm_expand_query",
    "lcm_preflight",
    "lcm_compress",
    "lcm_session_boundary",
];
const MEMORY_TOOLS: &[&str] = &["memory_status", "fact_store", "fact_feedback"];
const EDIT_TOOLS: &[&str] = &[
    "str_replace",
    "multi_str_replace",
    "insert_at",
    "insert_at_symbol",
    "replace_symbol",
    "ast_grep_rewrite",
];
const ADMIN_TOOLS: &[&str] = &[
    "dashboard",
    "skill_list",
    "skill_view",
    "automation_run_artifact_view",
    "hermes_skill_bridge",
    "diagnose",
    "run_affected_tests",
    "analytics",
];
/// Ordered tier list; used both for classification and stable output order.
const TIERS: &[(&str, &[&str])] = &[
    ("navigation", NAVIGATION_TOOLS),
    ("analysis", ANALYSIS_TOOLS),
    ("session", SESSION_TOOLS),
    ("memory", MEMORY_TOOLS),
    ("edit", EDIT_TOOLS),
    ("admin", ADMIN_TOOLS),
];

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

/// Strips a `tracedecay_`/`mcp__tracedecay__` prefix and returns the bucket
/// name for a tool. Unknown/non-tracedecay tool names bucket as `"other"`.
fn tool_tier(tool_name: &str) -> &'static str {
    let normalized = crate::analytics::normalize_tool_name(tool_name);
    let normalized = normalized
        .strip_prefix("tracedecay_")
        .unwrap_or(normalized.as_str());
    for (tier, names) in TIERS {
        if names.contains(&normalized) {
            return tier;
        }
    }
    "other"
}

#[derive(Default)]
struct ToolCallCounts {
    calls: i64,
    errors: i64,
}

fn parse_scope(args: &Value) -> Result<bool> {
    match args.get("scope").and_then(Value::as_str) {
        None | Some("project") => Ok(false),
        Some("all") => Ok(true),
        Some(other) => Err(config_error(format!(
            "unknown scope for tracedecay_analytics: {other} (use 'project' or 'all')"
        ))),
    }
}

fn parse_window_days(args: &Value) -> i64 {
    args.get("window_days")
        .and_then(Value::as_i64)
        .unwrap_or(14)
        .clamp(1, 365)
}

fn parse_section(args: &Value) -> Result<Option<&str>> {
    match args.get("section").and_then(Value::as_str) {
        None => Ok(None),
        Some(section @ ("tools" | "hints" | "facts" | "automation")) => Ok(Some(section)),
        Some(other) => Err(config_error(format!(
            "unknown section for tracedecay_analytics: {other} (use 'tools', 'hints', 'facts', or 'automation')"
        ))),
    }
}

fn wants_section(filter: Option<&str>, name: &str) -> bool {
    filter.is_none_or(|section| section == name)
}

struct ResolvedScope {
    /// `None` when `scope: "all"`; the canonical `analytics_events` project
    /// key otherwise.
    filter: Option<String>,
    /// Always resolved to a concrete project (used for the project-scoped
    /// facts/automation sections regardless of `scope`).
    root: PathBuf,
    display_root: String,
}

async fn resolve_scope(
    cg: &TraceDecay,
    args: &Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
    all_projects: bool,
) -> Result<ResolvedScope> {
    let context = project_registry_context(
        args,
        &["project_path"],
        global_db,
        allow_default_registry_fallback,
    )
    .await?;
    let (project_root, project_display) = match &context {
        // Resolving the selector through the registry's project_aliases join
        // (rather than trusting the raw selector path verbatim) keeps
        // per-project matching correct across worktrees/aliases of the same
        // logical project.
        Some(ctx) => (
            PathBuf::from(&ctx.project.canonical_root),
            ctx.project.display_root.clone(),
        ),
        None => (
            cg.project_root().to_path_buf(),
            cg.project_root().to_string_lossy().to_string(),
        ),
    };
    let filter = if all_projects {
        None
    } else {
        Some(GlobalDb::canonical_project_key(&project_root))
    };
    Ok(ResolvedScope {
        filter,
        root: project_root,
        display_root: project_display,
    })
}

async fn resolved_global_db(
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<Option<GlobalDb>> {
    if global_db.is_some() {
        return Ok(None);
    }
    if !allow_default_registry_fallback {
        return Err(config_error(
            "client global analytics store is unavailable for tracedecay_analytics",
        ));
    }
    let owned = GlobalDb::open().await.ok_or_else(|| {
        config_error("could not open tracedecay user-level global DB; run tracedecay init first")
    })?;
    Ok(Some(owned))
}

/// Handles `tracedecay_analytics` tool calls.
pub(super) async fn handle_analytics(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<ToolResult> {
    let all_projects = parse_scope(&args)?;
    let window_days = parse_window_days(&args);
    let section = parse_section(&args)?;

    let owned_db = resolved_global_db(global_db, allow_default_registry_fallback).await?;
    let gdb = global_db.or(owned_db.as_ref()).ok_or_else(|| {
        config_error("tracedecay_analytics could not resolve a global analytics DB")
    })?;

    let scope = resolve_scope(
        cg,
        &args,
        global_db,
        allow_default_registry_fallback,
        all_projects,
    )
    .await?;

    let since = current_timestamp().saturating_sub(window_days.saturating_mul(86_400));
    let event_count = gdb
        .count_analytics_events(scope.filter.as_deref(), since)
        .await
        .map_err(config_error)?;

    let mut value = json!({
        "status": "ok",
        "scope": if all_projects { "all" } else { "project" },
        "project_id": scope.filter,
        "project_root": scope.display_root,
        "window_days": window_days,
        "since": since,
        "event_count": event_count,
        "event_count_truncated": false,
    });

    if wants_section(section, "tools") {
        let counts = gdb
            .query_analytics_tool_counts(scope.filter.as_deref(), since)
            .await
            .map_err(config_error)?;
        if let Some(object) = value.as_object_mut() {
            object.insert("tools".to_string(), tools_section(&counts));
        }
    }
    if wants_section(section, "hints") {
        let counts = gdb
            .query_analytics_hint_counts(scope.filter.as_deref(), since)
            .await
            .map_err(config_error)?;
        let dashboard_counts = counts
            .iter()
            .map(
                |count| crate::dashboard::analytics_api::DashboardHintCount {
                    category: count.category.clone(),
                    emitted: count.emitted,
                    followed: count.followed,
                    ignored: count.ignored,
                    suppressed: count.suppressed,
                },
            )
            .collect::<Vec<_>>();
        let hints = crate::dashboard::analytics_api::hint_summary_from_counts(&dashboard_counts);
        if let Some(object) = value.as_object_mut() {
            object.insert("hints".to_string(), hints);
        }
    }
    if wants_section(section, "facts") {
        let facts = facts_section(cg, &args, global_db, allow_default_registry_fallback).await;
        if let Some(object) = value.as_object_mut() {
            object.insert("facts".to_string(), facts);
        }
    }
    if wants_section(section, "automation") {
        let automation = automation_section(&scope.root, since).await;
        if let Some(object) = value.as_object_mut() {
            object.insert("automation".to_string(), automation);
        }
    }

    Ok(tool_json_with_md(Some(&scope.root), &args, &value, || {
        renderers::analytics_md(&value)
    }))
}

fn tools_section(rows: &[AnalyticsToolCounts]) -> Value {
    let mut per_tool: BTreeMap<String, ToolCallCounts> = BTreeMap::new();
    for row in rows {
        let counts = per_tool.entry(row.tool_name.clone()).or_default();
        counts.calls += row.calls;
        counts.errors += row.errors;
    }

    let mut per_tier: BTreeMap<&'static str, ToolCallCounts> = BTreeMap::new();
    for (tool_name, counts) in &per_tool {
        let tier = per_tier.entry(tool_tier(tool_name)).or_default();
        tier.calls += counts.calls;
        tier.errors += counts.errors;
    }
    let tiers: Vec<Value> = TIERS
        .iter()
        .map(|(tier, _)| *tier)
        .chain(std::iter::once("other"))
        .filter_map(|tier| {
            per_tier.get(tier).map(
                |counts| json!({ "tier": tier, "calls": counts.calls, "errors": counts.errors }),
            )
        })
        .collect();

    let mut top_tools: Vec<(&String, &ToolCallCounts)> = per_tool.iter().collect();
    top_tools.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then_with(|| a.0.cmp(b.0)));
    let top_tools: Vec<Value> = top_tools
        .into_iter()
        .take(TOP_TOOLS_LIMIT)
        .map(|(tool_name, counts)| {
            json!({
                "tool_name": tool_name,
                "tier": tool_tier(tool_name),
                "calls": counts.calls,
                "errors": counts.errors,
            })
        })
        .collect();

    let defined: Vec<String> = crate::mcp::tools::get_tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    let mut zero_call: Vec<&String> = defined
        .iter()
        .filter(|name| !per_tool.contains_key(*name))
        .collect();
    zero_call.sort();
    let zero_call_count = zero_call.len();
    let zero_call_sample: Vec<&String> =
        zero_call.into_iter().take(ZERO_CALL_SAMPLE_LIMIT).collect();

    json!({
        "available": !rows.is_empty(),
        "tiers": tiers,
        "top_tools": top_tools,
        "distinct_tools_called": per_tool.len(),
        "defined_tool_count": defined.len(),
        "zero_call_tools": {
            "count": zero_call_count,
            "sample": zero_call_sample,
            "sample_truncated": zero_call_count > zero_call_sample.len(),
        },
    })
}

async fn facts_section(
    cg: &TraceDecay,
    args: &Value,
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Value {
    let target =
        match open_target_memory_db(cg, args, global_db, allow_default_registry_fallback).await {
            Ok(target) => target,
            Err(err) => {
                return json!({
                    "available": false,
                    "reason": err.to_string(),
                });
            }
        };
    let query = target
        .conn()
        .query(
            "SELECT COUNT(*),
                    COALESCE(SUM(retrieval_count), 0),
                    COALESCE(SUM(CASE WHEN retrieval_count > 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(helpful_count), 0),
                    COALESCE(SUM(unhelpful_count), 0),
                    COALESCE(SUM(CASE WHEN helpful_count > 0 OR unhelpful_count > 0 THEN 1 ELSE 0 END), 0)
             FROM memory_facts",
            (),
        )
        .await;
    let mut rows = match query {
        Ok(rows) => rows,
        Err(err) => {
            return json!({
                "available": false,
                "reason": format!("fact-store funnel query failed: {err}"),
                "project_root": target.project_root.display().to_string(),
            });
        }
    };
    let Ok(Some(row)) = rows.next().await else {
        return json!({
            "available": false,
            "reason": "fact-store funnel query returned no rows",
            "project_root": target.project_root.display().to_string(),
        });
    };
    let get_i64 = |index: i32| row.get::<i64>(index).unwrap_or(0);
    json!({
        "available": true,
        "project_root": target.project_root.display().to_string(),
        "facts": get_i64(0),
        "retrievals": get_i64(1),
        "facts_retrieved": get_i64(2),
        "helpful_feedback": get_i64(3),
        "unhelpful_feedback": get_i64(4),
        "facts_rated": get_i64(5),
    })
}

async fn automation_section(project_root: &Path, since: i64) -> Value {
    let dashboard_root = match crate::storage::resolve_layout_for_current_profile(project_root) {
        Ok(layout) => layout.dashboard_root,
        Err(err) => {
            return json!({
                "available": false,
                "reason": format!("could not resolve automation dashboard root: {err}"),
            });
        }
    };
    let records = match load_run_records(&dashboard_root, AUTOMATION_RECORD_LIMIT).await {
        Ok(records) => records,
        Err(err) => {
            return json!({
                "available": false,
                "reason": format!("could not read automation run ledger: {err}"),
                "dashboard_root": dashboard_root.display().to_string(),
            });
        }
    };

    let mut in_window = 0usize;
    let mut by_job: BTreeMap<String, BTreeMap<&'static str, i64>> = BTreeMap::new();
    for record in &records {
        if let Some(started_at) = parse_rfc3339_timestamp(&record.started_at) {
            if started_at < since {
                continue;
            }
        }
        in_window += 1;
        let job = serde_json::to_value(record.task)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        *by_job
            .entry(job)
            .or_default()
            .entry(record.status.as_str())
            .or_default() += 1;
    }

    let by_job: Vec<Value> = by_job
        .into_iter()
        .map(|(job, statuses)| {
            let succeeded = statuses.get("succeeded").copied().unwrap_or(0);
            let failed = statuses.get("failed").copied().unwrap_or(0);
            let skipped = statuses.get("skipped").copied().unwrap_or(0);
            let mut other = 0i64;
            for (status, count) in &statuses {
                if !matches!(*status, "succeeded" | "failed" | "skipped") {
                    other += *count;
                }
            }
            json!({
                "job": job,
                "succeeded": succeeded,
                "failed": failed,
                "skipped": skipped,
                "other": other,
            })
        })
        .collect();

    json!({
        "available": true,
        "dashboard_root": dashboard_root.display().to_string(),
        "records_considered": records.len(),
        "records_in_window": in_window,
        "records_truncated": records.len() >= AUTOMATION_RECORD_LIMIT,
        "by_job": by_job,
    })
}

//! Status, files, `type_hierarchy`, body, todos, `simplify_scan`, `port_status`,
//! `port_order` tool handlers.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use crate::context::read_modes::{LineRange, ReadMode};
use crate::context::source_read::{SourceReadRequest, read_source, resolve_indexed_source_file};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{RegisteredGlobalDb, SessionIngestHealth};
use crate::path_tree::format_compact_annotated_path_list;
use crate::project_registry::{ProjectRegistryView, render_project_registry_view};
use crate::storage::{ProjectPath, StorageMode, StoreKind};
use crate::tracedecay::{BranchDiagnostics, TraceDecay};
use crate::types::{NodeKind, Visibility};

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::dependency_hints;
use super::project_registry::{
    ProjectRegistryContextCommand, ProjectRegistryContextOutcome, ProjectRegistryListingCommand,
    ProjectRegistryListingOutcome, ProjectRegistryListingScope, ProjectRegistryReadPort,
    ProjectRegistrySelector, list_registered_projects, read_registered_project_context,
};
use super::support::{
    effective_path, filter_by_scope, is_explicit_project_path_selector, require_node_id,
    unique_file_paths,
};

/// Daemon-only sync entry point used by the first-party CLI. It is deliberately
/// not advertised in the MCP catalog: external agents should rely on the
/// daemon watcher while the CLI can request an explicit serialized refresh.
pub(super) async fn handle_admin_sync(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let output = if force {
        let result = cg.index_all().await?;
        json!({
            "mode": "full",
            "files": result.file_count,
            "nodes": result.node_count,
            "edges": result.edge_count,
            "duration_ms": result.duration_ms,
        })
    } else {
        let result = cg.sync().await?;
        json!({
            "mode": "incremental",
            "files_added": result.files_added,
            "files_modified": result.files_modified,
            "files_removed": result.files_removed,
            "duration_ms": result.duration_ms,
        })
    };
    Ok(ToolResult::new(
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&output).unwrap_or_default(),
            }]
        }),
        Vec::new(),
    ))
}

fn status_arg_flag(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn attach_compact_branch_summary(cg: &TraceDecay, output: &mut Value) {
    // Avoid `branch_diagnostics()` — it walks tracked-branch metadata, git
    // ancestry, and per-branch filesystem stats. Compact CLI status only needs
    // the already-resolved serving identity retained on TraceDecay.
    // Do not alias open/active into current/live: those are distinct under drift.
    if let Some(active) = cg.active_branch() {
        output["active_branch"] = json!(active);
    }
    if let Some(serving) = cg.serving_branch() {
        output["serving_branch"] = json!(serving);
    }
    if let Some(warning) = cg.fallback_warning() {
        output["branch_fallback"] = json!(true);
        output["branch_warning"] = json!(warning);
    }
}

fn attach_full_branch_status(cg: &TraceDecay, args: &Value, output: &mut Value) {
    let branch_diagnostics = cg.branch_diagnostics();
    if let Some(open_branch) = branch_diagnostics.open_active_branch.as_deref() {
        output["active_branch"] = json!(open_branch);
    }
    if let Some(current_branch) = branch_diagnostics.current_branch.as_deref() {
        output["current_branch"] = json!(current_branch);
        output["live_branch"] = json!(current_branch);
    }
    if let Some(serving_branch) = branch_diagnostics.serving_branch.as_deref() {
        output["serving_branch"] = json!(serving_branch);
    }
    if let Some(parent) = branch_diagnostics
        .branches
        .iter()
        .find(|entry| entry.is_serving)
        .and_then(|entry| entry.parent.as_deref())
    {
        output["parent_branch"] = json!(parent);
    }
    output["branch_drifted"] = json!(branch_diagnostics.branch_drifted);
    output["branch_resolution"] = json!(branch_diagnostics.branch_resolution.clone());
    output["tracked_branch_count"] = json!(branch_diagnostics.tracked_branch_count);
    output["serving_db_path"] = json!(branch_diagnostics.serving_db_path);
    output["serving_db_exists"] = json!(branch_diagnostics.serving_db_exists);
    if status_arg_flag(args, "include_branch_diagnostics", true) {
        output["branch_diagnostics"] =
            serde_json::to_value(&branch_diagnostics).unwrap_or(json!({}));
    }
    if branch_diagnostics.branch_drifted {
        output["branch_mismatch"] = json!({
            "git_branch": branch_diagnostics.current_branch,
            "indexed_branch": branch_diagnostics.open_active_branch,
            "serving_branch": branch_diagnostics.serving_branch,
        });
    }
    if branch_diagnostics.is_fallback {
        output["branch_fallback"] = json!(true);
        if let Some(target) = branch_diagnostics.fallback_target.as_deref() {
            output["branch_fallback_target"] = json!(target);
        }
        if let Some(warning) = branch_diagnostics.fallback_warning.as_deref() {
            output["branch_warning"] = json!(warning);
        }
    }
    if !branch_diagnostics.warnings.is_empty() {
        output["branch_warnings"] = json!(branch_diagnostics.warnings);
    }
}

/// Handles `tracedecay_status` tool calls.
pub(super) async fn handle_status(
    cg: &TraceDecay,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
    project_session_db: Option<&RegisteredGlobalDb>,
) -> Result<ToolResult> {
    if status_arg_flag(&args, "admission_only", false) {
        let mut output = json!({
            "project_admitted": true,
            "project_root": cg.project_root(),
        });
        if let Some(ss) = server_stats {
            output["server"] = ss;
        }
        if let Some(prefix) = scope_prefix {
            output["scope_prefix"] = json!(prefix);
        }
        let text = render::finalize(Some(cg.project_root()), &args, &output, || {
            render::generic_md(&output)
        });
        return Ok(ToolResult::new(
            json!({
                "content": [{ "type": "text", "text": text }]
            }),
            vec![],
        ));
    }

    let include_branch_diagnostics = status_arg_flag(&args, "include_branch_diagnostics", true);
    let include_storage_health = status_arg_flag(&args, "include_storage_health", true);
    let include_session_ingest = status_arg_flag(&args, "include_session_ingest", true);
    let include_staleness = status_arg_flag(&args, "include_staleness", true);

    let stats = cg.get_stats().await?;
    let mut output: Value = serde_json::to_value(&stats).unwrap_or(json!({}));
    let migration_reindex = cg.migration_reindex_status().await?;
    if !matches!(
        &migration_reindex,
        crate::tracedecay::MigrationReindexStatusV1::Current { .. }
    ) {
        output["migration_reindex"] =
            serde_json::to_value(&migration_reindex).unwrap_or_else(|error| {
                json!({
                    "state": "failed",
                    "reason": format!("could not serialize migration re-index state: {error}"),
                })
            });
        output["migration_reindex_warning"] =
            json!("graph counts are not authoritative while the migration re-index is pending");
    }
    if include_storage_health {
        let mut storage_health =
            serde_json::to_value(crate::runtime_telemetry::collect_database(cg, false).await?)
                .unwrap_or_else(|_| json!({}));
        if server_stats.is_some() {
            storage_health["daemon_owner_pid"] = json!(std::process::id());
            storage_health["daemon_generation"] = json!(crate::runtime_identity::process_run_id());
        }
        output["storage_health"] = storage_health;
    }
    if let Some(ss) = server_stats {
        output["server"] = ss;
    }

    if include_branch_diagnostics {
        attach_full_branch_status(cg, &args, &mut output);
    } else {
        attach_compact_branch_summary(cg, &mut output);
    }

    // Session-transcript ingest health (recall trust): last ingest time and
    // any un-ingested transcript backlog from the project sessions.db.
    if include_session_ingest {
        let session_db_path = cg.store_layout().sessions_db_path.clone();
        if session_db_path.exists() {
            match project_session_db {
                None => {
                    // The store exists but the daemon did not retain its authority;
                    // fail closed instead of opening a second connection here.
                    output["session_ingest"] = json!({
                        "status": "unavailable",
                        "reason": "session_store_unavailable",
                        "message": "daemon project session authority is unavailable",
                    });
                }
                Some(db) => match db.cursor_session_ingest_health().await {
                    Ok(ingest) => {
                        output["session_ingest"] =
                            serde_json::to_value(&ingest).unwrap_or_else(|error| {
                                json!({
                                    "status": "unavailable",
                                    "reason": "session_ingest_serialization_failed",
                                    "message": error.to_string(),
                                })
                            });
                        // `session_ingest` stays cursor-scoped so it keeps matching the
                        // doctor-owned `cursor_session_ingest` signal, but a stalled
                        // backlog on any provider still starves recall, so the warning
                        // is measured across every tracked transcript.
                        if let Some(warning) =
                            stalled_session_ingest_warning(db, cg.project_root()).await
                        {
                            output["session_ingest_warning"] = json!(warning);
                        }
                    }
                    Err(error) => {
                        output["session_ingest"] = json!({
                            "status": "unavailable",
                            "reason": "session_ingest_query_failed",
                            "message": error,
                        });
                    }
                },
            }
        }
    }

    if include_staleness {
        // Git commit staleness: count commits since last index
        let stale_commit_count = cg.git_commits_since(stats.last_updated as i64);
        if stale_commit_count > 0 {
            output["stale_commits"] = json!(stale_commit_count);
            output["stale_warning"] = json!(format!(
                "{} commit(s) since last sync. Run `tracedecay sync` to update the index.",
                stale_commit_count
            ));
        }

        // File-level staleness summary (sample up to 100 files for efficiency).
        // A store failure here must surface as a tool error, not as "no stale
        // files" — silently dropping the staleness section makes a broken store
        // look healthy.
        let all_files = cg.get_all_files().await?;
        let sample_paths: Vec<String> =
            all_files.iter().take(100).map(|f| f.path.clone()).collect();
        let stale_files = cg.check_file_staleness(&sample_paths).await;
        if !stale_files.is_empty() {
            output["stale_files"] = json!(stale_files.len());
        }
    }

    if let Some(prefix) = scope_prefix {
        output["scope_prefix"] = json!(prefix);
    }

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render_status_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

/// Warns when any provider's transcript backlog has outgrown what automatic
/// catch-up drains, so recall gaps are reported instead of read as healthy.
async fn stalled_session_ingest_warning(
    db: &RegisteredGlobalDb,
    project_root: &Path,
) -> Option<String> {
    const THRESHOLD: u64 = crate::sessions::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;
    match db.session_ingest_health_for_provider(None).await {
        Ok(ingest) => (ingest.max_transcript_pending_bytes > THRESHOLD)
            .then(|| session_ingest_warning(&ingest, project_root)),
        Err(error) => Some(format!(
            "session transcript ingest backlog could not be measured across providers: {error}"
        )),
    }
}

fn session_ingest_warning(ingest: &SessionIngestHealth, project_root: &Path) -> String {
    format!(
        "session transcript ingest looks stalled: a transcript has {} \
         un-ingested bytes ({} total across {} transcript(s)), exceeding \
         the automatic catch-up warning threshold — session recall is missing \
         those turns. Run `tracedecay sessions ingest --project-path {}` \
         to drain the backlog manually.",
        ingest.max_transcript_pending_bytes,
        ingest.pending_bytes,
        ingest.pending_transcripts,
        project_root.display()
    )
}

fn render_status_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Project Status");
    if let Some(obj) = value.as_object() {
        let mut warnings: Vec<String> = Vec::new();
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for k in keys {
            let v = &obj[k];
            if k.contains("warning")
                && let Some(s) = v.as_str()
            {
                warnings.push(s.to_string());
                continue;
            }
            match v {
                Value::String(s) => {
                    md.field(k, s);
                }
                Value::Number(n) => {
                    md.field(k, &n.to_string());
                }
                Value::Bool(b) => {
                    md.field(k, &b.to_string());
                }
                Value::Array(a) => {
                    md.field(k, &format!("{} item(s)", a.len()));
                }
                Value::Object(o) => {
                    md.field(k, &format!("{{{} field(s)}}", o.len()));
                }
                Value::Null => {}
            }
        }
        if !warnings.is_empty() {
            md.blank().heading(3, "Warnings");
            for w in &warnings {
                md.bullet(w);
            }
        }
    }
    md.render()
}

fn display_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

fn active_project_context(
    cg: &TraceDecay,
    branch: &BranchDiagnostics,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
) -> Value {
    let project_root = cg.project_root();
    let layout = cg.store_layout();
    let graph_db_path = cg.db_path();
    let mut output = json!({
        "project_root": display_path(project_root),
        "resolution_source": "active_project",
        "storage": {
            "class": store_kind_name(&layout.store_kind),
            "mode": storage_mode_name(&layout.storage_mode),
            "data_root": display_path(&layout.data_root),
            "config_path": display_path(&layout.config_path),
            "graph_db_path": display_path(&graph_db_path),
            "graph_db_exists": graph_db_path.exists(),
            "graph_db_size_bytes": graph_db_path.metadata().map_or(0, |metadata| metadata.len()),
            "sessions_db_path": display_path(&layout.sessions_db_path),
            "response_handle_root": display_path(&layout.response_handle_root),
            "lcm_payload_root": display_path(&layout.lcm_payload_root),
        },
        "branch": {
            "current_branch": branch.current_branch.clone(),
            "open_active_branch": branch.open_active_branch.clone(),
            "serving_branch": branch.serving_branch.clone(),
            "serving_db_path": display_path(&branch.serving_db_path),
            "serving_db_exists": branch.serving_db_exists,
            "branch_resolution": branch.branch_resolution.clone(),
            "branch_drifted": branch.branch_drifted,
            "is_fallback": branch.is_fallback,
            "fallback_target": branch.fallback_target.clone(),
            "fallback_warning": branch.fallback_warning.clone(),
            "tracked_branch_count": branch.tracked_branch_count,
            "warnings": branch.warnings.clone(),
        }
    });
    if let Some(prefix) = scope_prefix {
        output["scope_prefix"] = json!(prefix);
    }
    if let Some(stats) = server_stats {
        output["server"] = stats;
    }
    output
}

fn storage_mode_name(mode: &StorageMode) -> &'static str {
    match mode {
        StorageMode::ProjectLocal => "project_local",
        StorageMode::ProfileSharded => "profile_sharded",
    }
}

fn store_kind_name(kind: &StoreKind) -> &'static str {
    match kind {
        StoreKind::CodeProject => "code_project",
    }
}

/// Handles `tracedecay_active_project` tool calls.
pub(super) fn handle_active_project(
    cg: &TraceDecay,
    args: &Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
) -> ToolResult {
    let branch = cg.branch_diagnostics();
    let output = active_project_context(cg, &branch, server_stats, scope_prefix);
    let text = render::finalize(Some(cg.project_root()), args, &output, || {
        render::generic_md(&output)
    });
    ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    )
}

fn bounded_limit(args: &Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .map_or(default, |value| value.clamp(1, max))
}

fn project_registry_result(cg: &TraceDecay, args: &Value, payload: &Value) -> ToolResult {
    render_registry_result(Some(cg.project_root()), args, payload)
}

fn registry_result(args: &Value, payload: &Value) -> ToolResult {
    render_registry_result(None, args, payload)
}

fn render_registry_result(root: Option<&Path>, args: &Value, payload: &Value) -> ToolResult {
    let text = render::finalize(root, args, payload, || {
        if payload.get("project_tree").is_some() {
            let view = serde_json::from_value::<ProjectRegistryView>(json!({
                "summary": payload.get("summary").cloned().unwrap_or_else(|| json!({})),
                "project_tree": payload.get("project_tree").cloned().unwrap_or_else(|| json!([])),
            }));
            if let Ok(view) = view {
                let title = payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("registered projects");
                return render_project_registry_view(title, &view);
            }
        }
        render::generic_md(payload)
    });
    ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    )
}

fn registry_missing_payload() -> Value {
    json!({
        "status": "not_found",
        "message": "project registry is not present for this profile",
        "projects": [],
    })
}

/// Zeroed summary/tree keys for the missing-registry branch, mirroring the
/// ok-shape's `summary`/`project_tree` so callers get a stable payload shape
/// regardless of whether the registry is present (see
/// `src/dashboard/projects.rs`'s missing-registry branch).
fn empty_registry_view_payload(title: &str) -> (Value, Value, Value) {
    (
        json!(title),
        json!({
            "project_count": 0,
            "repo_count": 0,
            "truncated": false,
        }),
        json!([]),
    )
}

/// Handles `tracedecay_project_list` tool calls.
pub(super) async fn handle_project_list(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<ToolResult> {
    let limit = bounded_limit(&args, 25, 100);
    let outcome = list_registered_projects(
        registry,
        ProjectRegistryListingCommand {
            active_project_root: cg.project_root().to_path_buf(),
            scope: ProjectRegistryListingScope::All,
            limit,
        },
    )
    .await?;
    Ok(registry_listing_result(
        &args,
        "registered projects",
        None,
        limit,
        outcome,
    ))
}

/// Handles `tracedecay_project_search` tool calls.
pub(super) async fn handle_project_search(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: query".to_string(),
        })?
        .to_owned();
    let limit = bounded_limit(&args, 10, 50);
    let outcome = list_registered_projects(
        registry,
        ProjectRegistryListingCommand {
            active_project_root: cg.project_root().to_path_buf(),
            scope: ProjectRegistryListingScope::Matching {
                query: query.clone(),
            },
            limit,
        },
    )
    .await?;
    Ok(registry_listing_result(
        &args,
        &format!("projects matching \"{query}\""),
        Some(&query),
        limit,
        outcome,
    ))
}

/// Renders a listing outcome, keeping the missing-registry state a stable
/// `not_found` payload with the same summary/tree keys as the `ok` shape.
fn registry_listing_result(
    args: &Value,
    title: &str,
    query: Option<&str>,
    limit: usize,
    outcome: ProjectRegistryListingOutcome,
) -> ToolResult {
    match outcome {
        ProjectRegistryListingOutcome::RegistryUnavailable => {
            let mut payload = registry_missing_payload();
            let (title, summary, project_tree) = empty_registry_view_payload(title);
            payload["title"] = title;
            payload["summary"] = summary;
            payload["project_tree"] = project_tree;
            if let Some(query) = query {
                payload["query"] = json!(query);
            }
            payload["limit"] = json!(limit);
            payload["truncated"] = json!(false);
            registry_result(args, &payload)
        }
        ProjectRegistryListingOutcome::Listing(listing) => {
            let mut payload = json!({
                "status": "ok",
                "title": title,
                "registry_path": display_path(&listing.registry_path),
                "limit": limit,
                "truncated": listing.truncated,
                "summary": listing.view.summary,
                "project_tree": listing.view.project_tree,
                "projects": listing.projects,
            });
            if let Some(query) = query {
                payload["query"] = json!(query);
            }
            registry_result(args, &payload)
        }
    }
}

fn project_context_selector(cg: &TraceDecay, args: &Value) -> ProjectRegistrySelector {
    if let Some(project_id) = args.get("project_id").and_then(Value::as_str) {
        return ProjectRegistrySelector::ProjectId(project_id.to_owned());
    }
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return ProjectRegistrySelector::Path {
            path: cg.project_root().to_path_buf(),
            allow_git_identity: true,
        };
    };
    let path = Path::new(path);
    let allow_git_identity =
        path.is_absolute() && is_explicit_project_path_selector(path.to_string_lossy().as_ref());
    ProjectRegistrySelector::Path {
        path: path.to_path_buf(),
        allow_git_identity,
    }
}

/// Handles `tracedecay_project_context` tool calls.
pub(super) async fn handle_project_context(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&dyn ProjectRegistryReadPort>,
) -> Result<ToolResult> {
    let outcome = read_registered_project_context(
        registry,
        ProjectRegistryContextCommand {
            active_project_root: cg.project_root().to_path_buf(),
            selector: project_context_selector(cg, &args),
        },
    )
    .await?;
    let payload = match outcome {
        ProjectRegistryContextOutcome::RegistryUnavailable => registry_missing_payload(),
        ProjectRegistryContextOutcome::NotFound { registry_path } => json!({
            "status": "not_found",
            "registry_path": display_path(&registry_path),
            "project": null,
            "aliases": [],
            "stores": [],
        }),
        ProjectRegistryContextOutcome::Context(context) => json!({
            "status": "ok",
            "is_active": context.is_active,
            "registry_path": display_path(&context.registry_path),
            "project": context.project,
            "aliases": context.aliases,
            "stores": context.stores,
        }),
    };
    Ok(project_registry_result(cg, &args, &payload))
}

/// Handles `tracedecay_files` tool calls.
pub(super) async fn handle_files(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    debug_assert!(args.is_object(), "handle_files expects an object argument");
    let mut files = cg.get_all_files().await?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    // Apply directory prefix filter
    if let Some(dir) = effective_path(&args, scope_prefix) {
        let prefix = if dir.ends_with('/') {
            dir.to_string()
        } else {
            format!("{dir}/")
        };
        files.retain(|f| f.path.starts_with(&prefix) || f.path == dir);
    }

    // Apply glob pattern filter
    if let Some(pat) = args.get("pattern").and_then(|v| v.as_str())
        && let Ok(glob) = glob::Pattern::new(pat)
    {
        files.retain(|f| glob.matches(&f.path));
    }

    // Listing files is metadata-only — no source code is served, so no tokens saved.
    let touched_files = vec![];

    let layout = args
        .get("layout")
        .and_then(|v| v.as_str())
        .unwrap_or("grouped");

    let file_values: Vec<Value> = files
        .iter()
        .map(|f| json!({ "path": f.path, "symbols": f.node_count, "bytes": f.size }))
        .collect();
    let payload = json!({
        "count": files.len(),
        "layout": layout,
        "files": file_values,
    });
    let text = render::finalize(Some(cg.project_root()), &args, &payload, || {
        render_files_md(&payload)
    });

    Ok(ToolResult::new(
        json!({ "content": [{ "type": "text", "text": text }] }),
        touched_files,
    ))
}

fn render_files_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Files");
    md.field(
        "indexed files",
        &render::field_i64(value, "count").to_string(),
    );
    let layout = render::field_str(value, "layout");
    md.field("layout", layout);

    let files = value
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        md.blank().empty_note("No indexed files matched.");
        return md.render();
    }

    if layout == "flat" {
        let lines = files
            .iter()
            .filter_map(|file| {
                let path = file.get("path").and_then(Value::as_str)?;
                let symbols = render::field_i64(file, "symbols");
                let bytes = render::field_i64(file, "bytes");
                Some(format!("- {path} ({symbols} symbols, {bytes} bytes)"))
            })
            .collect::<Vec<_>>();
        let listing = lines.join("\n");
        md.blank().code("text", &listing);
        return md.render();
    }

    let paths = files
        .iter()
        .filter_map(|file| {
            file.get("path")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    let suffixes = files
        .iter()
        .map(|file| format!(" ({} symbols)", render::field_i64(file, "symbols")))
        .collect::<Vec<_>>();
    let annotated = paths
        .iter()
        .zip(suffixes.iter())
        .map(|(path, suffix)| (path.as_str(), suffix.as_str()));
    let listing = format_compact_annotated_path_list(annotated, "- ", "");
    md.blank().code("text", &listing);
    md.render()
}

/// Default node kinds for port comparisons.
const PORT_DEFAULT_KINDS: &[&str] = &[
    "function",
    "method",
    "class",
    "struct",
    "interface",
    "trait",
    "enum",
    "module",
];

/// Returns the compatibility group for a node kind string used in port matching.
///
/// Kinds in the same group are considered cross-language equivalents:
/// - group 0: class, struct (cross-language data type)
/// - group 1: function
/// - group 2: method
/// - group 3: interface, trait
/// - group 4: enum
/// - group 5: module
fn kind_compat_group(kind: &str) -> u8 {
    match kind {
        "class" | "struct" => 0,
        "function" => 1,
        "method" => 2,
        "interface" | "trait" => 3,
        "enum" => 4,
        "module" => 5,
        _ => 255,
    }
}

/// Composite match key used by `handle_port_status`.
///
/// Combines the lowercased name, an optional parent qualifier (for methods,
/// fields, and variants), and a kind compatibility group, so siblings whose
/// names happen to collide (`Biquad::new` vs `Adaa::new`) do not cross-match.
type PortKey = (String, Option<String>, u8);

/// Returns true for kinds that conceptually have a parent type/owner whose
/// identity matters for matching (methods, fields, variants, etc.). Top-level
/// items (struct, function, …) return false — their parent in `qualified_name`
/// is just the file path and is not useful for cross-port matching.
fn port_kind_has_parent(kind: &str) -> bool {
    matches!(
        kind,
        "method"
            | "field"
            | "enum_variant"
            | "struct_method"
            | "abstract_method"
            | "constructor"
            | "csharp_property"
            | "property"
            | "val"
            | "var"
    )
}

/// Extracts the parent qualifier from a node's `qualified_name`, stripping
/// generic parameters so `Biquad<T>::new` and `Biquad::new` share the same
/// parent. Returns `None` for kinds where the parent qualifier is not the
/// containing type (e.g. top-level structs whose parent is the file path).
fn port_parent_qualifier(node: &crate::types::Node) -> Option<String> {
    if !port_kind_has_parent(node.kind.as_str()) {
        return None;
    }
    let parts: Vec<&str> = node.qualified_name.split("::").collect();
    if parts.len() < 2 {
        return None;
    }
    let parent = parts[parts.len() - 2];
    // Strip generic parameters: `Biquad<T>` -> `Biquad`.
    let parent_no_generics = parent.split('<').next().unwrap_or(parent);
    Some(parent_no_generics.trim().to_string())
}

/// Handles `tracedecay_port_status` tool calls.
pub(super) async fn handle_port_status(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    debug_assert!(
        args.is_object(),
        "handle_port_status expects an object argument"
    );

    let source_dir = args
        .get("source_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: source_dir".to_string(),
        })?;

    let target_dir = args
        .get("target_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: target_dir".to_string(),
        })?;

    let kind_strs: Vec<String> = args.get("kinds").and_then(|v| v.as_array()).map_or_else(
        || {
            PORT_DEFAULT_KINDS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        },
        |arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                .collect()
        },
    );

    let kinds: Vec<NodeKind> = kind_strs
        .iter()
        .filter_map(|s| NodeKind::from_str(s))
        .collect();

    if kinds.is_empty() {
        return Ok(ToolResult::new(
            json!({
                "content": [{ "type": "text", "text": "No valid node kinds specified." }]
            }),
            vec![],
        ));
    }

    let source_nodes = cg.get_nodes_by_dir(source_dir, &kinds).await?;
    let target_nodes = cg.get_nodes_by_dir(target_dir, &kinds).await?;

    // Match key includes the parent qualifier (e.g. enclosing struct/class) for
    // kinds that have one, so `Biquad::new` does NOT collide with `Adaa::new`.
    // Top-level kinds (struct, function, …) keep using name-only matching.
    let mut target_map: HashMap<PortKey, Vec<&crate::types::Node>> = HashMap::new();
    for node in &target_nodes {
        let key: PortKey = (
            node.name.to_lowercase(),
            port_parent_qualifier(node).map(|s| s.to_lowercase()),
            kind_compat_group(node.kind.as_str()),
        );
        target_map.entry(key).or_default().push(node);
    }

    let mut matched_symbols: Vec<Value> = Vec::new();
    let mut matched_target_ids: HashSet<String> = HashSet::new();
    let mut unmatched_by_file: HashMap<String, Vec<Value>> = HashMap::new();

    for src_node in &source_nodes {
        let key: PortKey = (
            src_node.name.to_lowercase(),
            port_parent_qualifier(src_node).map(|s| s.to_lowercase()),
            kind_compat_group(src_node.kind.as_str()),
        );
        if let Some(targets) = target_map.get(&key) {
            // Take the first match
            let tgt = targets[0];
            matched_symbols.push(json!({
                "name": src_node.name,
                "source_kind": src_node.kind.as_str(),
                "target_kind": tgt.kind.as_str(),
                "source_file": src_node.file_path,
                "target_file": tgt.file_path,
            }));
            matched_target_ids.insert(tgt.id.clone());
        } else {
            unmatched_by_file
                .entry(src_node.file_path.clone())
                .or_default()
                .push(json!({
                    "name": src_node.name,
                    "kind": src_node.kind.as_str(),
                    "line": src_node.start_line,
                }));
        }
    }

    // Target-only symbols (in target but no source match)
    let target_only: Vec<Value> = target_nodes
        .iter()
        .filter(|n| !matched_target_ids.contains(&n.id))
        .map(|n| {
            json!({
                "name": n.name,
                "kind": n.kind.as_str(),
                "file": n.file_path,
                "line": n.start_line,
            })
        })
        .collect();

    let source_count = source_nodes.len();
    let matched_count = matched_symbols.len();
    let unmatched_count = source_count - matched_count;
    let coverage = if source_count > 0 {
        (matched_count as f64 / source_count as f64) * 100.0
    } else {
        0.0
    };

    let touched_files = unique_file_paths(
        source_nodes
            .iter()
            .chain(target_nodes.iter())
            .map(|n| n.file_path.as_str()),
    );

    let result = json!({
        "source_dir": source_dir,
        "target_dir": target_dir,
        "source_count": source_count,
        "target_count": target_nodes.len(),
        "matched": matched_count,
        "unmatched": unmatched_count,
        "target_only": target_only.len(),
        "coverage_percent": (coverage * 10.0).round() / 10.0,
        "unmatched_by_file": unmatched_by_file,
        "matched_symbols": matched_symbols,
        "target_only_symbols": target_only,
    });

    let text = render::finalize(Some(cg.project_root()), &args, &result, || {
        render::generic_md(&result)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched_files,
    ))
}

/// Handles `tracedecay_port_order` tool calls.
pub(super) async fn handle_port_order(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    debug_assert!(
        args.is_object(),
        "handle_port_order expects an object argument"
    );

    let source_dir = args
        .get("source_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: source_dir".to_string(),
        })?;

    let kind_strs: Vec<String> = args.get("kinds").and_then(|v| v.as_array()).map_or_else(
        || {
            PORT_DEFAULT_KINDS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        },
        |arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                .collect()
        },
    );

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(50, |v| v.min(500) as usize);

    let kinds: Vec<NodeKind> = kind_strs
        .iter()
        .filter_map(|s| NodeKind::from_str(s))
        .collect();

    if kinds.is_empty() {
        return Ok(ToolResult::new(
            json!({
                "content": [{ "type": "text", "text": "No valid node kinds specified." }]
            }),
            vec![],
        ));
    }

    let nodes = cg.get_nodes_by_dir(source_dir, &kinds).await?;
    let total_symbols = nodes.len();

    if nodes.is_empty() {
        let result = json!({
            "source_dir": source_dir,
            "total_symbols": 0,
            "returned": 0,
            "levels": [],
            "cycles": [],
        });
        let text = render::finalize(Some(cg.project_root()), &args, &result, || {
            render::generic_md(&result)
        });
        return Ok(ToolResult::new(
            json!({
                "content": [{ "type": "text", "text": text }]
            }),
            vec![],
        ));
    }

    let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let node_map: HashMap<&str, &crate::types::Node> =
        nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let id_set: HashSet<&str> = node_ids.iter().map(std::string::String::as_str).collect();

    let edges = cg.get_internal_edges(&node_ids).await?;

    // Build adjacency list and in-degree map for Kahn's algorithm.
    // Edge direction: source depends on target (source calls/uses target),
    // so in the dependency graph, source -> target means "source needs target".
    // For topological sort, we want nodes with in_degree 0 (nothing depends on
    // them internally, OR they have no dependencies). Actually, for porting
    // order we want leaves first = nodes that DON'T depend on other internal
    // nodes. So in-degree in the dependency DAG = number of things this node
    // depends on = outgoing edges in the call/uses graph.
    //
    // Reframe: dependency_graph[A] = {B, C} means A depends on B and C.
    // in_degree[A] = number of nodes A depends on.
    // Kahn's starts with in_degree 0 = nodes with no dependencies = safe to port first.
    let dep_edge_kinds: HashSet<&str> = ["calls", "uses", "extends", "implements"]
        .iter()
        .copied()
        .collect();

    let mut dep_graph: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    // Initialize all nodes
    for id in &node_ids {
        dep_graph.entry(id.as_str()).or_default();
        in_degree.entry(id.as_str()).or_insert(0);
    }

    // reverse_dep_graph[B] = list of nodes that depend on B.
    // When B is sorted, we decrement in_degree for each of its reverse deps.
    let mut reverse_dep_graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in &node_ids {
        reverse_dep_graph.entry(id.as_str()).or_default();
    }

    for edge in &edges {
        if !dep_edge_kinds.contains(edge.kind.as_str()) {
            continue;
        }
        if !id_set.contains(edge.source.as_str()) || !id_set.contains(edge.target.as_str()) {
            continue;
        }
        // Self-edges are common resolver artifacts for methods with generic
        // names (`push`, `new`, `clamp`, `num_rows`) where a call on another
        // receiver fuzzy-binds back to the current method. They also make a
        // single symbol unsortable in Kahn's algorithm, producing noisy
        // singleton cycles instead of useful porting order. Mutual cycles are
        // still reported below.
        if edge.source == edge.target {
            continue;
        }
        // source depends on target: add dependency source -> target
        dep_graph
            .entry(edge.source.as_str())
            .or_default()
            .push(edge.target.as_str());
        // reverse: target is depended on by source
        reverse_dep_graph
            .entry(edge.target.as_str())
            .or_default()
            .push(edge.source.as_str());
        *in_degree.entry(edge.source.as_str()).or_insert(0) += 1;
    }

    // Kahn's algorithm (BFS topological sort)
    let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
    for (&id, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(id);
        }
    }

    let mut levels: Vec<Vec<&str>> = Vec::new();
    let mut sorted_set: HashSet<&str> = HashSet::new();
    let mut emitted = 0usize;

    while !queue.is_empty() && emitted < limit {
        let mut current_level: Vec<&str> = Vec::new();
        let level_size = queue.len();
        for _ in 0..level_size {
            // Safety: we checked queue is non-empty above and iterate exactly level_size times
            let Some(id) = queue.pop_front() else { break };
            if sorted_set.contains(id) {
                continue;
            }
            sorted_set.insert(id);
            current_level.push(id);
            emitted += 1;
            if emitted >= limit {
                break;
            }
        }

        // For each sorted node, decrement in-degree of nodes that depend on it.
        for &sorted_id in &current_level {
            if let Some(dependents) = reverse_dep_graph.get(sorted_id) {
                for &dep_id in dependents {
                    if sorted_set.contains(dep_id) {
                        continue;
                    }
                    let deg = in_degree.entry(dep_id).or_insert(0);
                    if *deg > 0 {
                        *deg -= 1;
                    }
                    if *deg == 0 {
                        queue.push_back(dep_id);
                    }
                }
            }
        }

        if !current_level.is_empty() {
            levels.push(current_level);
        }
    }

    // Detect cycles: any unsorted nodes form cycles.
    let cycle_node_ids: HashSet<&str> = node_ids
        .iter()
        .map(std::string::String::as_str)
        .filter(|id| !sorted_set.contains(id))
        .collect();

    // Group cycles into SCCs so multiple disjoint mutually-recursive
    // groups don't collapse into one mega-cycle. Each non-trivial SCC
    // becomes its own entry with the files forming it surfaced — gives
    // the user a clear "break this cycle" target instead of a 200+
    // symbol blob.
    let mut cycle_adj: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (&node_id, neighbors) in &dep_graph {
        if !cycle_node_ids.contains(node_id) {
            continue;
        }
        let kept: HashSet<&str> = neighbors
            .iter()
            .copied()
            .filter(|n| cycle_node_ids.contains(n))
            .collect();
        cycle_adj.insert(node_id, kept);
    }
    let sccs = crate::graph::scc::tarjan_scc(&cycle_adj);

    let mut cycles_json: Vec<Value> = Vec::new();
    for scc in sccs {
        if !crate::graph::scc::is_cyclic_scc(&scc, &cycle_adj) {
            continue;
        }
        let scc_set: HashSet<&str> = scc.iter().copied().collect();
        // Rank symbols within the SCC by in-cycle out-degree (how many
        // *other* SCC members this symbol depends on). The symbol with the
        // smallest out-degree is the leaf-most node inside the cycle and is
        // the natural starting point: porting it requires stubbing the
        // fewest peers. The symbol with the largest out-degree is the
        // "hub" — the best candidate to break the cycle by refactoring its
        // call sites.
        let mut ranked: Vec<(&str, usize, usize)> = scc
            .iter()
            .map(|id| {
                let out_in_cycle = cycle_adj.get(id).map_or(0, |neighbors| {
                    neighbors.iter().filter(|n| scc_set.contains(*n)).count()
                });
                // In-degree (within the cycle) — how many SCC members
                // depend on this symbol. High in-degree = "many callers
                // inside the cycle", which is another useful break-point
                // signal.
                let mut in_in_cycle = 0;
                for (&src, neighbors) in &cycle_adj {
                    if !scc_set.contains(src) || src == *id {
                        continue;
                    }
                    if neighbors.contains(id) {
                        in_in_cycle += 1;
                    }
                }
                (*id, out_in_cycle, in_in_cycle)
            })
            .collect();
        // Ascending by out-degree → entry-point first; ties broken by
        // descending in-degree (hub-iness) so the most-referenced "leaf"
        // surfaces just after the cleanest leaf.
        ranked.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| b.2.cmp(&a.2)));

        let symbols_detailed: Vec<Value> = ranked
            .iter()
            .filter_map(|(id, out_deg, in_deg)| {
                let node = node_map.get(id)?;
                Some(json!({
                    "name": node.name,
                    "kind": node.kind.as_str(),
                    "file": node.file_path,
                    "line": node.start_line,
                    "in_cycle_out_degree": out_deg,
                    "in_cycle_in_degree": in_deg,
                }))
            })
            .collect();

        // Rank files by how many cycle members each contains — the file
        // with the most members is the best refactor target.
        let mut file_counts: HashMap<&str, usize> = HashMap::new();
        for id in &scc {
            if let Some(n) = node_map.get(id) {
                *file_counts.entry(n.file_path.as_str()).or_insert(0) += 1;
            }
        }
        let mut files_ranked: Vec<(&str, usize)> = file_counts.into_iter().collect();
        files_ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let files_json: Vec<Value> = files_ranked
            .iter()
            .map(|(path, count)| json!({"file": path, "members_in_cycle": count}))
            .collect();

        let entry_point = ranked.first().and_then(|(id, _, _)| node_map.get(id));
        let hub = ranked
            .iter()
            .max_by_key(|(_, _out, in_deg)| *in_deg)
            .and_then(|(id, _, _)| node_map.get(id));

        cycles_json.push(json!({
            "size": scc.len(),
            "files": files_json,
            "symbols": symbols_detailed,
            "entry_point": entry_point.map(|n| json!({
                "name": n.name, "file": n.file_path, "line": n.start_line,
            })),
            "break_point_candidate": hub.map(|n| json!({
                "name": n.name, "file": n.file_path, "line": n.start_line,
                "rationale": "Highest in-cycle in-degree — refactoring its callers is the most effective way to fragment this SCC.",
            })),
            "note": "Mutual dependency — port together, starting at `entry_point` and refactoring `break_point_candidate` to split the cycle.",
        }));
    }

    let levels_json: Vec<Value> = levels
        .iter()
        .enumerate()
        .map(|(i, level_ids)| {
            let description = if i == 0 {
                "No internal dependencies — port these first".to_string()
            } else {
                format!("Depends only on levels 0–{}", i - 1)
            };

            let symbols: Vec<Value> = level_ids
                .iter()
                .filter_map(|id| {
                    let node = node_map.get(id)?;
                    // Find what this node depends on (for depends_on field)
                    let deps: Vec<&str> = dep_graph
                        .get(id)
                        .map(|d| {
                            d.iter()
                                .filter_map(|dep_id| node_map.get(dep_id).map(|n| n.name.as_str()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let mut sym = json!({
                        "name": node.name,
                        "kind": node.kind.as_str(),
                        "file": node.file_path,
                        "line": node.start_line,
                    });
                    if !deps.is_empty() {
                        sym["depends_on"] = json!(deps);
                    }
                    Some(sym)
                })
                .collect();

            json!({
                "level": i,
                "description": description,
                "symbols": symbols,
            })
        })
        .collect();

    let touched_files = unique_file_paths(nodes.iter().map(|n| n.file_path.as_str()));

    let result = json!({
        "source_dir": source_dir,
        "total_symbols": total_symbols,
        "returned": emitted,
        "levels": levels_json,
        "cycles": cycles_json,
    });

    let text = render::finalize(Some(cg.project_root()), &args, &result, || {
        render::generic_md(&result)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched_files,
    ))
}

/// Handles `tracedecay_simplify_scan` tool calls.
pub(super) async fn handle_simplify_scan(
    cg: &TraceDecay,
    args: Value,
    _scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let files: Vec<String> = args
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: files (array of strings)".to_string(),
        })?;

    let mut duplications: Vec<Value> = Vec::new();
    let mut dead_introductions: Vec<Value> = Vec::new();
    let mut complexity_warnings: Vec<Value> = Vec::new();
    let mut coupling_warnings: Vec<Value> = Vec::new();

    for file in &files {
        // Store errors propagate: an empty scan result must mean "no
        // findings", never "the store query failed".
        let nodes = cg.get_nodes_by_file(file).await?;

        for node in &nodes {
            // 1. Duplication: find similar symbols elsewhere
            if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                let similar = cg.search(&node.name, 5).await?;
                let dupes: Vec<Value> = similar
                    .iter()
                    .filter(|s| {
                        s.node.id != node.id && s.score > 0.8 && s.node.file_path != node.file_path
                    })
                    .map(|d| {
                        json!({
                            "name": d.node.name,
                            "file": d.node.file_path,
                            "line": d.node.start_line,
                            "score": d.score,
                        })
                    })
                    .collect();
                if !dupes.is_empty() {
                    duplications.push(json!({
                        "symbol": node.name,
                        "file": node.file_path,
                        "line": node.start_line,
                        "similar_to": dupes,
                    }));
                }
            }

            // 2. Dead code: function/method with no incoming edges
            if matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.visibility != Visibility::Pub
                && node.name != "main"
                && !node.name.starts_with("test_")
            {
                let incoming = cg.get_incoming_edges(&node.id).await?;
                if incoming.is_empty() {
                    dead_introductions.push(json!({
                        "symbol": node.name,
                        "file": node.file_path,
                        "line": node.start_line,
                        "reason": "no incoming edges (unreferenced)",
                    }));
                }
            }

            // 3. Complexity: check if function exceeds threshold
            if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                let lines = node.end_line.saturating_sub(node.start_line) as usize;
                let fan_out = cg
                    .get_outgoing_edges(&node.id)
                    .await?
                    .iter()
                    .filter(|e| matches!(e.kind, crate::types::EdgeKind::Calls))
                    .count();
                let score = lines + fan_out * 3;
                if score > 100 {
                    complexity_warnings.push(json!({
                        "symbol": node.name,
                        "file": node.file_path,
                        "line": node.start_line,
                        "lines": lines,
                        "fan_out": fan_out,
                        "score": score,
                    }));
                }
            }
        }

        // 4. Coupling: check file fan_in
        let file_deps = cg.get_file_dependents(file).await?;
        if file_deps.len() > 15 {
            coupling_warnings.push(json!({
                "file": file,
                "fan_in": file_deps.len(),
                "warning": "high fan-in — changes here affect many dependents",
            }));
        }
    }

    let output = json!({
        "duplications": duplications,
        "dead_introductions": dead_introductions,
        "complexity_warnings": complexity_warnings,
        "coupling_warnings": coupling_warnings,
    });

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render_simplify_scan_markdown(&output)
    });
    Ok(ToolResult::new(
        json!({"content": [{"type": "text", "text": text}]}),
        files,
    ))
}

fn render_simplify_scan_markdown(output: &Value) -> String {
    let mut md = Md::new();
    md.heading(1, "Simplify Scan");

    let duplications = output
        .get("duplications")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let dead = output
        .get("dead_introductions")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let complexity = output
        .get("complexity_warnings")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let coupling = output
        .get("coupling_warnings")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let total = duplications.len() + dead.len() + complexity.len() + coupling.len();

    if total == 0 {
        md.empty_note("No simplification findings for the scanned files.");
        return md.render();
    }

    md.field("Findings", &total.to_string()).blank();
    render_simplify_duplications(&mut md, duplications);
    render_simplify_dead_code(&mut md, dead);
    render_simplify_complexity(&mut md, complexity);
    render_simplify_coupling(&mut md, coupling);
    md.render()
}

fn render_simplify_duplications(md: &mut Md, items: &[Value]) {
    render_simplify_section(md, "Possible Duplications", items, "symbol", |md, item| {
        md.line(&format!("  **Location:** {}", finding_location(item)));
        let similar = summarize_similar_symbols(item);
        if !similar.is_empty() {
            md.line(&format!("  **Similar symbols:** {similar}"));
        }
    });
}

fn render_simplify_dead_code(md: &mut Md, items: &[Value]) {
    render_simplify_section(md, "Potential Dead Code", items, "symbol", |md, item| {
        md.line(&format!("  **Location:** {}", finding_location(item)));
        md.line(&format!(
            "  **Reason:** {}",
            render::field_str(item, "reason")
        ));
    });
}

fn render_simplify_complexity(md: &mut Md, items: &[Value]) {
    render_simplify_section(md, "Complexity Warnings", items, "symbol", |md, item| {
        md.line(&format!("  **Location:** {}", finding_location(item)));
        md.line(&format!(
            "  **Lines:** {}",
            render::field_i64(item, "lines")
        ));
        md.line(&format!(
            "  **Fan-out:** {}",
            render::field_i64(item, "fan_out")
        ));
        md.line(&format!(
            "  **Score:** {}",
            render::field_i64(item, "score")
        ));
    });
}

fn render_simplify_coupling(md: &mut Md, items: &[Value]) {
    render_simplify_section(md, "Coupling Warnings", items, "file", |md, item| {
        md.line(&format!(
            "  **Fan-in:** {}",
            render::field_i64(item, "fan_in")
        ));
        md.line(&format!(
            "  **Warning:** {}",
            render::field_str(item, "warning")
        ));
    });
}

fn render_simplify_section<FDetails>(
    md: &mut Md,
    title: &str,
    items: &[Value],
    label_field: &str,
    details: FDetails,
) where
    FDetails: Fn(&mut Md, &Value),
{
    if items.is_empty() {
        return;
    }
    md.heading(2, title);
    for item in items {
        md.bullet(&format!("**{}**", render::field_str(item, label_field)));
        details(md, item);
    }
    md.blank();
}

fn finding_location(item: &Value) -> String {
    format!(
        "{}:{}",
        render::field_str(item, "file"),
        render::field_i64(item, "line")
    )
}

fn summarize_similar_symbols(item: &Value) -> String {
    let Some(similar) = item.get("similar_to").and_then(Value::as_array) else {
        return String::new();
    };
    let Some(first) = similar.first() else {
        return String::new();
    };
    let mut summary = format!(
        "{} at {}:{}",
        render::field_str(first, "name"),
        render::field_str(first, "file"),
        render::field_i64(first, "line")
    );
    if similar.len() > 1 {
        let _ = write!(summary, " (+{} more)", similar.len() - 1);
    }
    summary
}

/// Handles `tracedecay_type_hierarchy` tool calls.
pub(super) async fn handle_type_hierarchy(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;
    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(5, |v| v.min(10) as usize);

    let root = cg
        .get_node(node_id)
        .await?
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("node not found: {node_id}"),
        })?;

    let mut tree = format!(
        "{} ({}) -- {}:{}\n",
        root.name,
        root.kind.as_str(),
        root.file_path,
        root.start_line
    );
    let mut all_files: Vec<String> = vec![root.file_path.clone()];

    // Recursively build the hierarchy
    build_type_tree(cg, &root.id, max_depth, 0, &mut tree, &mut all_files).await?;

    let touched_files = unique_file_paths(all_files.iter().map(std::string::String::as_str));
    let payload = json!({
        "root": {
            "id": root.id,
            "name": root.name,
            "kind": root.kind.as_str(),
            "file": root.file_path,
            "line": root.start_line,
        },
        "max_depth": max_depth,
        "tree": tree,
    });
    let text = render::finalize(Some(cg.project_root()), &args, &payload, || {
        render_type_hierarchy_md(&payload)
    });
    Ok(ToolResult::new(
        json!({"content": [{"type": "text", "text": text}]}),
        touched_files,
    ))
}

fn render_type_hierarchy_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Type Hierarchy");
    if let Some(root) = value.get("root") {
        let name = render::field_str(root, "name");
        let kind = render::field_str(root, "kind");
        let file = render::field_str(root, "file");
        let line = render::field_i64(root, "line");
        md.field("root", &format!("{name} ({kind}) - {file}:{line}"));
    }
    md.field(
        "max_depth",
        &render::field_i64(value, "max_depth").to_string(),
    );
    md.blank().code("text", render::field_str(value, "tree"));
    md.render()
}

/// Recursively appends type hierarchy lines to the output string.
fn build_type_tree<'a>(
    cg: &'a TraceDecay,
    node_id: &'a str,
    max_depth: usize,
    depth: usize,
    output: &'a mut String,
    all_files: &'a mut Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        if depth >= max_depth {
            return Ok(());
        }

        let incoming = cg.get_incoming_edges(node_id).await?;
        let pad = "  ".repeat(depth);

        for edge in &incoming {
            if !matches!(
                edge.kind,
                crate::types::EdgeKind::Implements | crate::types::EdgeKind::Extends
            ) {
                continue;
            }
            if let Ok(Some(child)) = cg.get_node(&edge.source).await {
                let _ = writeln!(
                    output,
                    "{}|- {} {} ({}) -- {}:{}",
                    pad,
                    edge.kind.as_str(),
                    child.name,
                    child.kind.as_str(),
                    child.file_path,
                    child.start_line,
                );
                all_files.push(child.file_path.clone());
                build_type_tree(cg, &child.id, max_depth, depth + 1, output, all_files).await?;
            }
        }
        Ok(())
    })
}

/// Extract the source spanning tree-sitter rows `start_line..=end_line`
/// (0-based, inclusive) from `source`. Node line fields are stored as the
/// raw tree-sitter row index, so the caller passes them through unchanged.
/// Returns the empty string if the range is out of bounds.
pub(super) fn extract_lines(source: &str, start_line: u32, end_line: u32) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = start_line as usize;
    let end = (end_line as usize).saturating_add(1).min(lines.len());
    if start >= lines.len() || start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

/// Handles `tracedecay_body` tool calls.
pub(super) async fn handle_body(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let symbol =
        args.get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: symbol".to_string(),
            })?;

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(3, |v| v.clamp(1, 20) as usize);

    let chosen = body_candidates(
        cg,
        symbol,
        limit,
        scope_prefix,
        super::dependency_hints::lazy_indexing_requested(&args),
    )
    .await?;

    if chosen.is_empty() {
        return Ok(ToolResult::new(
            json!({
                "content": [{ "type": "text", "text": format!("No symbol named '{symbol}' found.") }]
            }),
            vec![],
        ));
    }

    let project_root = cg.project_root();
    let mut matches: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    for result in &chosen {
        let n = &result.node;
        let body = source_body_for_node(
            project_root,
            &n.file_path,
            n.start_line,
            n.end_line,
            &mut touched,
        );
        matches.push(json!({
            "id": n.id,
            "name": n.name,
            "qualified_name": n.qualified_name,
            "kind": n.kind.as_str(),
            "file": n.file_path,
            "start_line": n.start_line.saturating_add(1),
            "end_line": n.end_line.saturating_add(1),
            "signature": n.signature,
            "body": body,
        }));
    }

    let output = json!({
        "match_count": matches.len(),
        "matches": matches,
    });
    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render_body_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched,
    ))
}

/// Renders `tracedecay_body` matches like `render_read_md` rather than dumping
/// source into a table cell with newlines collapsed: each match gets a heading,
/// a location line, an optional signature, a token count, and a fenced code
/// block tagged with the file's language extension.
fn render_body_md(value: &Value) -> String {
    use crate::context::read_modes::estimate_tokens;

    let mut md = Md::new();
    let matches = value.get("matches").and_then(Value::as_array);
    let count = matches.map_or(0, std::vec::Vec::len);
    md.heading(2, &format!("Body matches ({count})"));

    let Some(matches) = matches else {
        return md.render();
    };
    for m in matches {
        let name = render::field_str(m, "name");
        let kind = render::field_str(m, "kind");
        let file = render::field_str(m, "file");
        let start = render::field_i64(m, "start_line");
        let end = render::field_i64(m, "end_line");
        let signature = render::field_str(m, "signature");
        let body = render::field_str(m, "body");

        md.blank();
        md.heading(3, &format!("{name} ({kind})"));
        md.field("location", &format!("{file}:{start}-{end}"));
        if !signature.is_empty() {
            md.field("signature", signature);
        }
        md.field("tokens", &estimate_tokens(body).to_string());
        md.blank();
        let lang = file.rsplit_once('.').map_or("", |(_, ext)| ext);
        md.code(lang, body);
    }
    md.render()
}

async fn body_candidates(
    cg: &TraceDecay,
    symbol: &str,
    limit: usize,
    scope_prefix: Option<&str>,
    lazy_index_ignored_dependencies: bool,
) -> Result<Vec<crate::types::SearchResult>> {
    // First try an exact-name lookup against the DB — this avoids the BM25
    // ranker's tendency to bury a definition under unrelated noise when the
    // bare name is common (e.g. `gmres` exists as both a `pub fn` and a
    // struct field). Falls back to suffix / name matching.
    let exact_nodes = cg.get_nodes_by_qualified_name(symbol).await?;
    let mut exact_nodes = filter_by_scope(exact_nodes, scope_prefix, |n| &n.file_path);
    if exact_nodes.is_empty() && lazy_index_ignored_dependencies {
        let indexed = dependency_hints::lazy_index_ignored_dependency_candidates(
            cg,
            symbol,
            limit,
            scope_prefix,
        )
        .await?;
        if !indexed.is_empty() {
            exact_nodes = filter_by_scope(
                cg.get_nodes_by_qualified_name(symbol).await?,
                scope_prefix,
                |n| &n.file_path,
            );
        }
    }

    // Wrap as SearchResult so the existing scoring/rendering path works.
    let mut candidates: Vec<crate::types::SearchResult> = exact_nodes
        .into_iter()
        .map(|node| crate::types::SearchResult { node, score: 0.0 })
        .collect();

    // If exact lookup returned nothing, fall back to BM25 search.
    if candidates.is_empty() {
        let raw = cg.search(symbol, (limit * 4).max(20)).await?;
        candidates = filter_by_scope(raw, scope_prefix, |r| &r.node.file_path);
    }

    // Whether the matches came from the exact lookup or the search fallback,
    // sort by `body_kind_preference` so callable / type definitions surface
    // above fields, variants, uses, etc. This is the bug-#1 fix: when both a
    // function and a same-named field exist, the function wins.
    candidates.sort_by_key(|r| body_kind_preference(&r.node.kind));
    candidates.truncate(limit);
    Ok(candidates)
}

fn source_body_for_node(
    project_root: &Path,
    file_path: &str,
    start_line: u32,
    end_line: u32,
    touched: &mut Vec<String>,
) -> String {
    let project_path = ProjectPath::resolve(project_root, Path::new(file_path));
    match project_path {
        Ok(ref path) => match crate::sync::read_source_file(&path.absolute_path()) {
            Ok(source) => {
                if !touched.iter().any(|path| path == file_path) {
                    touched.push(file_path.to_string());
                }
                extract_lines(&source, start_line, end_line)
            }
            Err(_) => String::from("<file unreadable>"),
        },
        Err(_) => String::from("<file path outside project>"),
    }
}

/// Ordering key used by `handle_body` to choose between same-named symbols.
/// Lower number = higher preference (sorted ascending). Callable kinds rank
/// best because the user almost always asks for "show me the body of X"
/// expecting a function or method; type definitions are next; fields,
/// variants, use statements come last.
fn body_kind_preference(kind: &NodeKind) -> u8 {
    match kind {
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::StructMethod
        | NodeKind::Constructor
        | NodeKind::AbstractMethod
        | NodeKind::ArrowFunction
        | NodeKind::Procedure => 0,
        NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Class
        | NodeKind::InnerClass
        | NodeKind::Interface
        | NodeKind::InterfaceType
        | NodeKind::Record
        | NodeKind::CaseClass
        | NodeKind::DataClass
        | NodeKind::SealedClass
        | NodeKind::TypeAlias
        | NodeKind::Union
        | NodeKind::Typedef => 1,
        NodeKind::Impl => 2,
        NodeKind::Const | NodeKind::Static | NodeKind::Macro | NodeKind::PreprocessorDef => 3,
        NodeKind::Field
        | NodeKind::ValField
        | NodeKind::VarField
        | NodeKind::Property
        | NodeKind::CSharpProperty
        | NodeKind::EnumVariant => 4,
        NodeKind::Use | NodeKind::Include => 5,
        _ => 6,
    }
}

/// Default marker kinds recognised by `tracedecay_todos`.
const DEFAULT_TODO_KINDS: &[&str] = &[
    "TODO",
    "FIXME",
    "XXX",
    "HACK",
    "WIP",
    "NOTE",
    "UNIMPLEMENTED",
];

/// True if `text` contains `marker` as a standalone uppercase word
/// (case-insensitive, surrounded by non-alphanumeric characters or string ends).
fn contains_marker_word(text: &str, marker: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let marker_lower = marker.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mlen = marker_lower.len();
    let mut idx = 0;
    while idx + mlen <= bytes.len() {
        if &bytes[idx..idx + mlen] == marker_lower.as_bytes() {
            let before_ok =
                idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric() && bytes[idx - 1] != b'_';
            let after_ok = idx + mlen == bytes.len()
                || (!bytes[idx + mlen].is_ascii_alphanumeric() && bytes[idx + mlen] != b'_');
            if before_ok && after_ok {
                return Some(idx);
            }
        }
        idx += 1;
    }
    None
}

/// Handles `tracedecay_todos` tool calls.
pub(super) async fn handle_todos(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let kinds: Vec<String> = args
        .get("kinds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_uppercase))
                .collect::<Vec<_>>()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_TODO_KINDS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        });

    let path = effective_path(&args, scope_prefix);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(200, |v| v.min(2000) as usize);

    let project_root = cg.project_root();
    let files = cg.get_all_files().await?;
    let mut markers: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut by_kind: HashMap<String, u64> = HashMap::new();

    'outer: for file in &files {
        if let Some(prefix) = path
            && !crate::path_scope::path_matches_scope(&file.path, Some(prefix))
        {
            continue;
        }
        let Ok(project_path) = ProjectPath::resolve(project_root, Path::new(&file.path)) else {
            continue;
        };
        let Ok(source) = crate::sync::read_source_file(&project_path.absolute_path()) else {
            continue;
        };
        // Cache nodes per file so enclosing-symbol lookup is one DB call per
        // file. Deliberately best-effort: the markers themselves come from
        // reading the source, so a store failure only drops the enclosing
        // symbol annotation — it never fakes an empty marker list.
        let nodes = cg.get_nodes_by_file(&file.path).await.unwrap_or_default();

        for (idx, line) in source.lines().enumerate() {
            let line_no = (idx as u32) + 1;
            for kind in &kinds {
                if contains_marker_word(line, kind).is_some() {
                    let enclosing = nodes
                        .iter()
                        .filter(|n| n.start_line <= line_no && line_no <= n.end_line)
                        .min_by_key(|n| n.end_line.saturating_sub(n.start_line))
                        .map(|n| n.qualified_name.clone());
                    *by_kind.entry(kind.clone()).or_insert(0) += 1;
                    markers.push(json!({
                        "kind": kind,
                        "file": file.path,
                        "line": line_no,
                        "text": line.trim(),
                        "enclosing": enclosing,
                    }));
                    if !touched.contains(&file.path) {
                        touched.push(file.path.clone());
                    }
                    if markers.len() >= limit {
                        break 'outer;
                    }
                    break; // one marker per line is enough
                }
            }
        }
    }

    let counts = serde_json::to_value(&by_kind).unwrap_or(json!({}));
    let output = json!({
        "match_count": markers.len(),
        "by_kind": counts,
        "markers": markers,
    });
    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched,
    ))
}

/// Handles `tracedecay_read` — mode-aware file read with cross-session cache.
pub(super) async fn handle_read(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let file =
        args.get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: file".to_string(),
            })?;

    let mode_str = args.get("mode").and_then(|v| v.as_str()).unwrap_or("full");
    let mode = ReadMode::parse(mode_str).ok_or_else(|| TraceDecayError::Config {
        message: format!("unknown mode '{mode_str}'; expected one of full, lines, map, signatures"),
    })?;
    let include_symbols = args
        .get("include_symbols")
        .and_then(Value::as_bool)
        .unwrap_or(mode == ReadMode::Lines);

    let line_range = if mode == ReadMode::Lines {
        let raw =
            args.get("lines")
                .and_then(|v| v.as_str())
                .ok_or_else(|| TraceDecayError::Config {
                    message: "mode='lines' requires the 'lines' argument (e.g. '120-180')"
                        .to_string(),
                })?;
        Some(
            LineRange::parse(raw).ok_or_else(|| TraceDecayError::Config {
                message: format!("invalid 'lines' value '{raw}'; expected 'A' or 'A-B'"),
            })?,
        )
    } else {
        None
    };

    let project_id = cg.project_root().to_string_lossy();
    let output = read_source(
        cg,
        SourceReadRequest {
            file,
            mode,
            line_range,
            raw_lines: args.get("lines").and_then(Value::as_str),
            include_symbols,
            project_id: &project_id,
        },
    )
    .await?;
    let display_file = output.file;
    let mut payload = json!({
        "file": &display_file,
        "mode": output.mode.as_str(),
        "mtime_ns": output.mtime_ns,
        "digest": output.digest,
        "token_count": output.token_count,
    });
    if output.unchanged {
        payload["unchanged"] = Value::Bool(true);
    }
    if let Some(body) = output.body {
        payload["body"] = Value::String(body);
    }
    if let Some(context) = output.context {
        payload["context"] = context;
    }
    let text = render::finalize(Some(cg.project_root()), &args, &payload, || {
        render_read_md(&payload)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![display_file],
    ))
}

fn render_read_md(value: &Value) -> String {
    let mut md = Md::new();
    let file = render::field_str(value, "file");
    let mode = render::field_str(value, "mode");
    md.heading(2, &format!("{file} ({mode})"));
    if value
        .get("unchanged")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        md.field("unchanged", "true");
        let digest = render::field_str(value, "digest");
        if !digest.is_empty() {
            md.field("digest", digest);
        }
    }
    md.field(
        "tokens",
        &render::field_i64(value, "token_count").to_string(),
    );
    render_read_context_md(&mut md, value.get("context"));
    if value.get("body").is_none() {
        return md.render();
    }
    md.blank();
    let lang = file.rsplit_once('.').map_or("", |(_, ext)| ext);
    md.code(lang, render::field_str(value, "body"));
    md.render()
}

fn render_read_context_md(md: &mut Md, context: Option<&Value>) {
    let Some(context) = context else {
        return;
    };
    let Some(symbols) = context.get("symbols").and_then(Value::as_array) else {
        return;
    };
    if symbols.is_empty() {
        return;
    }

    md.blank();
    md.heading(3, "Context");
    let symbol_count = context
        .get("symbol_count")
        .and_then(Value::as_u64)
        .unwrap_or(symbols.len() as u64);
    md.field("symbols", &symbol_count.to_string());
    for symbol in symbols {
        let kind = render::field_str(symbol, "kind");
        let name = render::field_str(symbol, "name");
        let line = render::field_i64(symbol, "line");
        let end_line = render::field_i64(symbol, "end_line");
        let signature = render::field_str(symbol, "signature");
        let span = if end_line > line {
            format!("{line}-{end_line}")
        } else {
            line.to_string()
        };
        if signature.is_empty() {
            md.bullet(&format!("{kind} {name} {span}"));
        } else {
            md.bullet(&format!("{kind} {name} {span}: `{signature}`"));
        }
    }
    if context
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        md.empty_note("symbol list truncated");
    }
}

/// Handles `tracedecay_outline` — flat symbol map for a file with optional
/// `kinds` filter.
pub(super) async fn handle_outline(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    use crate::context::read_modes::render_map;

    let file =
        args.get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: file".to_string(),
            })?;

    let kinds: Option<Vec<String>> = args.get("kinds").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    });

    let (abs_path, display_file) = resolve_indexed_source_file(cg, file).await?;

    let kinds_slice: Option<&[String]> = kinds.as_deref();
    let mut value = render_map(cg.db(), &display_file, kinds_slice).await?;
    match ast_grep_outline(&abs_path) {
        Ok(outline) => {
            value["ast_grep_outline"] = outline;
        }
        Err(err) => {
            value["ast_grep_outline"] = Value::Null;
            value["ast_grep_outline_error"] = json!(err.to_string());
        }
    }
    let text = render::finalize(Some(cg.project_root()), &args, &value, || {
        render_outline_md(&value)
    });

    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![display_file],
    ))
}

fn ast_grep_outline(abs_path: &Path) -> Result<Value> {
    ensure_ast_grep_outline_available()?;

    let output = crate::external_tools::ast_grep_command()
        .args([
            "outline",
            "--json=compact",
            "--items",
            "structure",
            "--view",
            "expanded",
        ])
        .arg(abs_path)
        .output()
        .map_err(|err| TraceDecayError::Config {
            message: format!("failed to run ast-grep outline: {err}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else if !stdout.trim().is_empty() {
            stdout.trim()
        } else {
            "no output"
        };
        return Err(TraceDecayError::Config {
            message: format!("ast-grep outline failed: {detail}"),
        });
    }

    serde_json::from_slice::<Value>(&output.stdout).map_err(|err| TraceDecayError::Config {
        message: format!("failed to parse ast-grep outline JSON: {err}"),
    })
}

fn ensure_ast_grep_outline_available() -> Result<()> {
    let diagnostics = super::super::definitions::ast_grep_diagnostics();
    if diagnostics.outline_available {
        Ok(())
    } else {
        Err(TraceDecayError::Config {
            message: format!(
                "tracedecay_outline requires ast-grep outline >= 0.44: {}",
                diagnostics.message
            ),
        })
    }
}

fn render_outline_md(value: &Value) -> String {
    let mut md = Md::new();
    let file = render::field_str(value, "file");
    let count = render::field_i64(value, "symbol_count");
    md.heading(2, &format!("Outline — {file}"));
    md.field("symbols", &count.to_string());
    md.blank();
    match value.get("symbols").and_then(Value::as_array) {
        Some(symbols) if !symbols.is_empty() => {
            for symbol in symbols {
                let name = render::field_str(symbol, "name");
                let kind = render::field_str(symbol, "kind");
                let visibility = render::field_str(symbol, "visibility");
                let line = render::field_i64(symbol, "line");
                let end = render::field_i64(symbol, "end_line");
                let span = if end > line {
                    format!("{line}-{end}")
                } else {
                    line.to_string()
                };
                let signature = render::field_str(symbol, "signature");
                md.bullet(&format!(
                    "**{name}** ({kind}) - lines {span} - {visibility}"
                ));
                if !signature.is_empty() {
                    md.line(&format!("  `{signature}`"));
                }
            }
        }
        _ => {
            md.empty_note("No symbols.");
        }
    }
    md.render()
}

/// Handles `tracedecay_config` — structured TOML / JSON queries by dotted
/// key path.
pub(super) fn handle_config(cg: &TraceDecay, args: &Value) -> Result<ToolResult> {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: key".to_string(),
        })?;
    let path = args.get("path").and_then(|v| v.as_str());
    let glob_pat = args.get("glob").and_then(|v| v.as_str());

    if path.is_none() && glob_pat.is_none() {
        return Err(TraceDecayError::Config {
            message: "tracedecay_config requires either 'path' or 'glob'".to_string(),
        });
    }
    if path.is_some() && glob_pat.is_some() {
        return Err(TraceDecayError::Config {
            message: "tracedecay_config: 'path' and 'glob' are mutually exclusive".to_string(),
        });
    }

    let project_root = cg.project_root().to_path_buf();
    let mut files: Vec<String> = Vec::new();
    if let Some(p) = path {
        let project_path = ProjectPath::resolve(&project_root, Path::new(p))?;
        files.push(project_path.relative_path_string());
    } else if let Some(pat) = glob_pat {
        let combined = project_root.join(pat);
        let walker =
            glob::glob(&combined.to_string_lossy()).map_err(|e| TraceDecayError::Config {
                message: format!("invalid glob '{pat}': {e}"),
            })?;
        for entry in walker.flatten() {
            if let Ok(project_path) = ProjectPath::resolve(&project_root, &entry) {
                files.push(project_path.relative_path_string());
            }
        }
        files.sort();
    }

    let mut matches: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for rel in &files {
        let project_path = ProjectPath::resolve(&project_root, Path::new(rel))?;
        let abs = project_path.absolute_path();
        let rel = project_path.relative_path_string();
        let Ok(contents) = std::fs::read_to_string(&abs) else {
            continue;
        };
        let Some(parsed) = parse_config_value(&rel, &contents) else {
            continue;
        };
        let parsed = match parsed {
            Ok(value) => value,
            Err(error) => {
                matches.push(json!({
                    "file": rel,
                    "error": error,
                }));
                continue;
            }
        };

        if !touched.contains(&rel) {
            touched.push(rel.clone());
        }
        matches.push(config_match_value(&rel, key, &contents, &parsed));
    }

    let payload = json!({
        "match_count": matches.iter().filter(|m| m.get("found") != Some(&Value::Bool(false))).count(),
        "matches": matches,
    });
    let text = render::finalize(Some(cg.project_root()), args, &payload, || {
        render::generic_md(&payload)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched,
    ))
}

#[derive(Debug, Clone, Copy)]
enum ConfigFormat {
    Toml,
    Json,
}

fn config_format(path: &str) -> Option<ConfigFormat> {
    let extension = Path::new(path).extension()?;
    if extension.eq_ignore_ascii_case("toml") {
        Some(ConfigFormat::Toml)
    } else if extension.eq_ignore_ascii_case("json") {
        Some(ConfigFormat::Json)
    } else {
        None
    }
}

fn parse_config_value(path: &str, contents: &str) -> Option<std::result::Result<Value, String>> {
    let parsed = match config_format(path)? {
        ConfigFormat::Toml => toml::from_str::<toml::Value>(contents)
            .map(|value| toml_to_json(&value))
            .map_err(|err| format!("toml parse error: {err}")),
        ConfigFormat::Json => serde_json::from_str::<Value>(contents)
            .map_err(|err| format!("json parse error: {err}")),
    };
    Some(parsed)
}

fn lookup_dotted(value: &Value, key: &str) -> Option<Value> {
    let mut cursor = value.clone();
    for segment in key.split('.') {
        cursor = match cursor {
            Value::Object(map) => map.get(segment).cloned()?,
            Value::Array(items) => {
                let idx: usize = segment.parse().ok()?;
                items.get(idx).cloned()?
            }
            _ => return None,
        };
    }
    Some(cursor)
}

fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number)
        }
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::Array(items.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let mut map = serde_json::Map::with_capacity(t.len());
            for (k, child) in t {
                map.insert(k.clone(), toml_to_json(child));
            }
            Value::Object(map)
        }
    }
}

fn config_match_value(file: &str, key: &str, contents: &str, parsed: &Value) -> Value {
    match lookup_dotted(parsed, key) {
        Some(value) => json!({
            "file": file,
            "key": key,
            "value": value,
            "line": find_key_line(contents, key),
        }),
        None => json!({
            "file": file,
            "key": key,
            "value": Value::Null,
            "found": false,
        }),
    }
}

fn find_key_line(contents: &str, key: &str) -> Option<u32> {
    let last = key.rsplit('.').next()?;
    let prefixes = config_key_line_prefixes(last);
    for (idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim_start();
        if prefixes.iter().any(|prefix| trimmed.starts_with(prefix)) {
            return Some((idx as u32) + 1);
        }
    }
    None
}

fn config_key_line_prefixes(key: &str) -> [String; 3] {
    [
        format!("{key} ="),
        format!("\"{key}\" ="),
        format!("\"{key}\":"),
    ]
}

/// Handles `tracedecay_signature_search` — substring search across the
/// cached `signature` column on every Function/Method node.
pub(super) async fn handle_signature_search(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let returns = args.get("returns").and_then(|v| v.as_str());
    let params: Vec<String> = args
        .get("params")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let want_async = args.get("async").and_then(serde_json::Value::as_bool);
    let path_filter = args.get("path").and_then(|v| v.as_str()).or(scope_prefix);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(50, |v| v.clamp(1, 500) as usize);

    if returns.is_none() && params.is_empty() && want_async.is_none() {
        return Err(TraceDecayError::Config {
            message:
                "tracedecay_signature_search requires at least one of returns / params / async"
                    .to_string(),
        });
    }

    let function_nodes = cg.db().get_nodes_by_kind(NodeKind::Function).await?;
    let method_nodes = cg.db().get_nodes_by_kind(NodeKind::Method).await?;

    let mut entries: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for node in function_nodes.iter().chain(method_nodes.iter()) {
        if let Some(prefix) = path_filter
            && !crate::path_scope::path_matches_scope(&node.file_path, Some(prefix))
        {
            continue;
        }

        if let Some(want) = want_async
            && node.is_async != want
        {
            continue;
        }

        let Some(sig) = node.signature.as_deref() else {
            continue;
        };

        if let Some(ret_pat) = returns
            && !returns_substring(sig).contains(ret_pat)
        {
            continue;
        }

        if !params.is_empty() {
            let param_region = params_substring(sig);
            if !params.iter().all(|p| param_region.contains(p.as_str())) {
                continue;
            }
        }

        if !touched.contains(&node.file_path) {
            touched.push(node.file_path.clone());
        }
        entries.push(json!({
            "name": node.name,
            "qualified_name": node.qualified_name,
            "kind": node.kind.as_str(),
            "file": node.file_path,
            "line": node.start_line,
            "is_async": node.is_async,
            "signature": sig,
        }));
        if entries.len() >= limit {
            break;
        }
    }

    let payload = json!({
        "match_count": entries.len(),
        "matches": entries,
    });
    let text = render::finalize(Some(cg.project_root()), &args, &payload, || {
        render::generic_md(&payload)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched,
    ))
}

fn returns_substring(signature: &str) -> &str {
    match signature.find("->") {
        Some(pos) => signature[pos + 2..].trim_start(),
        None => signature,
    }
}

fn params_substring(signature: &str) -> &str {
    let bytes = signature.as_bytes();
    let Some(open) = signature.find('(') else {
        return signature;
    };
    let mut depth = 0i32;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &signature[open + 1..i];
                }
            }
            _ => {}
        }
    }
    signature
}

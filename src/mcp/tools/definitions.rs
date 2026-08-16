//! MCP tool definitions (JSON Schema descriptors).
//!
//! Each `def_*` function returns a `ToolDefinition` with the tool name,
//! description, JSON Schema for its input parameters, MCP annotations
//! (readOnlyHint, title), and optional `_meta` (anthropic/alwaysLoad).
//!
//! The `def_*` functions live in domain submodules (graph, analysis, git,
//! testing, edit, lcm, memory, skills, admin); this root keeps the shared
//! schema helpers, the registry assembly (`get_tool_definitions`), and the
//! post-processing passes (project selectors, LCM storage scope, format).

use serde_json::{Value, json};
use std::collections::BTreeSet;
use tracedecay_tool_catalog::{CapabilityId, FeatureId, ProfileId, ScopeDimension};

use super::ToolDefinition;
use super::binding::registered_project_reader_tool_names;

mod admin;
mod analysis;
mod application;
mod application_schema;
pub(super) mod ast_grep;
mod edit;
mod git;
mod git_scope;
mod github_stack;
mod graph;
mod lcm;
mod memory;
mod multi_root;
mod native_integration;
mod native_worktree;
mod observatory;
mod session;
mod skills;
mod testing;
mod work;
mod workflow;

use admin::*;
use analysis::*;
use application::*;
use application_schema::canonical_application_request_schema;
use ast_grep::ast_grep_available;
pub(crate) use ast_grep::ast_grep_diagnostics;
use edit::*;
use git::*;
use github_stack::*;
use graph::*;
use lcm::*;
use memory::*;
use multi_root::*;
use native_integration::*;
use observatory::*;
use skills::*;
use testing::*;

/// Read-only annotations shared by every tool.
fn read_only(title: &str) -> Value {
    json!({
        "readOnlyHint": true,
        "title": title
    })
}

/// Build a `ToolDefinition` with `readOnlyHint` annotation and no `_meta`.
fn def(name: &str, title: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        annotations: Some(read_only(title)),
        meta: None,
    }
}

/// Write/exec annotations: tools that mutate files or run subprocesses.
fn read_write(title: &str) -> Value {
    json!({
        "readOnlyHint": false,
        "title": title
    })
}

/// Build a `ToolDefinition` for a tool that writes files or executes
/// subprocesses (`readOnlyHint: false`, no `_meta`).
fn def_rw(name: &str, title: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        annotations: Some(read_write(title)),
        meta: None,
    }
}

/// Build a `ToolDefinition` with `readOnlyHint` AND `anthropic/alwaysLoad`.
fn def_always_load(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        annotations: Some(read_only(title)),
        meta: Some(json!({ "anthropic/alwaysLoad": true })),
    }
}

fn object_schema(properties: Value) -> Value {
    let mut schema = json!({ "type": "object" });
    schema["properties"] = properties;
    schema
}

fn required_object_schema(properties: Value, required: &[&str]) -> Value {
    let mut schema = object_schema(properties);
    schema["required"] = json!(required);
    schema
}

fn def_object(name: &str, title: &str, description: &str, properties: Value) -> ToolDefinition {
    def(name, title, description, object_schema(properties))
}

fn def_required_object(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> ToolDefinition {
    def(
        name,
        title,
        description,
        required_object_schema(properties, required),
    )
}

fn string_property(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}

fn number_property(description: &str) -> Value {
    json!({
        "type": "number",
        "description": description
    })
}

fn def_path_limit_tool(
    name: &str,
    title: &str,
    description: &str,
    path_description: &str,
    limit_description: &str,
) -> ToolDefinition {
    def_object(
        name,
        title,
        description,
        json!({
            "path": string_property(path_description),
            "limit": number_property(limit_description)
        }),
    )
}

fn def_path_flag_tool(
    name: &str,
    title: &str,
    description: &str,
    path_description: &str,
    flag_name: &str,
    flag_description: &str,
) -> ToolDefinition {
    let mut properties = serde_json::Map::new();
    properties.insert("path".to_string(), string_property(path_description));
    properties.insert(
        flag_name.to_string(),
        json!({
            "type": "boolean",
            "description": flag_description
        }),
    );
    def_object(name, title, description, Value::Object(properties))
}

fn project_selector_properties() -> Value {
    json!({
        "project_selector": project_selector_object(
            "Optional registered project selector. Omit to use the active project."
        )
    })
}

fn with_project_selector_properties(mut properties: Value) -> Value {
    let Some(target) = properties.as_object_mut() else {
        return properties;
    };
    if let Some(selector_props) = project_selector_properties().as_object() {
        for (key, value) in selector_props {
            target.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    properties
}

fn project_selector_object(description: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "project_id": {
                "type": "string",
                "description": "Registered project id to query."
            }
        },
        "required": ["project_id"],
        "additionalProperties": false
    })
}

/// Computes the call budget based on project size.
pub fn explore_call_budget(total_nodes: u64) -> u8 {
    match total_nodes {
        0..=5_000 => 3,
        5_001..=20_000 => 4,
        20_001..=80_000 => 5,
        80_001..=250_000 => 7,
        _ => 10,
    }
}

/// Generates the `tracedecay_context` description with a dynamic call budget.
pub fn context_description(node_count: u64, budget: u8) -> String {
    format!(
        "Build an AI-ready context for a task description. Returns relevant symbols, \
         relationships, up to three untracked project memory matches when available, \
         and optionally code snippets.\n\n\
         CALL BUDGET (applies to tracedecay_context ONLY): {budget} calls maximum for \
         this project ({node_count} nodes). The narrow follow-up tools — tracedecay_search, \
         tracedecay_grep, tracedecay_callers, tracedecay_callees, tracedecay_body, \
         tracedecay_read, tracedecay_outline — are cheap and UNBUDGETED; call them freely. \
         When the context budget is spent, keep going with those narrow tracedecay tools to \
         drill in; do NOT fall back to native grep/glob/file reads. Only re-run \
         tracedecay_context if you genuinely need another broad semantic sweep."
    )
}

/// Returns tool definitions with a dynamic call budget for `tracedecay_context`.
pub fn get_tool_definitions_with_budget(
    node_count: u64,
    budget: u8,
) -> Result<Vec<ToolDefinition>, super::dispatch::McpDispatchMetadataError> {
    let mut defs = get_tool_definitions()?;
    apply_context_budget(&mut defs, node_count, budget);
    Ok(defs)
}

fn get_maximal_tool_definitions_with_budget(
    node_count: u64,
    budget: u8,
) -> Result<Vec<ToolDefinition>, super::dispatch::McpDispatchMetadataError> {
    let mut defs = get_maximal_tool_definitions()?;
    apply_context_budget(&mut defs, node_count, budget);
    Ok(defs)
}

fn apply_context_budget(defs: &mut [ToolDefinition], node_count: u64, budget: u8) {
    // Replace the context tool's description with the budgeted version
    for def in defs {
        if def.name == "tracedecay_context" {
            def.description = context_description(node_count, budget);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRegistryMode {
    HostAvailable,
    DeterministicMaximal,
}

/// Build the live MCP discovery result from the application catalog rather
/// than publishing the static compatibility registry as an unfiltered
/// superset.
pub fn get_catalog_filtered_tool_definitions_with_budget(
    node_count: u64,
    budget: u8,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Vec<ToolDefinition>, super::dispatch::McpDispatchMetadataError> {
    let catalog = crate::application_surface::application_surface_catalog_ref()?;
    let visible_operations = catalog
        .visible_bindings(
            profile_id,
            tracedecay_tool_catalog::BindingSurface::Mcp,
            1,
            &BTreeSet::<FeatureId>::new(),
            authorized_capabilities,
            available_scope,
        )
        .into_iter()
        .map(|(binding, _)| format!("tracedecay_{}", binding.operation().as_str()))
        .collect::<BTreeSet<_>>();
    let catalog_operations = catalog
        .capabilities()
        .flat_map(tracedecay_tool_catalog::CapabilityManifestV1::binding_ids)
        .filter_map(|binding_id| catalog.binding(binding_id))
        .filter(|binding| binding.surface() == tracedecay_tool_catalog::BindingSurface::Mcp)
        .map(|binding| format!("tracedecay_{}", binding.operation().as_str()))
        .collect::<BTreeSet<_>>();
    let mut definitions = get_maximal_tool_definitions_with_budget(node_count, budget)?;
    if registry_mode == ToolRegistryMode::HostAvailable {
        retain_host_available_tool_definitions(&mut definitions);
    }
    let mut definitions = definitions
        .into_iter()
        .filter(|definition| {
            !catalog_operations.contains(&definition.name)
                || visible_operations.contains(&definition.name)
        })
        .collect::<Vec<_>>();
    super::dispatch::attach_dispatch_metadata(&mut definitions)?;
    Ok(definitions)
}

pub fn get_catalog_filtered_tool_definitions_with_warming_budget(
    budget: u8,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Vec<ToolDefinition>, super::dispatch::McpDispatchMetadataError> {
    let mut definitions = get_catalog_filtered_tool_definitions_with_budget(
        0,
        budget,
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?;
    apply_context_warming_budget(&mut definitions, budget);
    Ok(definitions)
}

pub fn default_catalog_discovery_authority()
-> Result<BTreeSet<CapabilityId>, crate::application_surface::ApplicationSurfaceAdapterError> {
    Ok(
        crate::application_surface::application_surface_catalog_ref()?
            .capabilities()
            .map(|capability| capability.capability_id().clone())
            .collect(),
    )
}

pub fn project_catalog_discovery_scope() -> BTreeSet<ScopeDimension> {
    [
        ScopeDimension::Project,
        ScopeDimension::Repository,
        ScopeDimension::Worktree,
        ScopeDimension::Branch,
        ScopeDimension::Session,
        ScopeDimension::Resource,
        // The daemon-retained configuration authority resolves layers for
        // every surface, so configuration-scoped capabilities are
        // discoverable here; omitting this dimension hid every
        // `tracedecay_configuration_*` binding from live `tools/list`.
        ScopeDimension::ConfigurationLayer,
    ]
    .into_iter()
    .collect()
}

/// Returns tool definitions with a conservative temporary context budget while
/// a daemon opens the project graph needed to calculate the exact node count.
pub fn get_tool_definitions_with_warming_budget(
    budget: u8,
) -> Result<Vec<ToolDefinition>, super::dispatch::McpDispatchMetadataError> {
    let mut defs = get_tool_definitions()?;
    apply_context_warming_budget(&mut defs, budget);
    Ok(defs)
}

fn apply_context_warming_budget(defs: &mut [ToolDefinition], budget: u8) {
    for def in defs {
        if def.name == "tracedecay_context" {
            def.description = format!(
                "Build an AI-ready context for a task description. Returns relevant symbols, \
                 relationships, up to three untracked project memory matches when available, \
                 and optionally code snippets.\n\n\
                 CALL BUDGET (applies to tracedecay_context ONLY): {budget} calls maximum while \
                 this project graph is warming. The narrow follow-up tools — tracedecay_search, \
                 tracedecay_grep, tracedecay_callers, tracedecay_callees, tracedecay_body, \
                 tracedecay_read, tracedecay_outline — are cheap and UNBUDGETED; call them freely. \
                 When the context budget is spent, keep going with those narrow tracedecay tools \
                 to drill in; do NOT fall back to native grep/glob/file reads. Only re-run \
                 tracedecay_context if you genuinely need another broad semantic sweep."
            );
        }
    }
}

/// Returns the list of all tool definitions exposed by this MCP server.
///
/// Tools whose backing dependency is missing on the current host are
/// filtered out so the model never sees a tool that will immediately
/// fail when called. The host `ast-grep` CLI gates rewrite support.
/// `tracedecay_outline` remains advertised and reports its runtime
/// `ast-grep outline` requirement from the handler, because the Cursor
/// plugin docs/rules intentionally teach agents to start there.
pub fn get_tool_definitions()
-> Result<Vec<ToolDefinition>, super::dispatch::McpDispatchMetadataError> {
    let mut definitions = get_maximal_tool_definitions()?;
    retain_host_available_tool_definitions(&mut definitions);
    Ok(definitions)
}

pub(super) fn get_maximal_tool_definitions()
-> Result<Vec<ToolDefinition>, super::dispatch::McpDispatchMetadataError> {
    let application_registry =
        tracedecay_application::mcp_executable_binding_registry().map_err(|error| {
            super::dispatch::McpDispatchMetadataError::Initialization(error.to_string())
        })?;
    let request_schema = |operation: &'static str| {
        canonical_application_request_schema(&application_registry, operation)
    };
    let mut definitions = vec![
        def_search(),
        def_grep(),
        def_ast_grep_search(),
        def_retrieve(),
        def_context(request_schema("context")?),
        def_callers(),
        def_callees(request_schema("callees")?),
        def_impact(request_schema("impact")?),
        def_node(request_schema("node")?),
        def_status(),
        def_active_project(),
        def_project_list(),
        def_project_search(),
        def_project_context(),
        def_files(),
        def_git_status(),
        def_git_diff(),
        def_git_history(),
        def_git_blame(),
        def_git_hunks(),
        def_git_preview(),
        def_git_apply(),
        def_github_stack_signal_expand(),
        def_stack_snapshot(),
        def_preflight_native_integration(),
        def_approve_native_integration(),
        def_apply_native_integration(),
        def_native_integration_status(),
        def_cancel_native_integration(),
        def_multi_root_scope_set_read(),
        def_multi_root_scope_set_compare_and_swap(),
        def_multi_root_execute(),
        def_context_scout_status(),
        def_context_scout_recent(),
        def_context_scout_explain(),
        def_context_scout_capability(),
        def_context_scout_budget(),
        def_feedback_diagnostics(),
        def_feedback_get(),
        def_feedback_expand(),
        def_feedback_list(),
        def_feedback_advisory_cycle(),
        def_feedback_impact(),
        def_affected_tests(),
        def_test_results(),
        def_code_exact_occurrence(),
        def_code_phrase_search(),
        def_code_symbol_search(),
        def_code_signature_search(),
        def_code_implementations(),
        def_code_type_hierarchy(),
        def_code_callers(),
        def_code_callees(),
        def_code_facets(),
        def_code_timeline(),
        def_code_declaration(),
        def_code_definition(),
        def_code_type_definition(),
        def_code_references(),
        def_session_lookup(),
        def_qualified_name_read(),
        def_call_chain_read(),
        def_file_dependents_read(),
        def_source_lines_read(),
        def_source_body_read(),
        def_source_outline_read(),
        def_module_api_read(),
        def_file_metadata_read(),
        def_health_read(),
        def_health_delta(),
        def_storage_status_read(),
        def_diagnostics_read(),
        def_affected(),
        def_dead_code(),
        def_diff_context(),
        def_circular(),
        def_hotspots(),
        def_similar(request_schema("similar")?),
        def_rename_preview(request_schema("rename_preview")?),
        def_unused_imports(),
        def_unmounted_files(),
        def_rank(),
        def_largest(),
        def_coupling(),
        def_inheritance_depth(),
        def_distribution(),
        def_recursion(),
        def_complexity(),
        def_doc_coverage(),
        def_god_class(),
        def_changelog(),
        def_port_status(request_schema("port_status")?),
        def_port_order(request_schema("port_order")?),
        def_commit_context(),
        def_pr_context(),
        def_simplify_scan(),
        def_test_map(),
        def_type_hierarchy(),
        def_branch_search(),
        def_branch_diff(),
        def_branch_list(),
        def_str_replace(),
        def_multi_str_replace(),
        def_insert_at(),
        def_ast_grep_rewrite(),
        def_gini(),
        def_dependency_depth(),
        def_health(),
        def_redundancy(request_schema("redundancy")?),
        def_runtime(),
        def_dsm(),
        def_test_risk(),
        def_body(),
        def_todos(request_schema("todos")?),
        def_callers_for(),
        def_by_qualified_name(),
        def_signature(),
        def_impls(),
        def_diagnose(),
        def_derives(),
        def_run_affected_tests(),
        def_fact_store_add(request_schema("fact_store_add")?),
        def_fact_store_search(request_schema("fact_store_search")?),
        def_fact_store_probe(request_schema("fact_store_probe")?),
        def_fact_store_related(request_schema("fact_store_related")?),
        def_fact_store_reason(request_schema("fact_store_reason")?),
        def_fact_store_contradict(request_schema("fact_store_contradict")?),
        def_fact_store_get(request_schema("fact_store_get")?),
        def_fact_store_update(request_schema("fact_store_update")?),
        def_fact_store_remove(request_schema("fact_store_remove")?),
        def_fact_store_list(request_schema("fact_store_list")?),
        def_fact_feedback(request_schema("fact_feedback")?),
        def_memory_status(request_schema("memory_status")?),
        def_fact_store_curate(request_schema("fact_store_curate")?),
        def_automation_run_list(),
        def_automation_run_view(),
        def_automation_run_artifact_view(),
        def_skill_list(),
        def_skill_view(),
        def_hermes_skill_bridge(),
        def_dashboard(),
        def_analytics(),
        session::def_session_refresh(),
        session::def_session_refresh_begin(request_schema("session_refresh_begin")?),
        session::def_session_refresh_status(request_schema("session_refresh_status")?),
        session::def_session_refresh_cancel(request_schema("session_refresh_cancel")?),
        session::def_message_search(),
        session::def_sessions_for(),
        session::def_workflows(),
        def_lcm_status(),
        def_lcm_doctor(),
        def_lcm_load_session(),
        def_lcm_grep(),
        def_lcm_describe(),
        def_lcm_expand(),
        def_lcm_expand_query(),
        def_read(),
        def_outline(),
        def_implementations(),
        def_unsafe_patterns(),
        def_diagnostics(),
        def_config(),
        def_signature_search(),
        def_constructors(),
        def_field_sites(),
        def_replace_symbol(),
        def_insert_at_symbol(),
        def_move_symbol(),
        def_rename_symbol(),
        def_source_edit_reconcile(),
        def_source_edit_rollback(),
        def_find_exact_symbol(),
    ];
    definitions.extend(configuration_definitions());
    definitions.extend(context_scout_control_definitions());
    definitions.extend(observatory_definitions()?);
    definitions.extend(work::work_definitions()?);
    definitions.extend(workflow::workflow_definitions()?);
    definitions.extend(native_worktree::native_worktree_definitions());
    add_registered_project_selector_properties(&mut definitions);
    add_lcm_storage_scope_property(&mut definitions);
    add_format_property(&mut definitions);
    Ok(definitions)
}

fn retain_host_available_tool_definitions(definitions: &mut Vec<ToolDefinition>) {
    if !ast_grep_available() {
        definitions.retain(|d| d.name != "tracedecay_ast_grep_rewrite");
    }
    debug_assert!(
        !definitions.is_empty(),
        "get_tool_definitions returned empty list"
    );
    debug_assert!(
        definitions
            .iter()
            .all(|d| d.name.starts_with("tracedecay_")),
        "all tool definitions must have 'tracedecay_' prefix"
    );
}

/// Resolve a daemon-internal host surface for the CLI fallback without
/// advertising it through MCP discovery.
#[doc(hidden)]
pub fn internal_daemon_tool_definition(name: &str) -> Option<ToolDefinition> {
    match name {
        "tracedecay_hook_runtime" => Some(def_rw(
            "tracedecay_hook_runtime",
            "Internal Host Ingest",
            "Forward one exact host-ingest envelope to the daemon. The handler validates the action-specific payload.",
            json!({ "type": "object" }),
        )),
        _ => None,
    }
}

fn add_lcm_storage_scope_property(definitions: &mut [ToolDefinition]) {
    for definition in definitions.iter_mut().filter(|definition| {
        definition.name.starts_with("tracedecay_lcm_")
            || definition.name == "tracedecay_message_search"
    }) {
        let Some(properties) = definition
            .input_schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        properties.insert(
            "storage_scope".to_string(),
            json!({
                "type": "string",
                "enum": ["project", "user"],
                "description": "Session store scope. project (default) uses the active project shard; user uses the profile-level store for untethered conversations and cannot be combined with a project selector."
            }),
        );
    }
}

fn matching_tool_definitions_mut<'a>(
    definitions: &'a mut [ToolDefinition],
    tool_names: &'a [&'static str],
) -> impl Iterator<Item = &'a mut ToolDefinition> + 'a {
    definitions
        .iter_mut()
        .filter(move |definition| tool_names.contains(&definition.name.as_str()))
}

fn add_registered_project_selector_properties(definitions: &mut [ToolDefinition]) {
    for definition in
        matching_tool_definitions_mut(definitions, &registered_project_reader_tool_names())
    {
        let Some(properties) = definition.input_schema.get_mut("properties") else {
            continue;
        };
        *properties = with_project_selector_properties(std::mem::take(properties));
    }
}

const FORMAT_CAPABLE_TOOL_NAMES: &[&str] = &[
    // graph
    "tracedecay_search",
    "tracedecay_grep",
    "tracedecay_ast_grep_search",
    "tracedecay_context",
    "tracedecay_callers",
    "tracedecay_callees",
    "tracedecay_impact",
    "tracedecay_node",
    "tracedecay_similar",
    "tracedecay_rename_preview",
    "tracedecay_implementations",
    "tracedecay_callers_for",
    "tracedecay_find_exact_symbol",
    "tracedecay_by_qualified_name",
    "tracedecay_signature",
    "tracedecay_impls",
    "tracedecay_derives",
    // info
    "tracedecay_status",
    "tracedecay_project_list",
    "tracedecay_project_search",
    "tracedecay_project_context",
    "tracedecay_files",
    "tracedecay_body",
    "tracedecay_todos",
    "tracedecay_read",
    "tracedecay_outline",
    "tracedecay_config",
    "tracedecay_signature_search",
    "tracedecay_port_status",
    "tracedecay_port_order",
    "tracedecay_simplify_scan",
    // git
    "tracedecay_git_status",
    "tracedecay_git_diff",
    "tracedecay_git_history",
    "tracedecay_git_blame",
    "tracedecay_git_hunks",
    "tracedecay_affected",
    "tracedecay_diff_context",
    "tracedecay_changelog",
    "tracedecay_commit_context",
    "tracedecay_pr_context",
    "tracedecay_branch_search",
    "tracedecay_branch_diff",
    // application surfaces
    "tracedecay_git_preview",
    "tracedecay_git_apply",
    "tracedecay_github_stack_signal_expand",
    "tracedecay_stack_snapshot",
    "tracedecay_worktree_inventory",
    "tracedecay_worktree_cleanup_inspect",
    "tracedecay_worktree_cleanup_confirm",
    "tracedecay_worktree_cleanup_remove",
    "tracedecay_worktree_cleanup_reconcile",
    "tracedecay_observatory_read",
    "tracedecay_preflight_native_integration",
    "tracedecay_approve_native_integration",
    "tracedecay_apply_native_integration",
    "tracedecay_native_integration_status",
    "tracedecay_cancel_native_integration",
    "tracedecay_feedback_diagnostics",
    "tracedecay_feedback_get",
    "tracedecay_feedback_expand",
    "tracedecay_feedback_list",
    "tracedecay_feedback_impact",
    "tracedecay_affected_tests",
    "tracedecay_feedback_advisory_cycle",
    "tracedecay_test_results",
    "tracedecay_code_exact_occurrence",
    "tracedecay_code_phrase_search",
    "tracedecay_code_symbol_search",
    "tracedecay_code_signature_search",
    "tracedecay_code_implementations",
    "tracedecay_code_type_hierarchy",
    "tracedecay_code_callers",
    "tracedecay_code_callees",
    "tracedecay_code_facets",
    "tracedecay_code_timeline",
    "tracedecay_code_declaration",
    "tracedecay_code_definition",
    "tracedecay_code_type_definition",
    "tracedecay_code_references",
    "tracedecay_session_lookup",
    "tracedecay_qualified_name",
    "tracedecay_call_chain",
    "tracedecay_file_dependents",
    "tracedecay_source_lines",
    "tracedecay_source_body",
    "tracedecay_source_outline",
    "tracedecay_module_api",
    "tracedecay_file_metadata",
    "tracedecay_health_read",
    "tracedecay_health_delta",
    "tracedecay_storage_status",
    "tracedecay_diagnostics_read",
    "tracedecay_configuration_list",
    "tracedecay_configuration_explain",
    "tracedecay_configuration_get",
    "tracedecay_configuration_set",
    "tracedecay_configuration_unset",
    "tracedecay_configuration_batch",
    "tracedecay_configuration_write_credential",
    "tracedecay_configuration_observed_state",
    "tracedecay_configuration_protected_preview",
    "tracedecay_configuration_protected_apply",
    "tracedecay_configuration_rollback_preview",
    "tracedecay_configuration_rollback_apply",
    "tracedecay_configuration_audit",
    "tracedecay_context_scout_status",
    "tracedecay_context_scout_recent",
    "tracedecay_context_scout_explain",
    "tracedecay_context_scout_capability",
    "tracedecay_context_scout_budget",
    "tracedecay_context_scout_pause",
    "tracedecay_context_scout_resume",
    "tracedecay_context_scout_cancel",
    "tracedecay_context_scout_claim",
    "tracedecay_context_scout_delivery",
    "tracedecay_context_scout_feedback",
    // analysis
    "tracedecay_dead_code",
    "tracedecay_circular",
    "tracedecay_hotspots",
    "tracedecay_unused_imports",
    "tracedecay_unmounted_files",
    "tracedecay_rank",
    "tracedecay_largest",
    "tracedecay_coupling",
    "tracedecay_inheritance_depth",
    "tracedecay_distribution",
    "tracedecay_recursion",
    "tracedecay_complexity",
    "tracedecay_doc_coverage",
    "tracedecay_god_class",
    "tracedecay_unsafe_patterns",
    "tracedecay_diagnostics",
    "tracedecay_constructors",
    "tracedecay_field_sites",
    // health
    "tracedecay_test_map",
    "tracedecay_gini",
    "tracedecay_dependency_depth",
    "tracedecay_health",
    "tracedecay_runtime",
    "tracedecay_test_risk",
    // redundancy
    "tracedecay_redundancy",
    // memory
    "tracedecay_memory_status",
    "tracedecay_fact_store_add",
    "tracedecay_fact_store_curate",
    "tracedecay_fact_store_search",
    "tracedecay_fact_store_probe",
    "tracedecay_fact_store_related",
    "tracedecay_fact_store_reason",
    "tracedecay_fact_store_contradict",
    "tracedecay_fact_store_get",
    "tracedecay_fact_store_update",
    "tracedecay_fact_store_remove",
    "tracedecay_fact_store_list",
    "tracedecay_fact_feedback",
    // workflow
    "tracedecay_diagnose",
    "tracedecay_run_affected_tests",
    // session / LCM
    "tracedecay_message_search",
    "tracedecay_sessions_for",
    "tracedecay_workflows",
    "tracedecay_lcm_status",
    "tracedecay_lcm_doctor",
    "tracedecay_lcm_load_session",
    "tracedecay_lcm_grep",
    "tracedecay_lcm_describe",
    "tracedecay_lcm_expand",
    "tracedecay_lcm_expand_query",
    "tracedecay_session_refresh",
    "tracedecay_skill_list",
    "tracedecay_skill_view",
    "tracedecay_automation_run_list",
    "tracedecay_automation_run_view",
    "tracedecay_automation_run_artifact_view",
    "tracedecay_hermes_skill_bridge",
    // edit
    "tracedecay_str_replace",
    "tracedecay_multi_str_replace",
    "tracedecay_insert_at",
    "tracedecay_insert_at_symbol",
    "tracedecay_replace_symbol",
    "tracedecay_move_symbol",
    "tracedecay_rename_symbol",
    "tracedecay_ast_grep_rewrite",
    "tracedecay_source_edit_reconcile",
    "tracedecay_source_edit_rollback",
    // git & info
    "tracedecay_branch_list",
    "tracedecay_active_project",
    // misc
    "tracedecay_dashboard",
    "tracedecay_retrieve",
    "tracedecay_analytics",
    "tracedecay_type_hierarchy",
];

pub fn format_capable_tool_names() -> &'static [&'static str] {
    FORMAT_CAPABLE_TOOL_NAMES
}

pub fn tool_defaults_to_markdown(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_context"
            | "tracedecay_automation_run_list"
            | "tracedecay_automation_run_view"
            | "tracedecay_automation_run_artifact_view"
            | "tracedecay_dsm"
            | "tracedecay_fact_feedback"
            | "tracedecay_fact_store_add"
            | "tracedecay_fact_store_curate"
            | "tracedecay_fact_store_search"
            | "tracedecay_fact_store_probe"
            | "tracedecay_fact_store_related"
            | "tracedecay_fact_store_reason"
            | "tracedecay_fact_store_contradict"
            | "tracedecay_fact_store_get"
            | "tracedecay_fact_store_update"
            | "tracedecay_fact_store_remove"
            | "tracedecay_fact_store_list"
            | "tracedecay_files"
            | "tracedecay_read"
            | "tracedecay_skill_list"
            | "tracedecay_skill_view"
            | "tracedecay_type_hierarchy"
    )
}

fn add_format_property(definitions: &mut [ToolDefinition]) {
    for definition in matching_tool_definitions_mut(definitions, FORMAT_CAPABLE_TOOL_NAMES) {
        let properties = definition
            .input_schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .unwrap_or_else(|| panic!("{} must define object properties", definition.name));
        properties.insert(
            "format".to_string(),
            json!({
                "type": "string",
                "enum": ["markdown", "json"],
                "description": "Output format. Default 'markdown' (compact, LLM-optimized sections and bullets; no tables). Pass 'json' for compact machine-readable JSON when a program will parse the result."
            }),
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::unreadable_literal)]
mod tests;

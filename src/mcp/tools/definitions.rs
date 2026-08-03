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
mod ast_grep;
mod edit;
mod git;
mod git_scope;
mod graph;
mod lcm;
mod memory;
mod session;
mod skills;
mod testing;

use admin::*;
use analysis::*;
use application::*;
use edit::*;
use git::*;
use graph::*;
use lcm::*;
use memory::*;
use skills::*;
use testing::*;

// Re-exported for API parity with the pre-split module: the type is only
// consumed through `ast_grep_diagnostics()` return values, so nothing names
// it in-crate and the unused-imports lint would otherwise fire.
#[allow(unused_imports)]
pub use ast_grep::{
    AstGrepDiagnostics, ast_grep_available, ast_grep_diagnostics, ast_grep_diagnostics_json,
    ast_grep_outline_available,
};

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

fn boolean_property(description: &str) -> Value {
    json!({
        "type": "boolean",
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

fn def_limit_path_tool(
    name: &str,
    title: &str,
    description: &str,
    limit_description: &str,
    path_description: &str,
) -> ToolDefinition {
    def_object(
        name,
        title,
        description,
        json!({
            "limit": number_property(limit_description),
            "path": string_property(path_description)
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
    properties.insert(flag_name.to_string(), boolean_property(flag_description));
    def_object(name, title, description, Value::Object(properties))
}

fn def_node_depth_tool(
    name: &str,
    title: &str,
    description: &str,
    node_id_description: &str,
) -> ToolDefinition {
    def_required_object(
        name,
        title,
        description,
        json!({
            "node_id": string_property(node_id_description),
            "max_depth": number_property("Maximum traversal depth (default: 3)")
        }),
        &["node_id"],
    )
}

fn project_selector_properties() -> Value {
    json!({
        "project_selector": project_selector_object(
            "Optional registered project selector. Omit to use the active project.",
            "query",
        ),
        "project_id": {
            "type": "string",
            "description": "Convenience selector: registered project id to query instead of the active project."
        },
        "project_path": {
            "type": "string",
            "description": "Convenience selector: registered project root path or alias to query instead of the active project."
        }
    })
}

fn with_project_selector_properties(mut properties: Value) -> Value {
    let Some(target) = properties.as_object_mut() else {
        return properties;
    };
    if let Some(selector_props) = project_selector_properties().as_object() {
        for (key, value) in selector_props {
            target.insert(key.clone(), value.clone());
        }
    }
    properties
}

fn project_selector_object(description: &str, verb: &str) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": {
            "project_id": {
                "type": "string",
                "description": format!("Registered project id to {verb}.")
            },
            "path": {
                "type": "string",
                "description": format!("Registered project root path or alias to {verb}.")
            },
            "project_path": {
                "type": "string",
                "description": "Alias for path."
            }
        }
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
pub fn get_tool_definitions_with_budget(node_count: u64, budget: u8) -> Vec<ToolDefinition> {
    let mut defs = get_tool_definitions();
    apply_context_budget(&mut defs, node_count, budget);
    defs
}

fn get_maximal_tool_definitions_with_budget(node_count: u64, budget: u8) -> Vec<ToolDefinition> {
    let mut defs = get_maximal_tool_definitions();
    apply_context_budget(&mut defs, node_count, budget);
    defs
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
    let mut definitions = get_maximal_tool_definitions_with_budget(node_count, budget);
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
    ]
    .into_iter()
    .collect()
}

/// Returns tool definitions with a conservative temporary context budget while
/// a daemon opens the project graph needed to calculate the exact node count.
pub fn get_tool_definitions_with_warming_budget(budget: u8) -> Vec<ToolDefinition> {
    let mut defs = get_tool_definitions();
    apply_context_warming_budget(&mut defs, budget);
    defs
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
pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = get_maximal_tool_definitions();
    retain_host_available_tool_definitions(&mut definitions);
    definitions
}

pub(super) fn get_maximal_tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = vec![
        def_search(),
        def_grep(),
        def_ast_grep_search(),
        def_retrieve(),
        def_context(),
        def_callers(),
        def_callees(),
        def_impact(),
        def_node(),
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
        def_similar(),
        def_rename_preview(),
        def_api_migration_plan(),
        def_unused_imports(),
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
        def_port_status(),
        def_port_order(),
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
        def_redundancy(),
        def_runtime(),
        def_dsm(),
        def_test_risk(),
        def_session_start(),
        def_session_end(),
        def_body(),
        def_todos(),
        def_callers_for(),
        def_by_qualified_name(),
        def_signature(),
        def_impls(),
        def_diagnose(),
        def_derives(),
        def_run_affected_tests(),
        def_fact_store(),
        def_fact_feedback(),
        def_memory_status(),
        def_automation_run_artifact_view(),
        def_skill_list(),
        def_skill_view(),
        def_hermes_skill_bridge(),
        def_dashboard(),
        def_analytics(),
        session::def_session_refresh(),
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
        def_lcm_preflight(),
        def_lcm_compress(),
        def_lcm_session_boundary(),
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
        def_api_migration_apply(),
        def_source_edit_reconcile(),
        def_find_exact_symbol(),
    ];
    definitions.extend(configuration_definitions());
    definitions.extend(context_scout_control_definitions());
    add_registered_project_selector_properties(&mut definitions);
    add_lcm_storage_scope_property(&mut definitions);
    add_format_property(&mut definitions);
    definitions
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
    "tracedecay_session_start",
    "tracedecay_session_end",
    // redundancy
    "tracedecay_redundancy",
    // memory
    "tracedecay_memory_status",
    "tracedecay_fact_store",
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
    "tracedecay_lcm_session_boundary",
    "tracedecay_lcm_preflight",
    "tracedecay_lcm_compress",
    "tracedecay_session_refresh",
    // skills
    "tracedecay_skill_list",
    "tracedecay_skill_view",
    "tracedecay_automation_run_artifact_view",
    "tracedecay_hermes_skill_bridge",
    // edit
    "tracedecay_str_replace",
    "tracedecay_multi_str_replace",
    "tracedecay_insert_at",
    "tracedecay_insert_at_symbol",
    "tracedecay_replace_symbol",
    "tracedecay_move_symbol",
    "tracedecay_ast_grep_rewrite",
    "tracedecay_api_migration_plan",
    "tracedecay_api_migration_apply",
    "tracedecay_source_edit_reconcile",
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
            | "tracedecay_automation_run_artifact_view"
            | "tracedecay_dsm"
            | "tracedecay_fact_feedback"
            | "tracedecay_fact_store"
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
mod tests {
    use super::*;

    #[test]
    fn internal_host_ingest_is_cli_resolvable_but_not_advertised() {
        assert!(
            get_tool_definitions()
                .iter()
                .all(|definition| definition.name != "tracedecay_hook_runtime")
        );
        let definition = internal_daemon_tool_definition("tracedecay_hook_runtime")
            .expect("internal host-ingest definition");
        assert_eq!(definition.name, "tracedecay_hook_runtime");
        assert_eq!(definition.input_schema, json!({ "type": "object" }));
        assert!(internal_daemon_tool_definition("tracedecay_unknown").is_none());
    }

    #[test]
    fn multi_root_tools_are_not_discoverable() {
        let definitions = get_tool_definitions();
        for name in [
            "tracedecay_multi_root_scope_set_read",
            "tracedecay_multi_root_scope_set_compare_and_swap",
            "tracedecay_multi_root_execute",
        ] {
            assert!(
                definitions.iter().all(|definition| definition.name != name),
                "{name} must remain quarantined"
            );
        }
    }

    #[test]
    fn test_explore_call_budget_tiers() {
        assert_eq!(explore_call_budget(0), 3);
        assert_eq!(explore_call_budget(5000), 3);
        assert_eq!(explore_call_budget(5001), 4);
        assert_eq!(explore_call_budget(20000), 4);
        assert_eq!(explore_call_budget(20001), 5);
        assert_eq!(explore_call_budget(80000), 5);
        assert_eq!(explore_call_budget(80001), 7);
        assert_eq!(explore_call_budget(250000), 7);
        assert_eq!(explore_call_budget(250001), 10);
    }

    #[test]
    fn test_context_description_contains_budget() {
        let desc = context_description(5000, 4);
        assert!(
            desc.contains("4 calls maximum"),
            "description should contain budget: {desc}"
        );
        assert!(
            desc.contains("5000 nodes"),
            "description should contain node count: {desc}"
        );
    }

    #[test]
    fn context_scout_read_surfaces_are_registered_read_only() {
        let definitions = get_tool_definitions();
        for name in [
            "tracedecay_context_scout_status",
            "tracedecay_context_scout_recent",
            "tracedecay_context_scout_explain",
            "tracedecay_context_scout_capability",
            "tracedecay_context_scout_budget",
        ] {
            let definition = definitions
                .iter()
                .find(|definition| definition.name == name)
                .expect("Context Scout read surface is registered");
            assert_eq!(
                definition
                    .annotations
                    .as_ref()
                    .and_then(|annotations| annotations.get("readOnlyHint"))
                    .and_then(Value::as_bool),
                Some(true)
            );
        }
    }

    #[test]
    fn catalog_filtered_discovery_uses_the_deterministic_maximal_registry() {
        let profile_id = ProfileId::new(tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID)
            .expect("default profile");
        let definitions = get_catalog_filtered_tool_definitions_with_budget(
            0,
            explore_call_budget(0),
            &profile_id,
            &default_catalog_discovery_authority().expect("default discovery authority"),
            &project_catalog_discovery_scope(),
            ToolRegistryMode::DeterministicMaximal,
        )
        .expect("catalog-filtered definitions");

        assert!(
            definitions
                .iter()
                .any(|definition| definition.name == "tracedecay_ast_grep_rewrite")
        );

        let fingerprints = definitions
            .iter()
            .map(|definition| {
                let dispatch = &definition.meta.as_ref().unwrap()["tracedecay/dispatch"];
                assert_eq!(dispatch["version"], 1);
                assert_eq!(
                    definition.annotations.as_ref().unwrap()["readOnlyHint"],
                    dispatch["read_only"]
                );
                assert!(dispatch["deadline"]["maximum_millis"].as_u64().unwrap() > 0);
                dispatch["fingerprint"].as_str().unwrap()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            fingerprints.len(),
            1,
            "one catalog snapshot must fingerprint every advertised contract"
        );

        let dashboard = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_dashboard")
            .unwrap();
        let dispatch = &dashboard.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(dispatch["effect"], "administrative");
        assert_eq!(dispatch["availability"]["state"], "available");
        assert_eq!(dispatch["idempotency"], "idempotent");
        assert_eq!(dispatch["inverse"]["mode"], "same_tool");

        let doctor = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_lcm_doctor")
            .unwrap();
        let dispatch = &doctor.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(dispatch["effect"], "administrative");
        assert_eq!(dispatch["availability"]["state"], "unavailable");
        assert!(dispatch.get("receipt").is_none());
        assert!(dispatch.get("reconciliation").is_none());
    }

    #[test]
    fn handle_gated_feedback_reads_are_advertised_with_their_request_handle() {
        let definitions = get_tool_definitions();
        for name in [
            "tracedecay_feedback_diagnostics",
            "tracedecay_feedback_get",
            "tracedecay_feedback_expand",
            "tracedecay_feedback_list",
            "tracedecay_feedback_impact",
            "tracedecay_affected_tests",
        ] {
            let definition = definitions
                .iter()
                .find(|definition| definition.name == name)
                .unwrap_or_else(|| panic!("{name} must be advertised"));
            assert!(
                definition.input_schema["properties"]
                    .get("request_handle")
                    .is_some(),
                "{name} must accept the daemon-minted request handle"
            );
            assert_eq!(
                definition.input_schema["required"],
                json!(["request_handle"]),
                "{name} must require the request handle"
            );
        }
    }

    #[test]
    fn test_context_description_scopes_budget_and_frees_narrow_tools() {
        let desc = context_description(5000, 4);
        assert!(
            desc.contains("tracedecay_context ONLY"),
            "budget must be scoped to tracedecay_context so agents don't abandon after one call: {desc}"
        );
        assert!(
            desc.contains("UNBUDGETED"),
            "description must tell agents the narrow tools are unbudgeted: {desc}"
        );
        for narrow in [
            "tracedecay_search",
            "tracedecay_grep",
            "tracedecay_callers",
            "tracedecay_body",
        ] {
            assert!(
                desc.contains(narrow),
                "description should name the narrow follow-up tool {narrow}: {desc}"
            );
        }
    }

    #[test]
    fn test_get_tool_definitions_with_budget() {
        let defs = get_tool_definitions_with_budget(10000, 4);
        let context_tool = defs
            .iter()
            .find(|d| d.name == "tracedecay_context")
            .unwrap();
        assert!(context_tool.description.contains("4 calls maximum"));
        assert!(context_tool.description.contains("10000 nodes"));
    }

    #[test]
    fn catalog_filter_preserves_non_catalog_tools_and_filters_catalog_bindings() {
        let profile =
            ProfileId::new(tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID).unwrap();
        let definitions = get_catalog_filtered_tool_definitions_with_budget(
            10_000,
            4,
            &profile,
            &BTreeSet::new(),
            &project_catalog_discovery_scope(),
            ToolRegistryMode::HostAvailable,
        )
        .unwrap();

        assert!(
            definitions
                .iter()
                .any(|definition| definition.name == "tracedecay_context"),
            "legacy production tools remain discoverable until cataloged"
        );
        assert!(
            definitions
                .iter()
                .all(|definition| definition.name != "tracedecay_git_preview"),
            "catalog-bound tools require explicit capability authority"
        );
    }

    #[test]
    fn lcm_compatibility_definitions_expose_only_opaque_continuation_cursors() {
        let load = def_lcm_load_session();
        let grep = def_lcm_grep();

        for definition in [&load, &grep] {
            let properties = definition.input_schema["properties"]
                .as_object()
                .expect("LCM properties");
            assert_eq!(properties["cursor"]["type"], "string");
            assert_eq!(
                properties["temporal_mode"]["enum"],
                json!(["current", "as_of", "evolution", "forensic"])
            );
            assert_eq!(properties["as_of_micros"]["minimum"], 0);
        }

        assert!(
            load.input_schema["properties"]
                .get("after_store_id")
                .is_none(),
            "legacy offset pagination must not remain public"
        );
        assert_eq!(
            grep.input_schema["properties"]["include_summaries"]["default"],
            false
        );
        assert_eq!(
            grep.input_schema["properties"]["sort"]["default"],
            "relevance"
        );
    }
}

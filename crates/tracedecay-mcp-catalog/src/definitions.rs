//! MCP tool definitions (JSON Schema descriptors).
//!
//! Compatibility-owned `def_*` functions return their transport definitions.
//! Canonical application definitions project their names, schemas, effects,
//! and descriptions from the handler/catalog/executable authorities.
//!
//! The `def_*` functions live in domain submodules (graph, analysis, git,
//! testing, edit, lcm, memory, skills, admin); this module keeps the shared
//! schema helpers, the registry assembly (`get_tool_definitions`), and the
//! post-processing passes (project selectors, LCM storage scope, format).

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::LazyLock;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, CatalogValidationError, ExecutableBindingRegistryV1, OperationId,
    ScopeDimension,
};

use crate::McpCatalogError;
use crate::ToolDefinition;
use crate::registered_project_reader_tool_names;

mod admin;
mod analysis;
mod application;
mod application_schema;
pub mod ast_grep;
mod edit;
mod git;
mod graph;
mod lcm;
mod memory;
mod multi_root;
mod session;
mod skills;
mod testing;
mod work;
mod workflow;

use admin::*;
use analysis::*;
use application::*;
pub use application_schema::mcp_input_schema;
use application_schema::{canonical_application_request_schema, project_input_schema};
use ast_grep::ast_grep_available;
use edit::*;
use git::*;
use graph::*;
pub use graph::{SEARCH_MAX_LEXICAL_ANCHOR_BYTES, SEARCH_MAX_LEXICAL_ANCHORS};
use lcm::*;
use multi_root::*;
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
         and optionally code snippets. Use it for broad questions that need \
         relationship synthesis across the code graph. This project ({node_count} nodes) \
         allows {budget} broad context calls."
    )
}

/// Returns tool definitions with a dynamic call budget for `tracedecay_context`.
pub fn get_tool_definitions_with_budget(
    node_count: u64,
    budget: u8,
) -> Result<Vec<ToolDefinition>, McpCatalogError> {
    let mut defs = get_tool_definitions()?;
    apply_context_budget(&mut defs, node_count, budget);
    Ok(defs)
}

pub fn get_maximal_tool_definitions_with_budget(
    node_count: u64,
    budget: u8,
) -> Result<Vec<ToolDefinition>, McpCatalogError> {
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
) -> Result<Vec<ToolDefinition>, McpCatalogError> {
    let mut defs = get_tool_definitions()?;
    apply_context_warming_budget(&mut defs, budget);
    Ok(defs)
}

/// The `tracedecay_context` description while the project graph is still warming.
pub fn context_warming_description(budget: u8) -> String {
    format!(
        "Build an AI-ready context for a task description. Returns relevant symbols, \
         relationships, up to three untracked project memory matches when available, \
         and optionally code snippets. Use it for broad questions that need \
         relationship synthesis across the code graph. The graph is still warming, and \
         {budget} broad context calls are available."
    )
}

pub fn apply_context_warming_budget(defs: &mut [ToolDefinition], budget: u8) {
    for def in defs {
        if def.name == "tracedecay_context" {
            def.description = context_warming_description(budget);
        }
    }
}

/// Returns the list of all tool definitions exposed by this MCP server.
///
/// Tools whose backing dependency is missing on the current host are
/// filtered out so the model never sees a tool that will immediately
/// fail when called. The host `ast-grep` CLI gates rewrite support.
pub fn get_tool_definitions() -> Result<Vec<ToolDefinition>, McpCatalogError> {
    let mut definitions = get_maximal_tool_definitions()?;
    retain_host_available_tool_definitions(&mut definitions);
    Ok(definitions)
}

/// Counts how many times the maximal registry was actually assembled.
///
/// The registry is deterministic, so a correct cache builds it exactly once
/// per process no matter how many `tools/list` requests arrive.
#[cfg(test)]
pub(super) static MAXIMAL_DEFINITION_BUILDS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The maximal registry, assembled once per process and cloned per caller.
///
/// Every input is static for the life of the process: the application catalog
/// is a `LazyLock` snapshot and `ast_grep_available()` is a `OnceLock` host
/// probe. Nothing session-scoped is frozen here, the per-session passes
/// (`apply_context_budget`, `apply_context_warming_budget`, and the
/// profile/capability filtering in
/// `get_catalog_filtered_tool_definitions_with_budget`) all run on the *clone*
/// this returns, after the cache.
pub fn get_maximal_tool_definitions() -> Result<Vec<ToolDefinition>, McpCatalogError> {
    // The error type is not `Clone`, and a failure here is a deterministic
    // catalog/schema defect rather than a transient condition, so the cache
    // retains the rendered message and replays it.
    static MAXIMAL_DEFINITIONS: LazyLock<std::result::Result<Vec<ToolDefinition>, String>> =
        LazyLock::new(|| build_maximal_tool_definitions().map_err(|error| error.to_string()));
    match &*MAXIMAL_DEFINITIONS {
        Ok(definitions) => Ok(definitions.clone()),
        Err(message) => Err(McpCatalogError::Initialization(message.clone())),
    }
}

#[hotpath::measure(label = "mcp.catalog.assemble")]
fn build_maximal_tool_definitions() -> Result<Vec<ToolDefinition>, McpCatalogError> {
    #[cfg(test)]
    MAXIMAL_DEFINITION_BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // Work and Workflow own independent executable registries; assemble them
    // concurrently while the application descriptor projection runs here.
    let work_worker = spawn_definition_worker("tracedecay-catalog-work", work::work_definitions)?;
    let workflow_worker = spawn_definition_worker(
        "tracedecay-catalog-workflow",
        workflow::workflow_definitions,
    )?;
    let application_registry = tracedecay_contracts::mcp_executable_binding_registry()
        .map_err(|error| McpCatalogError::Initialization(error.to_string()))?;
    let request_schema = |operation: &'static str| {
        canonical_application_request_schema(application_registry, operation)
    };
    let mut definitions = vec![
        def_search(),
        def_grep(),
        def_ast_grep_search(),
        def_retrieve(),
        def_context(request_schema("context")?),
        def_impact(request_schema("impact")?),
        def_node(request_schema("node")?),
        def_status(),
        def_active_project(),
        def_project_list(),
        def_project_search(),
        def_project_context(),
        def_files(),
        def_multi_root_scope_set_read(),
        def_multi_root_scope_set_compare_and_swap(),
        def_multi_root_execute()?,
        def_remote_status_read(),
        def_affected(),
        def_dead_code(request_schema("dead_code")?),
        def_diff_context(),
        def_circular(request_schema("circular")?),
        def_hotspots(request_schema("hotspots")?),
        def_similar(request_schema("similar")?),
        def_redundancy(request_schema("redundancy")?),
        def_rename_preview(request_schema("rename_preview")?),
        def_unmounted_files(request_schema("unmounted_files")?),
        def_rank(request_schema("rank")?),
        def_largest(request_schema("largest")?),
        def_coupling(request_schema("coupling")?),
        def_inheritance_depth(request_schema("inheritance_depth")?),
        def_distribution(request_schema("distribution")?),
        def_recursion(request_schema("recursion")?),
        def_complexity(request_schema("complexity")?),
        def_doc_coverage(request_schema("doc_coverage")?),
        def_god_class(request_schema("god_class")?),
        def_changelog(),
        def_port_status(request_schema("port_status")?),
        def_port_order(request_schema("port_order")?),
        def_commit_context(),
        def_pr_context(),
        def_test_map(request_schema("test_map")?),
        def_branch_search(),
        def_branch_diff(),
        def_branch_list(),
        def_str_replace(),
        def_multi_str_replace(),
        def_insert_at(),
        def_ast_grep_rewrite(),
        def_gini(request_schema("gini")?),
        def_dependency_depth(request_schema("dependency_depth")?),
        def_health(request_schema("health")?),
        def_runtime(),
        def_dsm(request_schema("dsm")?),
        def_test_risk(request_schema("test_risk")?),
        def_todos(request_schema("todos")?),
        def_by_qualified_name(),
        def_signature(),
        def_diagnose(request_schema("diagnose")?),
        def_derives(),
        def_run_affected_tests(),
    ];
    definitions.extend(memory::memory_definitions(&request_schema)?);
    definitions.extend([
        def_automation_run_list(),
        def_automation_run_view(),
        def_automation_run_artifact_view(),
        def_skill_list(),
        def_skill_view(),
        def_hermes_skill_bridge(),
        def_dashboard(),
        def_analytics(),
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
        def_unsafe_patterns(request_schema("unsafe_patterns")?),
        def_config(),
        def_constructors(request_schema("constructors")?),
        def_field_sites(request_schema("field_sites")?),
        def_replace_symbol(),
        def_insert_at_symbol(),
        def_move_symbol(),
        def_rename_symbol(),
        def_source_edit_reconcile(),
        def_source_edit_rollback(),
        def_find_exact_symbol(),
    ]);
    definitions.extend(application_definitions()?);
    let work = work_worker.join().map_err(|_| {
        McpCatalogError::Initialization(
            "work tool catalog worker terminated unexpectedly".to_owned(),
        )
    })??;
    let workflow = workflow_worker.join().map_err(|_| {
        McpCatalogError::Initialization(
            "workflow tool catalog worker terminated unexpectedly".to_owned(),
        )
    })??;
    definitions.extend(work);
    definitions.extend(workflow);
    for definition in &mut definitions {
        project_input_schema(&mut definition.input_schema);
    }
    add_registered_project_selector_properties(&mut definitions);
    add_lcm_storage_scope_property(&mut definitions);
    add_format_property(&mut definitions)?;
    Ok(definitions)
}

pub(super) struct FamilyOperation {
    pub operation_id: String,
    pub name: String,
    pub title: String,
    pub description: String,
}

/// Project one executable registry into MCP tools. Callers own the transport
/// prefix, title, and description; the registry owns the schema and effect.
pub(super) fn project_executable_family(
    registry: &ExecutableBindingRegistryV1,
    operations: &[FamilyOperation],
    incomplete: (&'static str, &'static str),
    identity: (&'static str, &'static str),
    missing: (&'static str, &'static str),
) -> Result<Vec<ToolDefinition>, McpCatalogError> {
    if registry.iter().count() != operations.len() {
        return Err(catalog_invalid(incomplete.0, incomplete.1));
    }
    operations
        .iter()
        .map(|operation| {
            let operation_id = OperationId::new(operation.operation_id.clone())
                .map_err(|_| catalog_invalid(identity.0, identity.1))?;
            let binding = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .ok_or_else(|| catalog_invalid(missing.0, missing.1))?;
            Ok(ToolDefinition {
                name: operation.name.clone(),
                description: operation.description.clone(),
                input_schema: binding.request_schema().body().clone(),
                annotations: Some(json!({
                    "readOnlyHint": binding.effect().is_read_only(),
                    "title": operation.title,
                })),
                meta: None,
            })
        })
        .collect()
}

fn catalog_invalid(field: &'static str, reason: &'static str) -> McpCatalogError {
    CatalogValidationError::InvalidValue { field, reason }.into()
}

fn spawn_definition_worker(
    name: &'static str,
    worker: impl FnOnce() -> Result<Vec<ToolDefinition>, McpCatalogError> + Send + 'static,
) -> Result<std::thread::JoinHandle<Result<Vec<ToolDefinition>, McpCatalogError>>, McpCatalogError>
{
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(worker)
        .map_err(|error| {
            McpCatalogError::Initialization(format!(
                "failed to start {name} tool catalog worker: {error}"
            ))
        })
}

pub fn retain_host_available_tool_definitions(definitions: &mut Vec<ToolDefinition>) {
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

const FORMAT_CAPABLE_NON_APPLICATION_TOOL_NAMES: &[&str] = &[
    // graph
    "tracedecay_search",
    "tracedecay_grep",
    "tracedecay_ast_grep_search",
    "tracedecay_context",
    "tracedecay_impact",
    "tracedecay_node",
    "tracedecay_similar",
    "tracedecay_redundancy",
    "tracedecay_rename_preview",
    "tracedecay_find_exact_symbol",
    "tracedecay_by_qualified_name",
    "tracedecay_signature",
    "tracedecay_derives",
    // info
    "tracedecay_status",
    "tracedecay_project_list",
    "tracedecay_project_search",
    "tracedecay_project_context",
    "tracedecay_files",
    "tracedecay_todos",
    "tracedecay_config",
    "tracedecay_port_status",
    "tracedecay_port_order",
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
    "tracedecay_remote_status",
    // analysis
    "tracedecay_dead_code",
    "tracedecay_circular",
    "tracedecay_hotspots",
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
    "tracedecay_constructors",
    "tracedecay_field_sites",
    // health
    "tracedecay_test_map",
    "tracedecay_gini",
    "tracedecay_dependency_depth",
    "tracedecay_health",
    "tracedecay_runtime",
    "tracedecay_test_risk",
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
    "tracedecay_fact_store_supersede",
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
    "tracedecay_session_refresh_begin",
    "tracedecay_session_refresh_status",
    "tracedecay_session_refresh_cancel",
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
];

static FORMAT_CAPABLE_TOOL_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    FORMAT_CAPABLE_NON_APPLICATION_TOOL_NAMES
        .iter()
        .copied()
        .chain(ApplicationSurfaceOperation::MCP_TOOL_NAMES)
        .collect()
});

pub fn format_capable_tool_names() -> &'static [&'static str] {
    &FORMAT_CAPABLE_TOOL_NAMES
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
            | "tracedecay_fact_store_supersede"
            | "tracedecay_fact_store_list"
            | "tracedecay_files"
            | "tracedecay_skill_list"
            | "tracedecay_skill_view"
    )
}

fn add_format_property(definitions: &mut [ToolDefinition]) -> Result<(), McpCatalogError> {
    for definition in matching_tool_definitions_mut(definitions, format_capable_tool_names()) {
        let Some(schema) = definition.input_schema.as_object_mut() else {
            // Reachable on every tools/list and dispatch admission check, so a
            // malformed definition must surface as the builders' typed error
            // rather than a production panic.
            return Err(McpCatalogError::Initialization(format!(
                "{} must define an object schema",
                definition.name
            )));
        };
        let properties = schema
            .entry("properties")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| {
                McpCatalogError::Initialization(format!(
                    "{} must define object properties",
                    definition.name
                ))
            })?;
        // Repeated once per format-capable tool in every `tools/list`, so the
        // wording stays short.
        properties.insert(
            "format".to_string(),
            json!({
                "type": "string",
                "enum": ["markdown", "json"],
                "default": "markdown",
                "description": "Output format. Default 'markdown' (compact, LLM-optimized; no tables). 'json' for machine-readable output."
            }),
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::unreadable_literal)]
mod tests;

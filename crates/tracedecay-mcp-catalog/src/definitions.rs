//! MCP tool definitions (JSON Schema descriptors).
//!
//! Compatibility-owned definitions are rows of [`handwritten::HANDWRITTEN_TOOLS`].
//! Canonical application definitions project their names, schemas, effects,
//! and descriptions from the handler/catalog/executable authorities.
//!
//! This module keeps the shared schema helpers, the registry assembly
//! (`get_tool_definitions`), and the post-processing passes (project
//! selectors, LCM storage scope, format).

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

mod application;
mod application_schema;
mod edit;
mod handwritten;
mod lcm;
mod multi_root;
mod session;
mod work;
mod workflow;

use application::*;
pub use application_schema::mcp_input_schema;
use application_schema::{canonical_application_request_schema, project_input_schema};
use handwritten::{HANDWRITTEN_TOOLS, HandwrittenTool, ToolSchema};

/// Build one handwritten row's definition with `input_schema` and no `_meta`.
fn handwritten_definition(tool: &HandwrittenTool, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: tool.name.to_owned(),
        description: tool.description.to_owned(),
        input_schema,
        annotations: Some(json!({
            "readOnlyHint": tool.read_only,
            "title": tool.title
        })),
        meta: None,
    }
}

fn handwritten_input_schema(
    tool: &HandwrittenTool,
    registry: &ExecutableBindingRegistryV1,
) -> Result<Value, McpCatalogError> {
    match tool.schema {
        ToolSchema::Application => {
            let operation = tool.name.strip_prefix("tracedecay_").ok_or_else(|| {
                catalog_invalid("handwritten MCP tool name", "must start with tracedecay_")
            })?;
            canonical_application_request_schema(registry, operation)
        }
        ToolSchema::Handwritten(schema) => Ok(schema()),
        ToolSchema::Contract(schema) => schema(),
    }
}

/// Whether `operation` ships a handwritten definition instead of the catalog
/// projection.
fn has_handwritten_definition(operation: ApplicationSurfaceOperation) -> bool {
    HANDWRITTEN_TOOLS
        .iter()
        .any(|tool| tool.name == operation.mcp_tool_name())
}

/// `_meta` flag that keeps a tool's full schema loaded in hosts that defer the
/// rest of the catalog, and that names the tools default `tracedecay serve`
/// lists with full schemas rather than as stubs.
pub const ALWAYS_LOAD_META: &str = "anthropic/alwaysLoad";

/// The tool every truncated response tells the agent to call.
pub const RESPONSE_RETRIEVE_TOOL: &str = "tracedecay_retrieve";

/// The always-loaded tools: every tool with at least 100 recorded calls across
/// local Claude Code and Codex transcripts (October 2026), the project and
/// graph status reads, and [`RESPONSE_RETRIEVE_TOOL`].
const ALWAYS_LOADED_TOOL_NAMES: &[&str] = &[
    "tracedecay_active_project",
    "tracedecay_callers",
    "tracedecay_context",
    "tracedecay_diff_context",
    "tracedecay_fact_store_search",
    "tracedecay_files",
    "tracedecay_grep",
    "tracedecay_lcm_grep",
    "tracedecay_message_search",
    "tracedecay_node",
    RESPONSE_RETRIEVE_TOOL,
    "tracedecay_search",
    "tracedecay_source_body",
    "tracedecay_source_lines",
    "tracedecay_source_outline",
    "tracedecay_status",
    "tracedecay_storage_status",
    "tracedecay_test_map",
];

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

fn string_property(description: &str) -> Value {
    json!({
        "type": "string",
        "description": description
    })
}

/// A timestamp filter accepting Unix seconds or a parsed time string.
fn time_filter_schema(description: &str) -> Value {
    json!({
        "oneOf": [
            { "type": "integer", "minimum": 0 },
            { "type": "string" }
        ],
        "description": description
    })
}

/// Copies `project_selector` into each tool schema.
///
/// A `$ref` would shrink `tools/list`, and a host would still send the same
/// `project_selector.project_id` object. Hosts that do not resolve `$ref`
/// would drop the field from the schema they show the agent, so the copy
/// stays inline.
fn with_project_selector_properties(mut properties: Value) -> Value {
    if let Some(target) = properties.as_object_mut() {
        target.entry("project_selector").or_insert_with(|| {
            project_selector_object(
                "Optional registered project selector. Omit to use the active project.",
            )
        });
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

/// The `tracedecay_context` description for surfaces that have no project
/// graph to size, such as `tracedecay tool context --help`.
const CONTEXT_DESCRIPTION: &str = "Build an AI-ready context for a task description. Returns \
     relevant symbols, relationships, up to three untracked project memory matches when \
     available, and optionally code snippets. Use it for broad questions that need \
     relationship synthesis across the code graph.";

/// Generates the `tracedecay_context` description with a dynamic call budget.
pub fn context_description(node_count: u64, budget: u8) -> String {
    format!(
        "{CONTEXT_DESCRIPTION} This project ({node_count} nodes) allows {budget} broad \
         context calls."
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

fn apply_context_budget(defs: &mut [ToolDefinition], node_count: u64, budget: u8) {
    // Replace the context tool's description with the budgeted version
    for def in defs {
        if def.name == "tracedecay_context" {
            def.description = context_description(node_count, budget);
        }
    }
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

/// The `tracedecay_context` description while the project graph is still warming.
pub fn context_warming_description(budget: u8) -> String {
    format!(
        "{CONTEXT_DESCRIPTION} The graph is still warming, and {budget} broad context calls \
         are available."
    )
}

/// Returns the list of all tool definitions exposed by this MCP server.
pub fn get_tool_definitions() -> Result<Vec<ToolDefinition>, McpCatalogError> {
    Ok(get_tool_definitions_ref()?.to_vec())
}

/// One advertised tool used to parse a CLI invocation.
///
/// `tracedecay tool search` is a one-shot process. Assembling the full MCP
/// catalog just to read the search schema clones every other tool's JSON body.
/// Search's request schema is `SearchSurfaceRequestV1`; build that one
/// definition and leave the full registry for `tools/list`.
pub fn cli_tool_definition(name: &str) -> Result<ToolDefinition, McpCatalogError> {
    if ApplicationSurfaceOperation::from_tool_name(name)
        == Some(ApplicationSurfaceOperation::Search)
    {
        return Ok(search_cli_definition()?.clone());
    }
    get_tool_definitions_ref()?
        .iter()
        .find(|definition| definition.name == name)
        .cloned()
        .ok_or_else(|| McpCatalogError::Initialization(format!("unknown tool: {name}")))
}

fn search_cli_definition() -> Result<&'static ToolDefinition, McpCatalogError> {
    static DEFINITION: LazyLock<Result<ToolDefinition, String>> =
        LazyLock::new(|| build_search_cli_definition().map_err(|error| error.to_string()));
    match &*DEFINITION {
        Ok(definition) => Ok(definition),
        Err(message) => Err(McpCatalogError::Initialization(message.clone())),
    }
}

fn build_search_cli_definition() -> Result<ToolDefinition, McpCatalogError> {
    let canonical = serde_json::to_value(schemars::schema_for!(
        tracedecay_contracts::retrieval::SearchSurfaceRequestV1
    ))
    .map_err(|error| McpCatalogError::Initialization(error.to_string()))?;
    let search = HANDWRITTEN_TOOLS
        .iter()
        .find(|tool| tool.name == "tracedecay_search")
        .ok_or_else(|| catalog_invalid("search MCP definition", "must be handwritten"))?;
    let mut definition = handwritten_definition(search, canonical);
    mark_if_always_loaded(&mut definition);
    project_input_schema(&mut definition.input_schema);
    let mut definitions = vec![definition];
    add_format_property(&mut definitions)?;
    Ok(definitions.remove(0))
}

/// Borrow the registry, assembled once per process.
///
/// Every input is static for the life of the process: the application catalog
/// is a `LazyLock` snapshot. Nothing session-scoped is frozen here, the
/// per-session passes (the context description budget and the
/// profile/capability filtering in
/// `get_catalog_filtered_tool_definitions_with_budget`) all run on a clone,
/// after the cache.
pub fn get_tool_definitions_ref() -> Result<&'static [ToolDefinition], McpCatalogError> {
    // The error type is not `Clone`, and a failure here is a deterministic
    // catalog/schema defect rather than a transient condition, so the cache
    // retains the rendered message and replays it.
    static DEFINITIONS: LazyLock<std::result::Result<Vec<ToolDefinition>, String>> =
        LazyLock::new(|| build_tool_definitions().map_err(|error| error.to_string()));
    match &*DEFINITIONS {
        Ok(definitions) => Ok(definitions.as_slice()),
        Err(message) => Err(McpCatalogError::Initialization(message.clone())),
    }
}

#[tracing::instrument(name = "mcp.catalog.assemble", level = "trace", skip_all)]
fn build_tool_definitions() -> Result<Vec<ToolDefinition>, McpCatalogError> {
    // Work and Workflow own independent executable registries; assemble them
    // concurrently while the application descriptor projection runs here.
    let work_worker = spawn_definition_worker("tracedecay-catalog-work", work::work_definitions)?;
    let workflow_worker = spawn_definition_worker(
        "tracedecay-catalog-workflow",
        workflow::workflow_definitions,
    )?;
    let application_registry = tracedecay_contracts::mcp_executable_binding_registry()
        .map_err(|error| McpCatalogError::Initialization(error.to_string()))?;
    let mut definitions = HANDWRITTEN_TOOLS
        .iter()
        .map(|tool| {
            Ok(handwritten_definition(
                tool,
                handwritten_input_schema(tool, application_registry)?,
            ))
        })
        .collect::<Result<Vec<_>, McpCatalogError>>()?;
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
    mark_always_loaded(&mut definitions)?;
    Ok(definitions)
}

fn mark_always_loaded(definitions: &mut [ToolDefinition]) -> Result<(), McpCatalogError> {
    if let Some(name) = ALWAYS_LOADED_TOOL_NAMES.iter().find(|name| {
        definitions
            .iter()
            .all(|definition| definition.name != **name)
    }) {
        return Err(McpCatalogError::Initialization(format!(
            "always-loaded tool {name} is not in the catalog"
        )));
    }
    definitions.iter_mut().for_each(mark_if_always_loaded);
    Ok(())
}

fn mark_if_always_loaded(definition: &mut ToolDefinition) {
    if ALWAYS_LOADED_TOOL_NAMES.contains(&definition.name.as_str()) {
        definition.meta = Some(json!({ ALWAYS_LOAD_META: true }));
    }
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

/// Every public application operation accepts the `format` presentation flag.
static FORMAT_CAPABLE_TOOL_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    ApplicationSurfaceOperation::ALL
        .into_iter()
        .filter(|operation| !operation.is_internal())
        .map(ApplicationSurfaceOperation::mcp_tool_name)
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

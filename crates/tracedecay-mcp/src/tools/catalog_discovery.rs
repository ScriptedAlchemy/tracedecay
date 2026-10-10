//! Live `tools/list` filtering against the root application-surface catalog.
//!
//! Catalog assembly lives in `tracedecay-mcp`. These wrappers stay in the
//! composition root because they name `application_surface` and attach
//! dispatch metadata from the daemon-coupled binding table.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock};

use crate::{
    ToolDefinition, ToolListAdvertisement, ToolRegistryMode, ToolResult,
    advertise_tool_list_payload, ast_grep_available, context_description,
    context_warming_description, get_maximal_tool_definitions,
    retain_host_available_tool_definitions, tool_list_advertisement_from_env,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_tool_catalog::{CapabilityId, FeatureId, ProfileId, ScopeDimension};

use super::dispatch::McpDispatchMetadataError;

/// Documented ceiling for the process-wide discovery cache.
///
/// The live key space is profile × capability-set digest × scope digest ×
/// registry mode × host ast-grep gate. Production callers share one default
/// profile and one project scope; tests add a handful of capability-set
/// variants. Budget and node count are patched onto a hit and are not keys.
const DISCOVERY_CACHE_MAX_ENTRIES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DiscoveryCacheKey {
    profile_digest: [u8; 32],
    capabilities_digest: [u8; 32],
    scopes_digest: [u8; 32],
    registry_mode: u8,
    host_ast_grep: bool,
}

/// Only the serialized `{"tools": [...]}` payload is cached: every
/// production read serves it, and keeping the definitions it was serialized
/// from as well held the catalog twice.
static DISCOVERY_CACHE: LazyLock<RwLock<HashMap<DiscoveryCacheKey, Arc<Value>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[cfg(test)]
thread_local! {
    static DISCOVERY_CACHE_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// This thread's discovery cache hits since the last reset.
#[cfg(test)]
pub(crate) fn catalog_discovery_cache_hits_for_test() -> u64 {
    DISCOVERY_CACHE_HITS.with(std::cell::Cell::get)
}

/// Clears this thread's discovery cache hit counter.
#[cfg(test)]
pub(crate) fn reset_catalog_discovery_cache_hits_for_test() {
    DISCOVERY_CACHE_HITS.with(|hits| hits.set(0));
}

/// Drops every cached discovery entry. Test-only so parallel suites can
/// isolate hit-counter assertions without sharing a prior miss.
#[cfg(test)]
pub(crate) fn reset_catalog_discovery_cache_for_test() {
    match DISCOVERY_CACHE.write() {
        Ok(mut cache) => cache.clear(),
        Err(poisoned) => poisoned.into_inner().clear(),
    }
}

fn digest_strings<I, S>(values: I) -> [u8; 32]
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u8]>,
{
    let mut hasher = Sha256::new();
    for value in values {
        hasher.update(value.as_ref());
        hasher.update([0]);
    }
    hasher.finalize().into()
}

fn discovery_cache_key(
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> DiscoveryCacheKey {
    let registry_mode = match registry_mode {
        ToolRegistryMode::HostAvailable => 1,
        ToolRegistryMode::DeterministicMaximal => 2,
    };
    DiscoveryCacheKey {
        profile_digest: digest_strings([profile_id.as_str()]),
        capabilities_digest: digest_strings(
            authorized_capabilities
                .iter()
                .map(tracedecay_tool_catalog::CapabilityId::as_str),
        ),
        scopes_digest: digest_strings(available_scope.iter().map(|scope| {
            // Stable tag: serde rename is snake_case and the set is ordered.
            match scope {
                ScopeDimension::ConfigurationLayer => "configuration_layer",
                ScopeDimension::Project => "project",
                ScopeDimension::Repository => "repository",
                ScopeDimension::Worktree => "worktree",
                ScopeDimension::Branch => "branch",
                ScopeDimension::Session => "session",
                ScopeDimension::Resource => "resource",
            }
        })),
        registry_mode,
        host_ast_grep: ast_grep_available(),
    }
}

/// Node-independent compose: maximal definitions, host gate, catalog filter,
/// and dispatch-contract metadata. Context descriptions are patched on serve.
fn compose_node_independent_definitions(
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Vec<ToolDefinition>, McpDispatchMetadataError> {
    let catalog =
        tracedecay_daemon_service::application_surface::application_surface_catalog_ref()?;
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
    let mut definitions = get_maximal_tool_definitions()?;
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

fn record_discovery_cache_hit() {
    #[cfg(test)]
    {
        DISCOVERY_CACHE_HITS.with(|hits| hits.set(hits.get().saturating_add(1)));
    }
}

fn discovery_cache_get_or_insert(
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Arc<Value>, McpDispatchMetadataError> {
    let key = discovery_cache_key(
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    );
    if let Ok(cache) = DISCOVERY_CACHE.read()
        && let Some(entry) = cache.get(&key)
    {
        record_discovery_cache_hit();
        return Ok(Arc::clone(entry));
    }
    let tools = compose_node_independent_definitions(
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?;
    let entry = Arc::new(serde_json::json!({ "tools": tools }));
    match DISCOVERY_CACHE.write() {
        Ok(mut cache) => {
            if let Some(published) = cache.get(&key) {
                record_discovery_cache_hit();
                return Ok(Arc::clone(published));
            }
            if cache.len() < DISCOVERY_CACHE_MAX_ENTRIES {
                cache.insert(key, Arc::clone(&entry));
            }
        }
        Err(_) => {
            // A poisoned lock is not an authority failure: serve the compose
            // we already paid for rather than inventing a payload.
        }
    }
    Ok(entry)
}

fn context_description_for(node_count: Option<u64>, budget: u8) -> String {
    match node_count {
        None => context_warming_description(budget),
        Some(node_count) => context_description(node_count, budget),
    }
}

fn patch_context_description_in_payload(
    payload: &mut Value,
    description: String,
) -> Result<(), McpDispatchMetadataError> {
    let tools = payload
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            McpDispatchMetadataError::Initialization(
                "cached tools/list payload is missing the tools array".to_owned(),
            )
        })?;
    for tool in tools {
        if tool.get("name").and_then(Value::as_str) == Some("tracedecay_context") {
            let object = tool.as_object_mut().ok_or_else(|| {
                McpDispatchMetadataError::Initialization(
                    "cached tracedecay_context entry is not an object".to_owned(),
                )
            })?;
            object.insert("description".to_owned(), Value::String(description));
            return Ok(());
        }
    }
    Ok(())
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
) -> Result<Vec<ToolDefinition>, McpDispatchMetadataError> {
    let mut payload = catalog_discovery_tools_list_payload(
        Some(node_count),
        budget,
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?;
    let tools = payload.get_mut("tools").map(Value::take).ok_or_else(|| {
        McpDispatchMetadataError::Initialization(
            "cached tools/list payload is missing the tools array".to_owned(),
        )
    })?;
    serde_json::from_value(tools).map_err(|error| {
        McpDispatchMetadataError::Initialization(format!(
            "cached tools/list payload does not decode as tool definitions: {error}"
        ))
    })
}

/// Live `tools/list` payload after applying the process advertisement.
///
/// Compose stays the full catalog-filtered set so dispatch and search share
/// one cache. The handshake then projects that set onto the default core or
/// the explicit full listing.
pub fn advertised_catalog_discovery_tools_list_payload(
    node_count: Option<u64>,
    budget: u8,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Value, McpDispatchMetadataError> {
    advertised_catalog_discovery_tools_list_payload_with_mode(
        node_count,
        budget,
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
        tool_list_advertisement_from_env()?,
    )
}

/// [`advertised_catalog_discovery_tools_list_payload`] with an explicit mode.
pub fn advertised_catalog_discovery_tools_list_payload_with_mode(
    node_count: Option<u64>,
    budget: u8,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
    advertisement: ToolListAdvertisement,
) -> Result<Value, McpDispatchMetadataError> {
    let payload = catalog_discovery_tools_list_payload(
        node_count,
        budget,
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?;
    Ok(advertise_tool_list_payload(payload, advertisement)?)
}

/// The host-available catalog the default tool search reads.
///
/// Mounted sessions search this whole set. A projectless session passes only
/// its discoverable subset to [`execute_tool_search_within`] so search never
/// names a tool that connection cannot call.
pub fn catalog_tool_search_definitions() -> Result<Vec<ToolDefinition>, McpDispatchMetadataError> {
    let profile_id =
        ProfileId::new(tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID).map_err(|error| {
            McpDispatchMetadataError::Initialization(format!(
                "invalid MCP discovery profile: {error}"
            ))
        })?;
    get_catalog_filtered_tool_definitions_with_budget(
        0,
        crate::explore_call_budget(0),
        &profile_id,
        &default_catalog_discovery_authority()?,
        &crate::project_catalog_discovery_scope(),
        ToolRegistryMode::HostAvailable,
    )
}

/// Search the full catalog-filtered set by relevance or exact name.
///
/// This is the on-demand path for tools the default handshake withholds.
/// An empty query returns every reachable name. `names` loads those exact
/// definitions. `include_schema` defaults to true when `names` is set. The
/// rendered body runs through the shared response budget: an oversized
/// catalog lands in a retrieval handle instead of flooding one frame.
pub fn execute_tool_search(
    response_handle_root: Option<&Path>,
    args: &Value,
) -> Result<ToolResult, McpDispatchMetadataError> {
    execute_tool_search_within(
        response_handle_root,
        args,
        catalog_tool_search_definitions()?,
    )
}

/// [`execute_tool_search`] over a caller-visible definition set.
///
/// `definitions` is the set this connection may call: a mounted session the
/// full host-available catalog, a projectless session its discoverable
/// subset, so a listing or schema load never names a tool the connection
/// cannot call.
pub fn execute_tool_search_within(
    response_handle_root: Option<&Path>,
    args: &Value,
    definitions: Vec<ToolDefinition>,
) -> Result<ToolResult, McpDispatchMetadataError> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map_or("", str::trim)
        .to_owned();
    let names = args
        .get("names")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let include_schema = args
        .get("include_schema")
        .and_then(Value::as_bool)
        .unwrap_or(!names.is_empty());
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(50, |value| value.clamp(1, 500) as usize);

    let (mode, selected, total, missing_names) = if !names.is_empty() {
        let mut selected = Vec::new();
        let mut missing = Vec::new();
        for name in &names {
            match definitions
                .iter()
                .find(|definition| definition.name == *name)
            {
                Some(definition) => selected.push(definition.clone()),
                None => missing.push(name.clone()),
            }
        }
        ("load", selected, names.len(), missing)
    } else if query.is_empty() {
        let total = definitions.len();
        ("catalog", definitions, total, Vec::new())
    } else {
        let mut ranked = definitions
            .into_iter()
            .filter_map(|definition| {
                tool_search_score(&query, &definition).map(|score| (score, definition))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.name.cmp(&right.1.name))
        });
        let total = ranked.len();
        (
            "query",
            ranked
                .into_iter()
                .take(limit)
                .map(|(_, definition)| definition)
                .collect(),
            total,
            Vec::new(),
        )
    };
    let tools = selected
        .iter()
        .map(|definition| tool_search_match(definition, include_schema))
        .collect::<Vec<_>>();
    let payload = json!({
        "mode": mode,
        "query": if query.is_empty() { Value::Null } else { Value::String(query) },
        "total": total,
        "returned": tools.len(),
        "truncated": mode == "query" && total > tools.len(),
        "missing_names": missing_names,
        "tools": tools,
    });
    Ok(crate::tool_json_with_md(
        response_handle_root,
        args,
        &payload,
        || render_tool_search_markdown(&payload),
    ))
}

fn tool_search_score(query: &str, definition: &ToolDefinition) -> Option<u32> {
    let query = query.to_ascii_lowercase();
    let name = definition.name.to_ascii_lowercase();
    let short = name.strip_prefix("tracedecay_").unwrap_or(name.as_str());
    let description = definition.description.to_ascii_lowercase();
    if name == query || short == query {
        return Some(300);
    }
    if name.contains(&query) || short.contains(&query) {
        return Some(200);
    }
    if description.contains(&query) {
        return Some(100);
    }
    let tokens = query
        .split(|byte: char| !byte.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2)
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return None;
    }
    let hits = tokens
        .iter()
        .filter(|token| name.contains(**token) || description.contains(**token))
        .count();
    (hits > 0).then_some(50 + u32::try_from(hits).unwrap_or(u32::MAX).saturating_mul(10))
}

fn tool_search_match(definition: &ToolDefinition, include_schema: bool) -> Value {
    let title = definition
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let mut entry = json!({
        "name": definition.name,
        "title": title,
        "description": definition.description,
        "always_load": crate::tool_definition_is_always_loaded(definition),
    });
    if include_schema {
        entry["input_schema"] = definition.input_schema.clone();
    }
    entry
}

fn render_tool_search_markdown(payload: &Value) -> String {
    let tools = payload["tools"].as_array().cloned().unwrap_or_default();
    let mut lines = vec![format!(
        "TraceDecay tools ({}/{} {}, truncated={})",
        payload["returned"],
        payload["total"],
        payload["mode"].as_str().unwrap_or("catalog"),
        payload["truncated"]
    )];
    for tool in tools {
        let name = tool["name"].as_str().unwrap_or("unknown");
        let description = tool["description"].as_str().unwrap_or("");
        let first_line = description.lines().next().unwrap_or("");
        lines.push(format!("- {name}: {first_line}"));
        if let Some(schema) = tool.get("input_schema") {
            lines.push(format!(
                "  schema: {}",
                serde_json::to_string(schema).unwrap_or_else(|_| "{}".to_owned())
            ));
        }
    }
    if let Some(missing) = payload["missing_names"]
        .as_array()
        .filter(|missing| !missing.is_empty())
    {
        lines.push(format!(
            "missing names: {}",
            missing
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.join("\n")
}

/// Composed `{"tools": [...]}` discovery payload for `tools/list`.
///
/// A hit clones the cached `Value` once and patches only the dynamic
/// `tracedecay_context` description. It does not deep-clone definitions or
/// re-serialize dispatch contracts.
pub fn catalog_discovery_tools_list_payload(
    node_count: Option<u64>,
    budget: u8,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Value, McpDispatchMetadataError> {
    let entry = discovery_cache_get_or_insert(
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?;
    let mut payload = (*entry).clone();
    patch_context_description_in_payload(
        &mut payload,
        context_description_for(node_count, budget),
    )?;
    Ok(payload)
}

pub fn default_catalog_discovery_authority()
-> Result<BTreeSet<CapabilityId>, tracedecay_daemon_protocol::ApplicationSurfaceAdapterError> {
    Ok(
        tracedecay_daemon_service::application_surface::application_surface_catalog_ref()?
            .capabilities()
            .map(|capability| capability.capability_id().clone())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TOOL_SEARCH_TOOL_NAME, explore_call_budget, project_catalog_discovery_scope};

    #[test]
    fn catalog_filtered_discovery_uses_the_deterministic_maximal_registry() {
        let profile_id = ProfileId::new(tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID)
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

        let source_edit = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_ast_grep_rewrite")
            .expect("available source-edit handler is advertised");
        let source_edit_dispatch = &source_edit.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(source_edit_dispatch["effect"], "source_edit");
        assert_eq!(source_edit_dispatch["availability"]["state"], "available");
        assert_eq!(source_edit_dispatch["idempotency"], "key_required");

        for (tool_name, required_identity) in [
            (
                "tracedecay_approve_native_integration",
                &["preview_id", "preview_digest"][..],
            ),
            (
                "tracedecay_apply_native_integration",
                &[
                    "preview_id",
                    "preview_digest",
                    "approval_id",
                    "approval_digest",
                    "transaction_id",
                ][..],
            ),
            (
                "tracedecay_cancel_native_integration",
                &["transaction_id"][..],
            ),
        ] {
            let definition = definitions
                .iter()
                .find(|definition| definition.name == tool_name)
                .expect("native-integration effect is advertised");
            let dispatch = &definition.meta.as_ref().unwrap()["tracedecay/dispatch"];
            assert_eq!(dispatch["idempotency"], "idempotent", "{tool_name}");

            let properties = definition.input_schema["properties"]
                .as_object()
                .expect("native-integration input properties");
            assert!(
                !properties.contains_key("idempotency_key"),
                "{tool_name} must not advertise an unused caller idempotency key"
            );
            assert_eq!(
                definition.input_schema["required"],
                serde_json::json!(required_identity),
                "{tool_name} must expose its durable replay identity"
            );
        }

        let fingerprints = definitions
            .iter()
            .map(|definition| {
                let dispatch = &definition.meta.as_ref().unwrap()["tracedecay/dispatch"];
                assert_eq!(dispatch["version"], 1);
                assert_eq!(
                    definition.annotations.as_ref().unwrap()["readOnlyHint"],
                    dispatch["read_only"]
                );
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
        assert_eq!(dispatch["effect"], "binds_server");
        assert_eq!(dispatch["read_only"], false);
        assert_eq!(dispatch["availability"]["state"], "available");
        assert_eq!(dispatch["idempotency"], "idempotent");
        assert_eq!(dispatch["inverse"]["mode"], "same_tool");

        let doctor = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_lcm_doctor")
            .unwrap();
        let dispatch = &doctor.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(dispatch["effect"], "read");
        assert_eq!(dispatch["availability"]["state"], "available");
        assert_eq!(dispatch["deadline"]["maximum_millis"], 30_000);
        assert!(dispatch.get("receipt").is_none());
        assert!(dispatch.get("reconciliation").is_none());

        let affected_tests = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_run_affected_tests")
            .unwrap();
        let dispatch = &affected_tests.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(dispatch["effect"], "spawns_process");
        assert_eq!(dispatch["read_only"], false);
        assert_eq!(
            dispatch["deadline"]["maximum_millis"], 600_000,
            "a long-running tool gets the ten-minute ceiling"
        );

        for retired in [
            "tracedecay_lcm_preflight",
            "tracedecay_lcm_compress",
            "tracedecay_lcm_session_boundary",
        ] {
            assert!(
                definitions
                    .iter()
                    .all(|definition| definition.name != retired),
                "{retired} must remain daemon-internal"
            );
        }
    }

    #[test]
    fn catalog_filter_preserves_non_catalog_tools_and_filters_catalog_bindings() {
        let profile = ProfileId::new(tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID).unwrap();
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
                .any(|definition| definition.name == "tracedecay_multi_root_execute"),
            "legacy production tools remain discoverable until cataloged"
        );
        assert!(
            definitions.iter().all(|definition| {
                definition.name != "tracedecay_search"
                    && definition.name != "tracedecay_run_affected_tests"
                    && definition.name != "tracedecay_dashboard"
                    && definition.name != "tracedecay_status"
                    && definition.name != "tracedecay_context"
                    && definition.name != "tracedecay_git_preview"
            }),
            "catalog-bound tools require explicit capability authority"
        );
    }

    fn default_discovery_inputs() -> (ProfileId, BTreeSet<CapabilityId>, BTreeSet<ScopeDimension>) {
        (
            ProfileId::new(tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID)
                .expect("default profile"),
            default_catalog_discovery_authority().expect("default discovery authority"),
            project_catalog_discovery_scope(),
        )
    }

    fn serialize_tools_payload(payload: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(payload).expect("tools/list payload must serialize")
    }

    /// Compose without reading or writing the process cache. Byte-equivalence
    /// must compare a cache serve against this path, not two cache-backed APIs.
    fn freshly_composed_tools_list_payload(
        node_count: Option<u64>,
        budget: u8,
        profile_id: &ProfileId,
        authorized_capabilities: &BTreeSet<CapabilityId>,
        available_scope: &BTreeSet<ScopeDimension>,
        registry_mode: ToolRegistryMode,
    ) -> Result<Value, McpDispatchMetadataError> {
        let mut tools = compose_node_independent_definitions(
            profile_id,
            authorized_capabilities,
            available_scope,
            registry_mode,
        )?;
        let description = context_description_for(node_count, budget);
        for tool in &mut tools {
            if tool.name == "tracedecay_context" {
                tool.description.clone_from(&description);
            }
        }
        Ok(serde_json::json!({ "tools": tools }))
    }

    fn with_discovery_counter_lock<T>(body: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_catalog_discovery_cache_for_test();
        reset_catalog_discovery_cache_hits_for_test();
        crate::tools::dispatch::reset_attach_dispatch_metadata_calls_for_test();
        body()
    }

    #[test]
    fn repeated_equivalent_discovery_hits_the_process_cache() {
        with_discovery_counter_lock(|| {
            let (profile, authority, scope) = default_discovery_inputs();
            let first = catalog_discovery_tools_list_payload(
                None,
                explore_call_budget(0),
                &profile,
                &authority,
                &scope,
                ToolRegistryMode::HostAvailable,
            )
            .expect("first discovery compose");
            let attaches_after_first =
                crate::tools::dispatch::attach_dispatch_metadata_calls_for_test();
            let hits_after_first = catalog_discovery_cache_hits_for_test();

            let second = catalog_discovery_tools_list_payload(
                None,
                explore_call_budget(0),
                &profile,
                &authority,
                &scope,
                ToolRegistryMode::HostAvailable,
            )
            .expect("second discovery compose");
            assert_eq!(
                serialize_tools_payload(&first),
                serialize_tools_payload(&second),
                "equivalent discovery must stay byte-identical"
            );
            assert_eq!(
                crate::tools::dispatch::attach_dispatch_metadata_calls_for_test(),
                attaches_after_first,
                "a cache hit must not serialize dispatch contracts again"
            );
            assert!(
                catalog_discovery_cache_hits_for_test() >= hits_after_first.saturating_add(1),
                "the second equivalent compose must be a cache hit"
            );
        });
    }

    #[test]
    fn cached_payload_matches_fresh_compose_for_every_mode_and_budget() {
        let (profile, authority, scope) = default_discovery_inputs();
        for mode in [
            ToolRegistryMode::DeterministicMaximal,
            ToolRegistryMode::HostAvailable,
        ] {
            for budget in [3_u8, 4, 5, 7, 10] {
                for node_count in [None, Some(0_u64), Some(6_000), Some(100_000)] {
                    let cached = catalog_discovery_tools_list_payload(
                        node_count, budget, &profile, &authority, &scope, mode,
                    )
                    .expect("cached discovery payload");
                    let fresh = freshly_composed_tools_list_payload(
                        node_count, budget, &profile, &authority, &scope, mode,
                    )
                    .expect("fresh discovery compose");
                    assert_eq!(
                        serialize_tools_payload(&cached),
                        serialize_tools_payload(&fresh),
                        "cached payload must match a fresh compose for mode={mode:?} budget={budget} node_count={node_count:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn distinct_authorized_capability_sets_never_share_a_cache_entry() {
        with_discovery_counter_lock(|| {
            let (profile, full_authority, scope) = default_discovery_inputs();
            let empty_authority = BTreeSet::new();
            let full = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &full_authority,
                &scope,
                ToolRegistryMode::DeterministicMaximal,
            )
            .expect("full-authority payload");
            let empty = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &empty_authority,
                &scope,
                ToolRegistryMode::DeterministicMaximal,
            )
            .expect("empty-authority payload");
            assert_ne!(
                serialize_tools_payload(&full),
                serialize_tools_payload(&empty),
                "different authorized-capability sets must produce distinct payloads"
            );

            let full_again = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &full_authority,
                &scope,
                ToolRegistryMode::DeterministicMaximal,
            )
            .expect("full-authority cache hit");
            let empty_again = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &empty_authority,
                &scope,
                ToolRegistryMode::DeterministicMaximal,
            )
            .expect("empty-authority cache hit");
            assert_eq!(
                serialize_tools_payload(&full),
                serialize_tools_payload(&full_again)
            );
            assert_eq!(
                serialize_tools_payload(&empty),
                serialize_tools_payload(&empty_again)
            );
            let full_names = full["tools"]
                .as_array()
                .expect("full tools")
                .iter()
                .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
                .collect::<BTreeSet<_>>();
            let empty_names = empty["tools"]
                .as_array()
                .expect("empty tools")
                .iter()
                .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
                .collect::<BTreeSet<_>>();
            assert_ne!(
                full_names, empty_names,
                "capability isolation must change the advertised name set"
            );

            let mut without_configuration = scope.clone();
            without_configuration.remove(&ScopeDimension::ConfigurationLayer);
            let scoped = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &full_authority,
                &without_configuration,
                ToolRegistryMode::DeterministicMaximal,
            )
            .expect("scope-restricted payload");
            assert_ne!(
                serialize_tools_payload(&full),
                serialize_tools_payload(&scoped),
                "different available-scope sets must produce distinct payloads"
            );
            let scoped_again = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &full_authority,
                &without_configuration,
                ToolRegistryMode::DeterministicMaximal,
            )
            .expect("scope-restricted cache hit");
            assert_eq!(
                serialize_tools_payload(&scoped),
                serialize_tools_payload(&scoped_again)
            );
            let scoped_names = scoped["tools"]
                .as_array()
                .expect("scoped tools")
                .iter()
                .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
                .collect::<BTreeSet<_>>();
            assert_ne!(
                full_names, scoped_names,
                "scope isolation must change the advertised name set"
            );
        });
    }

    #[test]
    fn default_advertisement_is_cheaper_than_the_full_handshake() {
        let (profile, authority, scope) = default_discovery_inputs();
        let full = advertised_catalog_discovery_tools_list_payload_with_mode(
            Some(0),
            3,
            &profile,
            &authority,
            &scope,
            ToolRegistryMode::HostAvailable,
            crate::ToolListAdvertisement::Full,
        )
        .expect("full handshake");
        let default = advertised_catalog_discovery_tools_list_payload_with_mode(
            Some(0),
            3,
            &profile,
            &authority,
            &scope,
            ToolRegistryMode::HostAvailable,
            crate::ToolListAdvertisement::Default,
        )
        .expect("default handshake");
        let full_names = full["tools"]
            .as_array()
            .expect("full tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<BTreeSet<_>>();
        let default_names = default["tools"]
            .as_array()
            .expect("default tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<BTreeSet<_>>();
        assert!(default_names.contains(TOOL_SEARCH_TOOL_NAME));
        assert!(default_names.contains("tracedecay_search"));
        assert!(!default_names.contains("tracedecay_impact"));
        assert!(full_names.contains("tracedecay_impact"));
        assert!(default_names.is_subset(&full_names));
        let full_tokens = crate::tool_list_approx_tokens(&full).expect("full tokens");
        let default_tokens = crate::tool_list_approx_tokens(&default).expect("default tokens");
        assert!(
            default_tokens * 4 < full_tokens,
            "default={default_tokens} tools={} must be far cheaper than full={full_tokens} tools={}",
            default_names.len(),
            full_names.len()
        );
    }

    fn fixture_tool_definition(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: format!("{name} description"),
            input_schema: json!({ "type": "object" }),
            annotations: None,
            meta: None,
        }
    }

    #[test]
    fn tool_search_names_missing_names_and_truncates_only_real_omissions() {
        let definitions = vec![
            fixture_tool_definition("tracedecay_alpha"),
            fixture_tool_definition("tracedecay_alpine"),
            fixture_tool_definition("tracedecay_beta"),
        ];

        // Two matches against limit=2 return the whole set: no omission, no
        // truncation claim.
        let exact = execute_tool_search_within(
            None,
            &json!({ "query": "alp", "limit": 2, "format": "json" }),
            definitions.clone(),
        )
        .expect("exact-limit query");
        let exact_payload = exact.structured_result().expect("structured exact");
        assert_eq!(exact_payload["total"], 2);
        assert_eq!(exact_payload["returned"], 2);
        assert_eq!(
            exact_payload["truncated"], false,
            "an exact-limit query must not claim truncation: {exact_payload}"
        );

        let cut = execute_tool_search_within(
            None,
            &json!({ "query": "alp", "limit": 1, "format": "json" }),
            definitions.clone(),
        )
        .expect("cut query");
        assert_eq!(
            cut.structured_result().expect("structured cut")["truncated"],
            true,
            "a query that drops real matches must claim truncation"
        );

        let loaded = execute_tool_search_within(
            None,
            &json!({ "names": ["tracedecay_alpha", "tracedecay_typo"], "format": "json" }),
            definitions,
        )
        .expect("partial load");
        let loaded_payload = loaded.structured_result().expect("structured load");
        assert_eq!(
            loaded_payload["missing_names"],
            json!(["tracedecay_typo"]),
            "an unresolvable name must be reported, not dropped: {loaded_payload}"
        );
        assert_eq!(loaded_payload["returned"], 1);
    }

    #[test]
    fn tool_search_reaches_every_catalog_filtered_tool() {
        let catalog =
            execute_tool_search(None, &json!({ "format": "json" })).expect("catalog search");
        let payload = catalog.structured_result().expect("structured catalog");
        assert_eq!(payload["mode"], "catalog");
        let names = payload["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        let (profile, authority, scope) = default_discovery_inputs();
        let full = catalog_discovery_tools_list_payload(
            Some(0),
            3,
            &profile,
            &authority,
            &scope,
            ToolRegistryMode::HostAvailable,
        )
        .expect("full compose");
        let reachable = full["tools"]
            .as_array()
            .expect("full tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names, reachable,
            "an empty tool search must name every catalog-filtered tool"
        );
        assert!(
            names.len() > 20,
            "catalog search must not collapse to the default handshake: {names:?}"
        );

        let loaded = execute_tool_search(
            None,
            &json!({
                "names": ["tracedecay_retrieve"],
                "format": "json"
            }),
        )
        .expect("load retrieve");
        let loaded_payload = loaded.structured_result().expect("structured load");
        assert_eq!(loaded_payload["mode"], "load");
        assert_eq!(loaded_payload["tools"][0]["name"], "tracedecay_retrieve");
        assert!(
            loaded_payload["tools"][0]["input_schema"]
                .as_object()
                .is_some_and(|schema| schema.contains_key("properties")),
            "loading a deferred tool must return its full schema: {loaded_payload}"
        );

        let ranked = execute_tool_search(
            None,
            &json!({
                "query": "impact",
                "format": "json"
            }),
        )
        .expect("relevance search");
        let ranked_payload = ranked.structured_result().expect("structured query");
        assert_eq!(ranked_payload["mode"], "query");
        let first = ranked_payload["tools"][0]["name"]
            .as_str()
            .expect("first match");
        assert!(
            first.contains("impact"),
            "impact query must rank an impact tool first: {ranked_payload}"
        );
    }
}

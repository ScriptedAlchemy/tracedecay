//! Live `tools/list` filtering against the application-surface catalog.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, RwLock};

use crate::{
    ToolDefinition, context_description, context_warming_description, get_tool_definitions,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_tool_catalog::{CapabilityId, FeatureId, ProfileId, ScopeDimension};

use super::dispatch::McpDispatchMetadataError;

/// The stdio proxy's session-scoped discovery tool, outside the daemon catalog.
pub const TOOL_SEARCH_NAME: &str = "tracedecay_tool_search";

/// Documented ceiling for the process-wide discovery cache.
///
/// The live key space is profile × capability-set digest × scope digest.
/// Production callers share one default profile and one project scope; tests
/// add a handful of capability-set variants. Budget and node count are patched
/// onto a hit and are not keys.
const DISCOVERY_CACHE_MAX_ENTRIES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DiscoveryCacheKey {
    profile_digest: [u8; 32],
    capabilities_digest: [u8; 32],
    scopes_digest: [u8; 32],
}

/// Only the serialized `{"tools": [...]}` payload is cached: every
/// production read serves it, and keeping the definitions it was serialized
/// from as well held the catalog twice.
static DISCOVERY_CACHE: LazyLock<RwLock<HashMap<DiscoveryCacheKey, Arc<Value>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Drops every cached discovery entry so isolation cases do not inherit
/// another test's capability or scope payload.
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
) -> DiscoveryCacheKey {
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
    }
}

/// Node-independent compose: tool definitions, catalog filter, and
/// dispatch-contract metadata. Context descriptions are patched on serve.
fn compose_node_independent_definitions(
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
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
    let mut definitions = get_tool_definitions()?
        .into_iter()
        .filter(|definition| {
            !catalog_operations.contains(&definition.name)
                || visible_operations.contains(&definition.name)
        })
        .collect::<Vec<_>>();
    super::dispatch::attach_dispatch_metadata(&mut definitions)?;
    Ok(definitions)
}

fn discovery_cache_get_or_insert(
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
) -> Result<Arc<Value>, McpDispatchMetadataError> {
    let key = discovery_cache_key(profile_id, authorized_capabilities, available_scope);
    if let Ok(cache) = DISCOVERY_CACHE.read()
        && let Some(entry) = cache.get(&key)
    {
        return Ok(Arc::clone(entry));
    }
    let tools =
        compose_node_independent_definitions(profile_id, authorized_capabilities, available_scope)?;
    let entry = Arc::new(serde_json::json!({ "tools": tools }));
    match DISCOVERY_CACHE.write() {
        Ok(mut cache) => {
            if let Some(published) = cache.get(&key) {
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
) -> Result<Vec<ToolDefinition>, McpDispatchMetadataError> {
    let mut payload = catalog_discovery_tools_list_payload(
        Some(node_count),
        budget,
        profile_id,
        authorized_capabilities,
        available_scope,
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
) -> Result<Value, McpDispatchMetadataError> {
    let entry =
        discovery_cache_get_or_insert(profile_id, authorized_capabilities, available_scope)?;
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
    use crate::{explore_call_budget, project_catalog_discovery_scope};

    #[test]
    fn catalog_filtered_discovery_advertises_dispatch_contracts() {
        let profile_id = ProfileId::new(tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID)
            .expect("default profile");
        let definitions = get_catalog_filtered_tool_definitions_with_budget(
            0,
            explore_call_budget(0),
            &profile_id,
            &default_catalog_discovery_authority().expect("default discovery authority"),
            &project_catalog_discovery_scope(),
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

    fn advertised_tool<'a>(payload: &'a Value, name: &str) -> &'a Value {
        payload["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} must be advertised"))
    }

    fn advertises(payload: &Value, name: &str) -> bool {
        payload["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .any(|tool| tool["name"] == name)
    }

    fn assert_rewrite_is_advertised(payload: &Value) {
        let rewrite = advertised_tool(payload, "tracedecay_ast_grep_rewrite");
        assert_eq!(
            rewrite["inputSchema"]["required"],
            serde_json::json!(["path", "pattern", "rewrite"])
        );
        assert_eq!(
            rewrite["_meta"]["tracedecay/dispatch"]["effect"],
            "source_edit"
        );
    }

    fn with_isolated_discovery_cache<T>(body: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_catalog_discovery_cache_for_test();
        body()
    }

    #[test]
    fn discovery_advertises_rewrite_and_the_requested_context_budget() {
        let (profile, authority, scope) = default_discovery_inputs();
        let payload =
            catalog_discovery_tools_list_payload(Some(6_000), 4, &profile, &authority, &scope)
                .expect("discovery payload");
        assert_rewrite_is_advertised(&payload);
        assert!(
            advertised_tool(&payload, "tracedecay_context")["description"]
                .as_str()
                .expect("context description")
                .contains("This project (6000 nodes) allows 4 broad context calls."),
            "{}",
            advertised_tool(&payload, "tracedecay_context")["description"]
        );
    }

    #[test]
    fn distinct_authorized_capability_sets_never_share_a_cache_entry() {
        with_isolated_discovery_cache(|| {
            let (profile, full_authority, scope) = default_discovery_inputs();
            let empty_authority = BTreeSet::new();
            let full =
                catalog_discovery_tools_list_payload(Some(0), 3, &profile, &full_authority, &scope)
                    .expect("full-authority payload");
            let empty = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &empty_authority,
                &scope,
            )
            .expect("empty-authority payload");
            assert_rewrite_is_advertised(&full);
            assert!(advertises(&full, "tracedecay_search"));
            assert!(advertises(&full, "tracedecay_configuration_get"));
            assert!(!advertises(&empty, "tracedecay_search"));
            assert!(advertises(&empty, "tracedecay_multi_root_execute"));

            let full_again =
                catalog_discovery_tools_list_payload(Some(0), 3, &profile, &full_authority, &scope)
                    .expect("full-authority cache hit");
            let empty_again = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &empty_authority,
                &scope,
            )
            .expect("empty-authority cache hit");
            assert_rewrite_is_advertised(&full_again);
            assert!(advertises(&full_again, "tracedecay_search"));
            assert!(!advertises(&empty_again, "tracedecay_search"));
            assert!(advertises(&empty_again, "tracedecay_multi_root_execute"));

            let mut without_configuration = scope.clone();
            without_configuration.remove(&ScopeDimension::ConfigurationLayer);
            let scoped = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &full_authority,
                &without_configuration,
            )
            .expect("scope-restricted payload");
            assert_rewrite_is_advertised(&scoped);
            assert!(!advertises(&scoped, "tracedecay_configuration_get"));
            let scoped_again = catalog_discovery_tools_list_payload(
                Some(0),
                3,
                &profile,
                &full_authority,
                &without_configuration,
            )
            .expect("scope-restricted cache hit");
            assert_rewrite_is_advertised(&scoped_again);
            assert!(!advertises(&scoped_again, "tracedecay_configuration_get"));
        });
    }
}

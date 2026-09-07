//! Live `tools/list` filtering against the root application-surface catalog.
//!
//! Catalog assembly lives in `tracedecay-mcp`. These wrappers stay in the
//! composition root because they name `application_surface` and attach
//! dispatch metadata from the daemon-coupled binding table.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, RwLock};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_mcp::{
    ToolDefinition, ToolRegistryMode, ast_grep_available, context_description,
    context_warming_description, get_maximal_tool_definitions,
    retain_host_available_tool_definitions,
};
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

struct DiscoveryCacheEntry {
    tools: Arc<Vec<ToolDefinition>>,
    payload: Arc<Value>,
}

static DISCOVERY_CACHE: LazyLock<RwLock<HashMap<DiscoveryCacheKey, Arc<DiscoveryCacheEntry>>>> =
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
    hotpath::gauge!("mcp.catalog.discovery.cache_hits").inc(1u64);
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
) -> Result<Arc<DiscoveryCacheEntry>, McpDispatchMetadataError> {
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
    let tools = Arc::new(compose_node_independent_definitions(
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?);
    let payload = Arc::new(serde_json::json!({ "tools": tools.as_ref() }));
    let entry = Arc::new(DiscoveryCacheEntry { tools, payload });
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

fn apply_context_description(definitions: &mut [ToolDefinition], description: &str) {
    for definition in definitions {
        if definition.name == "tracedecay_context" {
            description.clone_into(&mut definition.description);
        }
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
    let entry = discovery_cache_get_or_insert(
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?;
    let mut definitions = (*entry.tools).clone();
    apply_context_description(
        &mut definitions,
        &context_description_for(Some(node_count), budget),
    );
    Ok(definitions)
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
    let mut payload = (*entry.payload).clone();
    patch_context_description_in_payload(
        &mut payload,
        context_description_for(node_count, budget),
    )?;
    Ok(payload)
}

pub fn get_catalog_filtered_tool_definitions_with_warming_budget(
    budget: u8,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Vec<ToolDefinition>, McpDispatchMetadataError> {
    let entry = discovery_cache_get_or_insert(
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?;
    let mut definitions = (*entry.tools).clone();
    apply_context_description(&mut definitions, &context_warming_description(budget));
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

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_mcp::{explore_call_budget, project_catalog_discovery_scope};

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

        let source_edit = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_ast_grep_rewrite")
            .expect("available source-edit handler is advertised");
        let source_edit_dispatch = &source_edit.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(source_edit_dispatch["effect"], "source_edit");
        assert_eq!(source_edit_dispatch["availability"]["state"], "available");
        assert_eq!(source_edit_dispatch["idempotency"], "key_required");

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
        assert_eq!(dispatch["effect"], "read");
        assert_eq!(dispatch["availability"]["state"], "available");
        assert!(dispatch.get("receipt").is_none());
        assert!(dispatch.get("reconciliation").is_none());

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
                .any(|definition| definition.name == "tracedecay_search"),
            "legacy production tools remain discoverable until cataloged"
        );
        assert!(
            definitions.iter().all(|definition| {
                definition.name != "tracedecay_context"
                    && definition.name != "tracedecay_git_preview"
            }),
            "catalog-bound tools require explicit capability authority"
        );
    }

    fn default_discovery_inputs() -> (ProfileId, BTreeSet<CapabilityId>, BTreeSet<ScopeDimension>) {
        (
            ProfileId::new(tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID)
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
        apply_context_description(&mut tools, &context_description_for(node_count, budget));
        Ok(serde_json::json!({ "tools": tools }))
    }

    fn with_discovery_counter_lock<T>(body: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_catalog_discovery_cache_for_test();
        reset_catalog_discovery_cache_hits_for_test();
        crate::mcp::tools::dispatch::reset_attach_dispatch_metadata_calls_for_test();
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
                crate::mcp::tools::dispatch::attach_dispatch_metadata_calls_for_test();
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
                crate::mcp::tools::dispatch::attach_dispatch_metadata_calls_for_test(),
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
    #[ignore = "measurement harness: timing evidence for #824, not an assertion"]
    fn measure_warm_cached_discovery_against_fresh_compose() {
        let (profile, authority, scope) = default_discovery_inputs();
        let mode = ToolRegistryMode::HostAvailable;
        let budget = explore_call_budget(0);
        let _ =
            catalog_discovery_tools_list_payload(None, budget, &profile, &authority, &scope, mode)
                .expect("warm cache");
        let mut cached = Vec::with_capacity(25);
        for _ in 0..25 {
            let started = std::time::Instant::now();
            let _ = catalog_discovery_tools_list_payload(
                None, budget, &profile, &authority, &scope, mode,
            )
            .expect("cached serve");
            cached.push(started.elapsed());
        }
        let mut fresh = Vec::with_capacity(25);
        for _ in 0..25 {
            let started = std::time::Instant::now();
            let _ = freshly_composed_tools_list_payload(
                None, budget, &profile, &authority, &scope, mode,
            )
            .expect("fresh compose");
            fresh.push(started.elapsed());
        }
        cached.sort();
        fresh.sort();
        let report = format!(
            "MEASURE #824 tools/list n=25 cached_p50={:?} cached_p95={:?} fresh_p50={:?} fresh_p95={:?}",
            cached[12], cached[23], fresh[12], fresh[23]
        );
        println!("{report}");
        std::fs::write(
            std::env::temp_dir().join("td-mcp-824-measure.txt"),
            report.as_bytes(),
        )
        .expect("write #824 measurement");
    }
}

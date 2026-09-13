//! Opt-in hotpath gauges for catalog load, resolution, and discovery.
//!
//! Keys are static capability names. Never pass tool names, capability IDs,
//! digests, or schema content. Every call is a no-op unless this crate's
//! `hotpath` feature is selected.

/// Size of a successfully assembled MCP dispatch catalog.
#[inline]
#[cfg(feature = "hotpath")]
pub(crate) fn mcp_catalog_entries(entries: usize) {
    hotpath::gauge!("tool_catalog.mcp.entries").set(entries as f64);
}

#[inline]
#[cfg(not(feature = "hotpath"))]
pub(crate) fn mcp_catalog_entries(_entries: usize) {}

/// Per-dispatch contract lookup outcome; a miss is recorded, never silent.
#[inline]
#[cfg(feature = "hotpath")]
pub(crate) fn mcp_contract_lookup(hit: bool) {
    if hit {
        hotpath::gauge!("tool_catalog.mcp.lookup_hits").inc(1.0);
    } else {
        hotpath::gauge!("tool_catalog.mcp.lookup_misses").inc(1.0);
    }
}

#[inline]
#[cfg(not(feature = "hotpath"))]
pub(crate) fn mcp_contract_lookup(_hit: bool) {}

/// Sizes of a successfully built catalog snapshot.
#[inline]
#[cfg(feature = "hotpath")]
pub(crate) fn snapshot_entries(capabilities: usize, bindings: usize, profiles: usize) {
    hotpath::gauge!("tool_catalog.snapshot.capabilities").set(capabilities as f64);
    hotpath::gauge!("tool_catalog.snapshot.bindings").set(bindings as f64);
    hotpath::gauge!("tool_catalog.snapshot.profiles").set(profiles as f64);
}

#[inline]
#[cfg(not(feature = "hotpath"))]
pub(crate) fn snapshot_entries(_capabilities: usize, _bindings: usize, _profiles: usize) {}

/// Binding resolution outcome. A miss deliberately covers unknown,
/// unavailable, feature-incompatible, profile-hidden, and
/// protocol-incompatible entries alike, mirroring the public contract.
#[inline]
#[cfg(feature = "hotpath")]
pub(crate) fn binding_resolution(resolved: bool) {
    if resolved {
        hotpath::gauge!("tool_catalog.resolve.hits").inc(1.0);
    } else {
        hotpath::gauge!("tool_catalog.resolve.misses").inc(1.0);
    }
}

#[inline]
#[cfg(not(feature = "hotpath"))]
pub(crate) fn binding_resolution(_resolved: bool) {}

/// Number of bindings published by the last discovery listing, including
/// empty listings for hidden or disabled surfaces.
#[inline]
#[cfg(feature = "hotpath")]
pub(crate) fn visible_bindings_published(count: usize) {
    hotpath::gauge!("tool_catalog.discovery.visible_bindings").set(count as f64);
}

#[inline]
#[cfg(not(feature = "hotpath"))]
pub(crate) fn visible_bindings_published(_count: usize) {}

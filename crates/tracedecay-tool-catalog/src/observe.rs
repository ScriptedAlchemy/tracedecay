//! Metrics gauges for catalog load, resolution, and discovery.
//!
//! Keys are static capability names. Never pass tool names, capability IDs,
//! digests, or schema content. Every call is a no-op unless this crate's
//! `metrics` recorder is installed.

/// Size of a successfully assembled MCP dispatch catalog.
#[inline]
pub(crate) fn mcp_catalog_entries(entries: usize) {}

/// Per-dispatch contract lookup outcome; a miss is recorded, never silent.
#[inline]
pub(crate) fn mcp_contract_lookup(hit: bool) {
    if hit {
    } else {
    }
}

/// Sizes of a successfully built catalog snapshot.
#[inline]
pub(crate) fn snapshot_entries(capabilities: usize, bindings: usize, profiles: usize) {
    {}
}

/// Binding resolution outcome. A miss deliberately covers unknown,
/// unavailable, feature-incompatible, profile-hidden, and
/// protocol-incompatible entries alike, mirroring the public contract.
#[inline]
pub(crate) fn binding_resolution(resolved: bool) {
    if resolved {
    } else {
    }
}

/// Number of bindings published by the last discovery listing, including
/// empty listings for hidden or disabled surfaces.
#[inline]
pub(crate) fn visible_bindings_published(count: usize) {}

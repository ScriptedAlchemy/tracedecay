//! The advertised MCP tool catalog, as host installers see it.
//!
//! A **registered port**. The catalog itself (`mcp::tools::definitions`, ~160
//! JSON-Schema descriptors plus host-capability filtering) stays in the root
//! crate; host installers here need only the four fields below, to write
//! permission allowlists, plugin manifests, and generated schema files.
//! Naming the root registry directly would invert the dependency.
//!
//! Root wiring: the root registers [`register`] with an adapter over
//! `mcp::tools::get_tool_definitions` and [`register_format_capable_names`]
//! with `mcp::tools::format_capable_tool_names` during startup, before any
//! install/update/doctor path runs.
//!
//! Unregistered, both readers answer empty. That is the correct inert answer
//! for every caller: installers then write no tool permissions rather than a
//! wrong set, and this crate's unit tests stay runnable without the root.

use std::sync::OnceLock;

use serde_json::Value;

/// One advertised MCP tool, reduced to what host installers consume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvertisedToolV1 {
    /// Bare tool name, without any host's `mcp__tracedecay__` prefix.
    pub name: String,
    /// Model-facing description, copied into generated host manifests.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: Value,
    /// The tool's `readOnlyHint` annotation, defaulted to `false`.
    ///
    /// Hosts that gate MCP calls behind per-call review use this to let the
    /// read-only subset run unattended, so a missing annotation must read as
    /// "not read-only" rather than as permission.
    pub read_only: bool,
}

/// Supplies the tools advertised on the current host.
pub type Catalog = fn() -> Vec<AdvertisedToolV1>;

/// Supplies the tool names whose output honours a `format` argument.
pub type FormatCapableNames = fn() -> &'static [&'static str];

static CATALOG: OnceLock<Catalog> = OnceLock::new();
static FORMAT_CAPABLE_NAMES: OnceLock<FormatCapableNames> = OnceLock::new();

/// Registers the root crate's advertised tool catalog.
///
/// Idempotent: the first registration wins, so concurrent daemon and CLI
/// initialisation cannot fight over it.
pub fn register(catalog: Catalog) {
    let _ = CATALOG.set(catalog);
}

/// Registers the root crate's format-capable tool-name list.
pub fn register_format_capable_names(names: FormatCapableNames) {
    let _ = FORMAT_CAPABLE_NAMES.set(names);
}

/// The tools advertised on this host, or empty when the root never registered.
#[must_use]
pub fn advertised_tools() -> Vec<AdvertisedToolV1> {
    CATALOG.get().map_or_else(Vec::new, |catalog| catalog())
}

/// The tool names whose output honours a `format` argument, or empty when the
/// root never registered.
#[must_use]
pub fn format_capable_tool_names() -> &'static [&'static str] {
    FORMAT_CAPABLE_NAMES.get().map_or(&[], |names| names())
}

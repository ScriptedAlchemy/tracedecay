//! Canonical MCP namespaces for tracedecay's own tools.
//!
//! Hosts expose the same tool under several namespaces depending on how
//! tracedecay was installed, so the prefixes live here once and every consumer
//! (permission allowlists in `agents::claude`, usage classification in
//! [`crate::analytics`]) reads them from this module instead of restating a
//! partial list.

/// Permission/tool prefix for the tracedecay tools exposed through the Claude
/// **plugin** MCP server. Claude namespaces a plugin server's tools as
/// `mcp__plugin_<pluginName>_<serverKey>__<tool>`; with plugin name
/// `tracedecay` and the server key `graph` (see `plugin/.mcp.json`), that
/// yields `mcp__plugin_tracedecay_graph__<tool>`. The server key is `graph`
/// rather than `tracedecay` so the host UI renders `plugin tracedecay graph`
/// instead of the redundant `plugin tracedecay tracedecay`.
pub const PLUGIN_TOOL_PREFIX: &str = "mcp__plugin_tracedecay_graph__";

/// Prior plugin namespace, from when the plugin MCP server key was also
/// `tracedecay` (`plugin_tracedecay_tracedecay`). Kept so pre-rename installs
/// are still recognized; entries under it are never removed.
pub const PRIOR_PLUGIN_TOOL_PREFIX: &str = "mcp__plugin_tracedecay_tracedecay__";

/// Legacy config-managed namespace. It does NOT match the plugin namespace, so
/// an install that wrote only these entries prompted interactively on every
/// plugin tool call; the installer now writes the plugin-namespace twins too.
pub const LEGACY_TOOL_PREFIX: &str = "mcp__tracedecay__";

/// Single-underscore namespace used by hosts that flatten the MCP separator.
pub const FLAT_TOOL_PREFIX: &str = "mcp_tracedecay_";

/// Every namespace a tracedecay tool call can arrive under, longest first so a
/// prefix that contains another is stripped whole.
pub const ALL_TOOL_PREFIXES: [&str; 4] = [
    PRIOR_PLUGIN_TOOL_PREFIX,
    PLUGIN_TOOL_PREFIX,
    LEGACY_TOOL_PREFIX,
    FLAT_TOOL_PREFIX,
];

/// Strips the host MCP namespace from a tracedecay tool name, leaving the bare
/// tool name. Names in no known namespace are returned unchanged.
pub fn strip_tool_prefix(name: &str) -> &str {
    ALL_TOOL_PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(name)
}

//! Canonical MCP namespaces for tracedecay's own tools.
//!
//! Hosts expose the same tool under several namespaces depending on how
//! tracedecay was installed. Permission allowlists read these prefixes.
//! Usage classification restates the same host namespaces as literals in
//! `tracedecay_automation::analytics` so that leaf does not depend on this crate.

/// Permission/tool prefix for the tracedecay tools exposed through the Claude
/// **plugin** MCP server. Claude namespaces a plugin server's tools as
/// `mcp__plugin_<pluginName>_<serverKey>__<tool>`; with plugin name
/// `tracedecay` and the server key `graph` (see `plugin/.mcp.json`), that
/// yields `mcp__plugin_tracedecay_graph__<tool>`. The server key is `graph`
/// rather than `tracedecay` so the host UI renders `plugin tracedecay graph`
/// instead of the redundant `plugin tracedecay tracedecay`.
pub const PLUGIN_TOOL_PREFIX: &str = "mcp__plugin_tracedecay_graph__";

/// Legacy config-managed namespace. It does NOT match the plugin namespace, so
/// an install that wrote only these entries prompted interactively on every
/// plugin tool call; the installer now writes the plugin-namespace twins too.
pub const LEGACY_TOOL_PREFIX: &str = "mcp__tracedecay__";

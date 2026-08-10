//! `tracedecay_simplify_scan` availability boundary.

use super::*;

/// The former scan mixed SQLite-only similarity scores with graph degree
/// queries. The verified projection intentionally publishes neither that
/// similarity authority nor an equivalent score, so the compound result must
/// fail closed until the canonical redundancy authority owns this journey.
pub(crate) async fn handle_simplify_scan(
    _cg: &TraceDecay,
    _args: Value,
    _scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    Err(info_graph_error(
        "verified-simplify-similarity-unavailable",
        "simplify scan requires a canonical similarity authority that is not published by the verified code generation",
    ))
}

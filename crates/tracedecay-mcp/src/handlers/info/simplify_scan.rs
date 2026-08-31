//! `tracedecay_simplify_scan` availability boundary.

use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::ToolResult;

/// The former scan mixed SQLite-only similarity scores with graph degree
/// queries. The verified projection intentionally publishes neither that
/// similarity authority nor an equivalent score, so the compound result must
/// fail closed until the canonical redundancy authority owns this journey.
#[hotpath::measure(future = true, label = "mcp.info.simplify_scan.total")]
pub async fn handle_simplify_scan() -> Result<ToolResult> {
    Err(TraceDecayError::project_route(
        "verified-simplify-similarity-unavailable",
        false,
        "simplify scan requires a canonical similarity authority that is not published by the verified code generation",
    ))
}

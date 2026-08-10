//! Wire decoding and error mapping shared across dashboard graph operations.
//!
//! These are the small, stateless conversions every operation in the parent
//! adapter reaches for: turning a raw `nodes`/`edges` row (as produced by
//! `queries`) into its typed read model (`decode_node`, `decode_edge`), the
//! relational-to-domain edge-kind vocabulary (`relation_kind_str`), the two
//! error constructors every fallible read funnels through
//! (`unavailable`, `map_graph_error`).

use serde_json::Value;
use tracedecay_application::{
    DashboardGraphEdgeV1, DashboardGraphNodeV1, DashboardGraphReadErrorV1, DashboardGraphSpanV1,
};
use tracedecay_domain::RelationEdgeKindV1;
use tracedecay_graph_db::GraphDbError;

/// Canonical relation kinds share the relational edge-kind vocabulary for
/// the kinds both sides define, so the served wire strings are stable across
/// the adjacency cutover.
pub(super) fn relation_kind_str(kind: RelationEdgeKindV1) -> &'static str {
    match kind {
        RelationEdgeKindV1::Calls => "calls",
        RelationEdgeKindV1::Uses => "uses",
        RelationEdgeKindV1::TypeOf => "type_of",
        RelationEdgeKindV1::Contains => "contains",
        RelationEdgeKindV1::Implements => "implements",
        RelationEdgeKindV1::Extends => "extends",
        RelationEdgeKindV1::Annotates => "annotates",
        RelationEdgeKindV1::Returns => "returns",
        RelationEdgeKindV1::Receives => "receives",
    }
}

pub(super) fn unavailable(detail: impl Into<String>) -> DashboardGraphReadErrorV1 {
    DashboardGraphReadErrorV1::Unavailable {
        detail: detail.into(),
    }
}

pub(super) fn map_graph_error(error: GraphDbError) -> DashboardGraphReadErrorV1 {
    match error {
        GraphDbError::Cancelled => DashboardGraphReadErrorV1::Cancelled,
        GraphDbError::DeadlineExceeded => DashboardGraphReadErrorV1::TimedOut,
        GraphDbError::InvalidRequest { message } => {
            DashboardGraphReadErrorV1::InvalidRequest { detail: message }
        }
        corrupt @ (GraphDbError::Corrupt { .. }
        | GraphDbError::ProjectionMismatch { .. }
        | GraphDbError::GenerationMismatch { .. }
        | GraphDbError::ResetRequired { .. }) => DashboardGraphReadErrorV1::Corrupt {
            detail: corrupt.to_string(),
        },
        other => DashboardGraphReadErrorV1::Unavailable {
            detail: other.to_string(),
        },
    }
}

pub(super) fn i64_field(row: &Value, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or(0)
}

pub(super) fn str_field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(super) fn decode_node(row: Value) -> Result<DashboardGraphNodeV1, DashboardGraphReadErrorV1> {
    let mut node: DashboardGraphNodeV1 =
        serde_json::from_value(row).map_err(|error| DashboardGraphReadErrorV1::Corrupt {
            detail: format!("dashboard graph node row is invalid: {error}"),
        })?;
    node.span = Some(DashboardGraphSpanV1 {
        start_line: node.start_line.unwrap_or(0),
        end_line: node.end_line.unwrap_or(0),
        start_column: node.start_column.unwrap_or(0),
        end_column: node.end_column.unwrap_or(0),
        attrs_start_line: node.attrs_start_line.unwrap_or(0),
    });
    Ok(node)
}

pub(super) fn decode_edge(row: Value) -> Result<DashboardGraphEdgeV1, DashboardGraphReadErrorV1> {
    serde_json::from_value(row).map_err(|error| DashboardGraphReadErrorV1::Corrupt {
        detail: format!("dashboard graph edge row is invalid: {error}"),
    })
}

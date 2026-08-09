//! Opaque GitHub stack-signal expansion tool schema.

use serde_json::json;

use super::application_schema::closed_object_schema;
use super::{ToolDefinition, def, string_property};

/// The hook wakeup carries no signal content or actor. This MCP operation is
/// where the daemon mints actor authority and expands one opaque signal ID.
pub(super) fn def_github_stack_signal_expand() -> ToolDefinition {
    def(
        "tracedecay_github_stack_signal_expand",
        "Expand GitHub stack signal",
        "Authorize and expand one durable GitHub stack signal by opaque identity. The daemon mints actor authority; callers cannot name recipients, queue state, or stack content.",
        closed_object_schema(
            json!({
                "signal_id": string_property("Opaque durable GitHub stack signal identity."),
                "expected_watermark_id": {
                    "type": ["string", "null"],
                    "description": "Optional exact host-delivery watermark guard."
                }
            }),
            &["signal_id"],
        ),
    )
}

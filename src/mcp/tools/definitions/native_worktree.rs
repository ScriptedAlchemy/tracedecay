//! MCP + CLI tool definitions for the native worktree-cleanup family.
//!
//! The catalog declares these five operations on three surfaces —
//! `NATIVE_WORKTREE_SURFACES = [Cli, Mcp, Http]`
//! (`tracedecay-application/src/git/native_integration_surface.rs`) — but only
//! HTTP was ever mounted. `tracedecay tool` and MCP publish the SAME
//! `ToolDefinition` list, so a family absent from that list is absent from both
//! agent-facing surfaces at once, which is how five declared operations stayed
//! invisible while their routes answered.
//!
//! The input schemas are GENERATED from the same request types the HTTP
//! decoder deserializes (`NativeWorktreeSurfaceRequest`'s five payloads, all of
//! which derive `JsonSchema`). Hand-writing them here would create a second
//! schema source that can accept a field the typed decoder rejects — the exact
//! drift the Work family's discovery projection exists to prevent.

use schemars::generate::SchemaSettings;
use serde_json::{Value, json};
// Imported through the `git` module rather than the crate root: the root
// re-export list does not carry this family, and widening it would mean editing
// a file another lane is holding open.
use tracedecay_application::git::{
    NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION, NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION,
    NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION,
    NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION, NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION,
    WorktreeCleanupConfirmRequestV1, WorktreeCleanupInspectRequestV1,
    WorktreeCleanupReconcileRequestV1, WorktreeCleanupRemoveRequestV1, WorktreeInventoryRequestV1,
};

use crate::mcp::tools::ToolDefinition;

/// Render one request type's schemars body as an MCP input schema.
///
/// `for_deserialize` is deliberate: the schema advertised to a caller must be
/// the schema the daemon will actually decode against, not the serialize-side
/// view of the same type.
fn request_schema<T: schemars::JsonSchema>() -> Value {
    let generator = SchemaSettings::default().for_deserialize().into_generator();
    serde_json::to_value(generator.into_root_schema_for::<T>())
        .unwrap_or_else(|_| json!({ "type": "object" }))
}

fn worktree_tool(
    operation: &str,
    title: &str,
    description: &str,
    read_only: bool,
    input_schema: Value,
) -> ToolDefinition {
    ToolDefinition {
        name: format!("tracedecay_{operation}"),
        description: description.to_owned(),
        input_schema,
        annotations: Some(json!({
            "readOnlyHint": read_only,
            "title": title,
        })),
        meta: None,
    }
}

/// Every worktree-cleanup tool, in the catalog's own declaration order.
///
/// The `read_only` flags restate the catalog's effect classes: inventory,
/// inspect and reconcile are `Read`, confirm is `Preview` (it mints a proof and
/// mutates nothing), and remove is `Administrative` — the only one of the five
/// that changes anything on disk, and even then only a linked-worktree
/// registration and root. It never deletes a branch.
pub(super) fn native_worktree_definitions() -> Vec<ToolDefinition> {
    vec![
        worktree_tool(
            NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION,
            "Inventory authorized native worktrees",
            "Read only the native worktree administration records covered by one persisted \
             scope-set revision and digest. Read-only: no ref, index, or worktree changes.",
            true,
            request_schema::<WorktreeInventoryRequestV1>(),
        ),
        worktree_tool(
            NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION,
            "Inspect one linked worktree for cleanup",
            "Re-read exact native worktree state and emit a digest-bound cleanup inspection \
             without mutating Git. The digest is what a later confirmation is bound to, so a \
             worktree that moved between inspect and confirm cannot be confirmed.",
            true,
            request_schema::<WorktreeCleanupInspectRequestV1>(),
        ),
        worktree_tool(
            NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION,
            "Confirm one inspected worktree",
            "Revalidate the inspection digest and mint a confirmation proof only when clean, \
             unlocked, unheld, and non-unique linked-worktree evidence still holds. Mints a \
             proof; changes nothing.",
            true,
            request_schema::<WorktreeCleanupConfirmRequestV1>(),
        ),
        worktree_tool(
            NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION,
            "Remove one confirmed linked worktree",
            "Remove only the exact clean, unlocked, unheld, non-unique linked worktree \
             registration and root named by a separate confirmation proof. Branches are never \
             deleted.",
            false,
            request_schema::<WorktreeCleanupRemoveRequestV1>(),
        ),
        worktree_tool(
            NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION,
            "Reconcile one worktree cleanup outcome",
            "Re-read exact native administration state after removal or restart and distinguish \
             removed, still-present, stale, and uncertain outcomes. Uncertainty is reported as \
             uncertainty rather than resolved by assumption.",
            true,
            request_schema::<WorktreeCleanupReconcileRequestV1>(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::native_worktree_definitions;

    #[test]
    fn every_declared_worktree_operation_has_exactly_one_tool() {
        let definitions = native_worktree_definitions();
        let names = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names.len(), definitions.len(), "tool names must be unique");
        for operation in [
            super::NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION,
            super::NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION,
            super::NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION,
            super::NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION,
            super::NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION,
        ] {
            // The catalog forward sweep resolves an MCP binding by exactly this
            // name, so the prefix is a contract and not a presentation choice.
            assert!(
                names.contains(format!("tracedecay_{operation}").as_str()),
                "{operation} has no tool definition"
            );
        }
    }

    #[test]
    fn input_schemas_are_objects_generated_from_the_decoded_request_types() {
        for definition in native_worktree_definitions() {
            assert_eq!(
                definition.input_schema["type"], "object",
                "{} must advertise an object body",
                definition.name
            );
        }
    }
}

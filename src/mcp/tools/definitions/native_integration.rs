//! MCP tool definitions for the Plan 36 native-integration journey.
//!
//! These live in their own module rather than in `application.rs` so the
//! journey's schemas evolve without touching the shared Git/feedback
//! definitions. The five tools mirror the application catalog exactly:
//! `stack_snapshot`, `preflight_native_integration`, `apply_native_integration`,
//! `native_integration_status`, and `cancel_native_integration`.
//!
//! Every input is exact typed identity. There is no property that accepts a
//! filesystem path, a branch display name, a free-form SHA, a patch, a commit
//! message, a merge strategy, a remote, or a Git argument, so this surface
//! cannot be widened into generic Git execution.

use serde_json::json;

use super::{def, def_rw, required_object_schema, string_property};
use crate::mcp::tools::ToolDefinition;

fn digest_property(description: &str) -> serde_json::Value {
    json!({
        "type": "string",
        "pattern": "^sha256:[0-9a-f]{64}$",
        "description": description
    })
}

/// The exact authorized root pair plus one declared-edge or independent-branch
/// binding. Both roots must resolve to the same proven repository.
fn snapshot_binding_properties() -> serde_json::Value {
    json!({
        "source": {
            "type": "object",
            "description": "Exact authorized source scope: project, repository, worktree, and full ref."
        },
        "destination": {
            "type": "object",
            "description": "Exact authorized destination scope in the same proven repository."
        },
        "authorized_scope_set_digest": digest_property(
            "Digest of the authorized multi-root scope set the selection was taken from."
        ),
        "inventory_snapshot_id": string_property(
            "Frozen worktree inventory snapshot identity."
        ),
        "inventory_epoch": {
            "type": "integer",
            "minimum": 1,
            "description": "Monotonic inventory epoch frozen with the snapshot."
        },
        "selection": {
            "type": "object",
            "description": "Either an exact declared stack edge (stack, revision, node pair, direction) or an independent-branch proposal digest. Branch names, paths, and provider topology cannot select an edge."
        },
        "grant_digest": digest_property("Capability grant revision digest."),
        "policy_digest": digest_property("Policy revision digest.")
    })
}

const SNAPSHOT_REQUIRED: &[&str] = &[
    "source",
    "destination",
    "authorized_scope_set_digest",
    "inventory_snapshot_id",
    "inventory_epoch",
    "selection",
    "grant_digest",
    "policy_digest",
];

pub(super) fn def_stack_snapshot() -> ToolDefinition {
    def(
        "tracedecay_stack_snapshot",
        "Freeze a branch-stack selection",
        "Reauthorize and freeze one exact authorized branch-stack edge or independent-branch pair, \
         with its repository tips and inventory epoch, into the immutable snapshot identity that \
         native-integration preflight consumes. Read-only: no ref, index, or worktree changes.",
        required_object_schema(snapshot_binding_properties(), SNAPSHOT_REQUIRED),
    )
}

pub(super) fn def_preflight_native_integration() -> ToolDefinition {
    def(
        "tracedecay_preflight_native_integration",
        "Preflight a native integration",
        "Compute one immutable native-integration preview from a frozen snapshot in a private \
         daemon-owned index and object directory. Proves the real refs, index, and worktrees are \
         unchanged, and classifies the result as eligible, conflicted, review-required, partial, \
         stale, denied, or unavailable. There is no auto-resolution and no policy override.",
        required_object_schema(
            json!({
                "snapshot": {
                    "type": "object",
                    "description": "The exact frozen snapshot binding returned by stack_snapshot."
                },
                "evidence": {
                    "type": "object",
                    "description": "Exact graph, test, schema, and migration revision digests joined to native conflict evidence."
                },
                "preferred_mode": {
                    "type": ["string", "null"],
                    "enum": ["fast_forward", "two_parent_merge", "cherry_pick_exact_commits", null],
                    "description": "Optional selection among the three fixed mechanical encodings. It cannot change topology, commit order, or the commit set."
                }
            }),
            &["snapshot", "evidence"],
        ),
    )
}

pub(super) fn def_apply_native_integration() -> ToolDefinition {
    def_rw(
        "tracedecay_apply_native_integration",
        "Apply an approved native integration",
        "Apply exactly one unexpired native-integration preview under a one-use content-bound \
         approval through the daemon transaction, returning one terminal receipt proving \
         committed, unchanged, rolled back, or inspection-required state. Arbitrary Git inputs, \
         messages, paths, commit lists, remotes, and history rewriting are not accepted.",
        required_object_schema(
            json!({
                "preview_id": string_property("Opaque preview identity returned by preflight_native_integration."),
                "preview_digest": digest_property("Exact immutable preview digest returned by preflight_native_integration."),
                "approval_id": string_property("Identity of the one-use approval issued for exactly this preview."),
                "approval_digest": digest_property("Exact content-bound approval digest."),
                "transaction_id": string_property("Caller-stable transaction identity for idempotent replay and recovery.")
            }),
            &[
                "preview_id",
                "preview_digest",
                "approval_id",
                "approval_digest",
                "transaction_id",
            ],
        ),
    )
}

pub(super) fn def_native_integration_status() -> ToolDefinition {
    def(
        "tracedecay_native_integration_status",
        "Read native-integration status",
        "Read the durable phase, cancellation request, and terminal outcome of one \
         native-integration transaction, including its inspectable receipt identity.",
        required_object_schema(
            json!({
                "transaction_id": string_property("Native-integration transaction identity.")
            }),
            &["transaction_id"],
        ),
    )
}

pub(super) fn def_cancel_native_integration() -> ToolDefinition {
    def_rw(
        "tracedecay_cancel_native_integration",
        "Cancel a native integration",
        "Request cancellation of one native-integration transaction. Cancellation before the \
         native commit point leaves state unchanged; after the commit point the committed receipt \
         is returned instead of claiming cancellation.",
        required_object_schema(
            json!({
                "transaction_id": string_property("Native-integration transaction identity.")
            }),
            &["transaction_id"],
        ),
    )
}

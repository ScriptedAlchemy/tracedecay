//! MCP tool definitions for the Plan 36 native-integration journey.
//!
//! These live in their own module rather than in `application.rs` so the
//! journey's schemas evolve without touching the shared Git/feedback
//! definitions. The six tools mirror the application catalog exactly:
//! `stack_snapshot`, `preflight_native_integration`,
//! `approve_native_integration`, `apply_native_integration`,
//! `native_integration_status`, and `cancel_native_integration`.
//!
//! Every input is exact typed identity. There is no property that accepts a
//! filesystem path, a branch display name, a free-form SHA, a patch, a commit
//! message, a merge strategy, a remote, or a Git argument, so this surface
//! cannot be widened into generic Git execution.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use tracedecay_application::git::{
    NativeIntegrationApplySurfaceRequest, NativeIntegrationApproveSurfaceRequest,
    NativeIntegrationCancelSurfaceRequest, NativeIntegrationPreflightSurfaceRequest,
    NativeIntegrationStackSnapshotSurfaceRequest, NativeIntegrationStatusSurfaceRequest,
};

use super::{def, def_rw};
use crate::mcp::tools::ToolDefinition;

fn request_schema<T: JsonSchema>() -> serde_json::Value {
    let generator = SchemaSettings::default().for_deserialize().into_generator();
    generator.into_root_schema_for::<T>().into()
}

pub(super) fn def_stack_snapshot() -> ToolDefinition {
    def(
        "tracedecay_stack_snapshot",
        "Freeze a branch-stack selection",
        "Reauthorize and freeze one exact authorized branch-stack edge or independent-branch pair, \
         with its repository tips and inventory epoch, into the immutable snapshot identity that \
         native-integration preflight consumes. Read-only: no ref, index, or worktree changes.",
        request_schema::<NativeIntegrationStackSnapshotSurfaceRequest>(),
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
        request_schema::<NativeIntegrationPreflightSurfaceRequest>(),
    )
}

pub(super) fn def_approve_native_integration() -> ToolDefinition {
    def_rw(
        "tracedecay_approve_native_integration",
        "Approve a native-integration preview",
        "Issue one one-use content-bound approval for exactly one unexpired, mechanically \
         eligible native-integration preview. The daemon binds the approval to the requesting \
         principal, the apply capability, the current grant lineage, and the preview's expiry; \
         approving an identity without its exact content digest is unrepresentable.",
        request_schema::<NativeIntegrationApproveSurfaceRequest>(),
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
        request_schema::<NativeIntegrationApplySurfaceRequest>(),
    )
}

pub(super) fn def_native_integration_status() -> ToolDefinition {
    def(
        "tracedecay_native_integration_status",
        "Read native-integration status",
        "Read the durable phase, cancellation request, and terminal outcome of one \
         native-integration transaction, including its inspectable receipt identity.",
        request_schema::<NativeIntegrationStatusSurfaceRequest>(),
    )
}

pub(super) fn def_cancel_native_integration() -> ToolDefinition {
    def_rw(
        "tracedecay_cancel_native_integration",
        "Cancel a native integration",
        "Request cancellation of one native-integration transaction. Cancellation before the \
         native commit point leaves state unchanged; after the commit point the committed receipt \
         is returned instead of claiming cancellation.",
        request_schema::<NativeIntegrationCancelSurfaceRequest>(),
    )
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use schemars::generate::SchemaSettings;
    use tracedecay_application::git::{
        NativeIntegrationApplySurfaceRequest, NativeIntegrationApproveSurfaceRequest,
        NativeIntegrationCancelSurfaceRequest, NativeIntegrationPreflightSurfaceRequest,
        NativeIntegrationStackSnapshotSurfaceRequest, NativeIntegrationStatusSurfaceRequest,
    };

    fn canonical_schema<T: JsonSchema>() -> serde_json::Value {
        let generator = SchemaSettings::default().for_deserialize().into_generator();
        generator.into_root_schema_for::<T>().into()
    }

    #[test]
    fn every_native_integration_input_schema_is_generated_from_its_decoded_request() {
        assert_eq!(
            super::def_stack_snapshot().input_schema,
            canonical_schema::<NativeIntegrationStackSnapshotSurfaceRequest>()
        );
        assert_eq!(
            super::def_preflight_native_integration().input_schema,
            canonical_schema::<NativeIntegrationPreflightSurfaceRequest>()
        );
        assert_eq!(
            super::def_approve_native_integration().input_schema,
            canonical_schema::<NativeIntegrationApproveSurfaceRequest>()
        );
        assert_eq!(
            super::def_apply_native_integration().input_schema,
            canonical_schema::<NativeIntegrationApplySurfaceRequest>()
        );
        assert_eq!(
            super::def_native_integration_status().input_schema,
            canonical_schema::<NativeIntegrationStatusSurfaceRequest>()
        );
        assert_eq!(
            super::def_cancel_native_integration().input_schema,
            canonical_schema::<NativeIntegrationCancelSurfaceRequest>()
        );
    }
}

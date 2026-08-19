//! Direct surface proof for the Plan 36 native-integration journey.
//!
//! Plan 36 slice 1 requires `stack_snapshot` and `preflight_native_integration`
//! on "the shipped application and CLI/MCP surfaces", slice 3 adds
//! `apply_native_integration`, `native_integration_status`, and
//! `cancel_native_integration`, and slice 4 requires the whole journey to be
//! exposed consistently through CLI and MCP over one application result.
//! The same surface later mounted the explicit-root worktree inventory and
//! cleanup operations; those bind CLI, MCP, and HTTP, while the transaction
//! journey still withholds HTTP because apply has no transport fallback.
//!
//! This suite proves the journey is *mounted and callable* end to end at the
//! surface boundary: canonical operation identity, catalog bindings on both
//! CLI and MCP, an advertised MCP tool per operation, typed request decoding
//! that rejects a mismatched or path-bearing request, and a truthful typed
//! result envelope whose unavailable and cancellation states advance nothing.
//!
//! The full multi-repository apply journey (Plan 36 "Direct acceptance") needs
//! a mounted native-integration runtime and is deliberately not attempted here.

use std::collections::BTreeSet;

use serde_json::json;
use tracedecay::application_surface::{
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, parse_application_surface_request,
    resolve_application_surface_dispatch, resolve_catalog_tool_binding,
};
use tracedecay::daemon_client::RequestedOutputFormat;
use tracedecay::mcp::tools::get_tool_definitions;
use tracedecay_application::{
    NativeIntegrationSurfaceResultV1, NativeIntegrationSurfaceUnavailableV1,
};
use tracedecay_application::{
    RequestId, native_integration_surface_catalog_contribution,
    native_integration_surface_handler_descriptors, native_integration_surface_operation,
};
use tracedecay_tool_catalog::{BindingSurface, CatalogContributionV1};

/// The transaction journey, restated here as the reverse authority. Deriving
/// it from the module under test would let a dropped operation pass vacuously.
const JOURNEY: [(ApplicationSurfaceOperation, &str); 6] = [
    (
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot,
        "stack_snapshot",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationPreflight,
        "preflight_native_integration",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationApprove,
        "approve_native_integration",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationApply,
        "apply_native_integration",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationStatus,
        "native_integration_status",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationCancel,
        "cancel_native_integration",
    ),
];

/// Explicit-root worktree inventory/cleanup mounted on the same surface.
const WORKTREE_JOURNEY: [(ApplicationSurfaceOperation, &str); 5] = [
    (
        ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory,
        "worktree_inventory",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect,
        "worktree_cleanup_inspect",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm,
        "worktree_cleanup_confirm",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove,
        "worktree_cleanup_remove",
    ),
    (
        ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile,
        "worktree_cleanup_reconcile",
    ),
];

#[test]
fn every_journey_operation_has_canonical_identity() {
    for (operation, name) in JOURNEY.iter().chain(WORKTREE_JOURNEY.iter()) {
        assert_eq!(operation.as_str(), *name);
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name(&format!("tracedecay_{name}")),
            Some(*operation),
            "{name} does not resolve from its MCP tool name"
        );
        assert!(
            ApplicationSurfaceOperation::ALL.contains(operation),
            "{name} is missing from the canonical operation authority"
        );
    }
}

#[test]
fn every_journey_operation_binds_to_cli_and_mcp_and_withholds_http() {
    let contribution =
        native_integration_surface_catalog_contribution().expect("catalog contribution");
    let descriptors = native_integration_surface_handler_descriptors().expect("descriptors");
    assert_eq!(
        descriptors.len(),
        JOURNEY.len() + WORKTREE_JOURNEY.len(),
        "handler descriptors must cover the transaction journey and the worktree journey"
    );

    for (operation, name) in JOURNEY {
        assert_cli_and_mcp_bindings(&contribution, name);
        // Apply is an authoritative native mutation and this journey has no
        // transport fallback, so HTTP stays deliberately unexposed.
        assert!(
            !operation.is_http_exposed(),
            "{name} must not be exposed over HTTP"
        );
        assert!(
            native_integration_surface_operation(name)
                .expect("operation resolution")
                .is_some(),
            "{name} resolves to no application operation"
        );
    }
    for (operation, name) in WORKTREE_JOURNEY {
        assert_cli_and_mcp_bindings(&contribution, name);
        assert!(
            operation.is_http_exposed(),
            "{name} is the read/admin worktree journey and must stay on HTTP"
        );
        assert!(
            contribution.bindings().iter().any(|binding| {
                binding.operation().as_str() == name && binding.surface() == BindingSurface::Http
            }),
            "{name} declares no HTTP binding"
        );
        assert!(
            native_integration_surface_operation(name)
                .expect("operation resolution")
                .is_some(),
            "{name} resolves to no application operation"
        );
    }
}

/// The dashboard consumes the read-only status projection over the same
/// application result; every mutating transaction operation stays off the
/// dashboard so no gateway can advance a transaction, apply edits, or mutate
/// Git from it.
#[test]
fn only_the_status_read_carries_a_dashboard_binding() {
    let contribution =
        native_integration_surface_catalog_contribution().expect("catalog contribution");
    for (_, name) in JOURNEY {
        let declares_dashboard = contribution.bindings().iter().any(|binding| {
            binding.operation().as_str() == name
                && binding.surface() == BindingSurface::Dashboard
        });
        assert_eq!(
            declares_dashboard,
            name == "native_integration_status",
            "{name} dashboard exposure must match the read-only status contract"
        );
    }
    let resolved = resolve_catalog_tool_binding(
        BindingSurface::Dashboard,
        "tracedecay_native_integration_status",
    )
    .expect("dashboard binding resolution");
    assert!(
        resolved.is_some(),
        "the status dashboard binding is declared but the production resolver answers nothing"
    );
}

fn assert_cli_and_mcp_bindings(contribution: &CatalogContributionV1, name: &str) {
    for surface in [BindingSurface::Cli, BindingSurface::Mcp] {
        assert!(
            contribution.bindings().iter().any(|binding| {
                binding.operation().as_str() == name && binding.surface() == surface
            }),
            "{name} declares no {surface:?} binding"
        );
        let resolved = resolve_catalog_tool_binding(surface, &format!("tracedecay_{name}"))
            .expect("binding resolution");
        assert!(
            resolved.is_some(),
            "{name} is declared for {surface:?} but the production resolver answers nothing"
        );
    }
}

#[test]
fn every_journey_operation_is_an_advertised_mcp_tool() {
    let advertised = get_tool_definitions()
        .expect("tool definitions")
        .into_iter()
        .map(|definition| definition.name)
        .collect::<BTreeSet<_>>();
    for (_, name) in JOURNEY.iter().chain(WORKTREE_JOURNEY.iter()) {
        assert!(
            advertised.contains(&format!("tracedecay_{name}")),
            "tracedecay_{name} is not advertised by the MCP tool registry"
        );
    }
}

/// A minimally valid `stack_snapshot` body. Only exact typed identity appears:
/// there is no path, branch display name, free-form SHA, or Git argument.
fn stack_snapshot_body() -> serde_json::Value {
    let digest = format!("sha256:{}", "ab".repeat(32));
    json!({
        "source": {
            "project_id": "project.alpha",
            "repository_id": "repository.alpha",
            "worktree_id": "worktree.source",
            "reference": "refs/heads/source",
            "scope_digest": digest
        },
        "destination": {
            "project_id": "project.alpha",
            "repository_id": "repository.alpha",
            "worktree_id": "worktree.destination",
            "reference": "refs/heads/destination",
            "scope_digest": digest
        },
        "authorized_scope_set_id": "scope-set.alpha",
        "authorized_scope_set_revision": 1,
        "authorized_scope_set_digest": digest,
        "inventory_snapshot_id": "inventory.snapshot.1",
        "inventory_epoch": 7,
        "selection": {
            "kind": "independent_branch",
            "binding": {"proposal_digest": digest}
        },
        "grant_digest": digest,
        "policy_digest": digest
    })
}

#[test]
fn stack_snapshot_decodes_into_the_typed_journey_request() {
    let request = parse_application_surface_request(
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot,
        stack_snapshot_body(),
    )
    .expect("stack_snapshot request");
    let ApplicationSurfaceRequest::NativeIntegration(_) = &request else {
        panic!("stack_snapshot did not decode into the native-integration family");
    };
    // The decoded request reaches a real dispatch binding rather than a
    // declaration alone.
    resolve_application_surface_dispatch(
        BindingSurface::Mcp,
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot,
        RequestId::new("request.native-integration.stack-snapshot").expect("request id"),
        request,
        RequestedOutputFormat::Json,
    )
    .expect("stack_snapshot dispatch");
}

#[test]
fn a_journey_request_cannot_carry_an_unknown_or_path_bearing_field() {
    let mut body = stack_snapshot_body();
    body["repository_path"] = json!("/tmp/alpha");
    assert!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::NativeIntegrationStackSnapshot,
            body,
        )
        .is_err(),
        "a path-bearing stack_snapshot body must be rejected, not silently ignored"
    );
}

#[test]
fn a_journey_request_cannot_be_submitted_under_another_operation() {
    // A `stack_snapshot` body under the apply operation must not decode: apply
    // accepts only an exact preview identity plus a one-use approval.
    assert!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::NativeIntegrationApply,
            stack_snapshot_body(),
        )
        .is_err(),
        "apply must not accept a snapshot body"
    );
    // Approval issuance accepts only the exact preview identity/digest pair;
    // a snapshot body or an approval body without the content digest must be
    // rejected rather than partially decoded.
    assert!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::NativeIntegrationApprove,
            stack_snapshot_body(),
        )
        .is_err(),
        "approve must not accept a snapshot body"
    );
    assert!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::NativeIntegrationApprove,
            json!({"preview_id": "preview.native-integration.example"}),
        )
        .is_err(),
        "approve must not accept a preview identity without its content digest"
    );
}

#[test]
fn typed_unavailable_and_cancellation_states_advance_nothing() {
    for reason in [
        NativeIntegrationSurfaceUnavailableV1::AuthorityUnmounted,
        NativeIntegrationSurfaceUnavailableV1::Denied,
        NativeIntegrationSurfaceUnavailableV1::Stale,
        NativeIntegrationSurfaceUnavailableV1::Partial,
        NativeIntegrationSurfaceUnavailableV1::ApprovalConflict,
        NativeIntegrationSurfaceUnavailableV1::NeedsInspection,
    ] {
        let result = NativeIntegrationSurfaceResultV1::unavailable(reason);
        assert!(
            !result.is_advancing(),
            "{reason:?} must never report durable advancement"
        );
        let encoded = serde_json::to_value(&result).expect("encode");
        assert_eq!(encoded["outcome"], "unavailable");
        let decoded: NativeIntegrationSurfaceResultV1 =
            serde_json::from_value(encoded).expect("decode");
        assert_eq!(decoded, result, "the typed result must round-trip exactly");
    }
}

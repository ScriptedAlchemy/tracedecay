use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_api::is_http_application_operation_exposed;
use tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID;
use tracedecay_contracts::{
    NativeIntegrationSurfaceResultV1, NativeIntegrationSurfaceUnavailableV1,
};
use tracedecay_contracts::{
    RequestId, native_integration_surface_catalog_contribution,
    native_integration_surface_handler_descriptors, native_integration_surface_operation,
};
use tracedecay_daemon_protocol::RequestedOutputFormat;
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceRequest, parse_application_surface_request,
};
use tracedecay_daemon_service::application_surface::{
    application_surface_catalog_ref, resolve_application_surface_dispatch,
};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, CatalogContributionV1, ProfileId,
    SurfaceOperationName,
};

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
            !is_http_application_operation_exposed(operation).expect("HTTP exposure registry"),
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
            is_http_application_operation_exposed(operation).expect("HTTP exposure registry"),
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
            binding.operation().as_str() == name && binding.surface() == BindingSurface::Dashboard
        });
        assert_eq!(
            declares_dashboard,
            name == "native_integration_status",
            "{name} dashboard exposure must match the read-only status contract"
        );
    }
    assert!(
        production_catalog_resolves(BindingSurface::Dashboard, "native_integration_status"),
        "the status dashboard binding is declared but the production resolver answers nothing"
    );
}

/// Whether the daemon's composed catalog resolves `operation` on `surface`
/// for the default profile at the production protocol revision.
fn production_catalog_resolves(surface: BindingSurface, operation: &str) -> bool {
    application_surface_catalog_ref()
        .expect("application catalog")
        .resolve_binding(
            &ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("default profile"),
            surface,
            &SurfaceOperationName::new(operation).expect("operation name"),
            1,
            &BTreeSet::new(),
        )
        .is_some()
}

fn assert_cli_and_mcp_bindings(contribution: &CatalogContributionV1, name: &str) {
    for surface in [BindingSurface::Cli, BindingSurface::Mcp] {
        assert!(
            contribution.bindings().iter().any(|binding| {
                binding.operation().as_str() == name && binding.surface() == surface
            }),
            "{name} declares no {surface:?} binding"
        );
        assert!(
            production_catalog_resolves(surface, name),
            "{name} is declared for {surface:?} but the production resolver answers nothing"
        );
    }
}

/// A minimally valid declared-stack body. The caller supplies visible topology
/// and typed commit identities; canonical order and digest stay daemon-owned.
fn stack_snapshot_body() -> serde_json::Value {
    let digest = format!("sha256:{}", "ab".repeat(32));
    let source_tip = "1".repeat(40);
    let destination_tip = "2".repeat(40);
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
            "kind": "declared_stack_edge",
            "binding": {
                "stack_id": "stack.alpha",
                "revision_id": "stack-revision.alpha.1",
                "nodes": [
                    {
                        "node_id": "node.destination",
                        "project_id": "project.alpha",
                        "repository_id": "repository.alpha",
                        "reference": "refs/heads/destination",
                        "tip": destination_tip,
                        "worktree_id": "worktree.destination"
                    },
                    {
                        "node_id": "node.source",
                        "project_id": "project.alpha",
                        "repository_id": "repository.alpha",
                        "reference": "refs/heads/source",
                        "tip": source_tip,
                        "worktree_id": "worktree.source"
                    }
                ],
                "edges": [{
                    "dependency": "node.source",
                    "dependent": "node.destination"
                }],
                "source_node_id": "node.source",
                "destination_node_id": "node.destination",
                "direction": "propagate_dependency_to_dependent"
            }
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

fn surface_refusal(operation: ApplicationSurfaceOperation, body: Value) -> String {
    match parse_application_surface_request(operation, body) {
        Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest { detail }) => detail,
        Err(other) => panic!("{operation:?} refused with the wrong kind: {other}"),
        Ok(_) => panic!("{operation:?} must refuse this body"),
    }
}

#[test]
fn stack_snapshot_rejects_caller_supplied_canonical_revision_fields() {
    let mut body = stack_snapshot_body();
    body["selection"]["binding"]["canonical_order"] = json!(["node.source"]);
    body["selection"]["binding"]["digest"] = json!(format!("sha256:{}", "cd".repeat(32)));
    let detail = surface_refusal(
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot,
        body,
    );
    assert!(
        detail.starts_with("unknown field `canonical_order`"),
        "canonical order and digest must be derived by the daemon: {detail}"
    );
}

#[test]
fn a_journey_request_cannot_carry_an_unknown_or_path_bearing_field() {
    let mut body = stack_snapshot_body();
    body["repository_path"] = json!("/tmp/alpha");
    let detail = surface_refusal(
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot,
        body,
    );
    assert!(
        detail.starts_with("unknown field `repository_path`"),
        "a path-bearing stack_snapshot body must be rejected, not silently ignored: {detail}"
    );
}

#[test]
fn a_journey_request_cannot_be_submitted_under_another_operation() {
    // A `stack_snapshot` body under the apply operation must not decode: apply
    // accepts only an exact preview identity plus a one-use approval.
    let detail = surface_refusal(
        ApplicationSurfaceOperation::NativeIntegrationApply,
        stack_snapshot_body(),
    );
    assert!(
        detail.starts_with("unknown field `authorized_scope_set_digest`"),
        "apply must not accept a snapshot body: {detail}"
    );
    // Approval issuance accepts only the exact preview identity/digest pair;
    // a snapshot body or an approval body without the content digest must be
    // rejected rather than partially decoded.
    let detail = surface_refusal(
        ApplicationSurfaceOperation::NativeIntegrationApprove,
        stack_snapshot_body(),
    );
    assert!(
        detail.starts_with(
            "unknown field `authorized_scope_set_digest`, expected `preview_id` or `preview_digest`"
        ),
        "approve must not accept a snapshot body: {detail}"
    );
    let detail = surface_refusal(
        ApplicationSurfaceOperation::NativeIntegrationApprove,
        json!({"preview_id": "preview.native-integration.example"}),
    );
    assert_eq!(
        detail, "missing field `preview_digest`",
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

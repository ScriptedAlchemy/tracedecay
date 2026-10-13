//! Lifecycle journeys for the one MCP route Claude, Cursor and Codex share
//! between the canonical `context-mcp` component and the legacy
//! `operator-mcp` selection.

use std::path::{Path, PathBuf};

use tracedecay_agent_hosts::agents::host_bundle::{
    HostArtifactActionV1, HostBundleLifecycleOpV1, HostComponentV1, HostKindV1,
    latest_host_component_receipt_at,
};
use tracedecay_runtime_core::config::ProfileRoot;

use super::{
    ComponentSetApplyContext, HostBundleCliOperation, HostLifecycleError, HostLifecycleResult,
    resolved_lifecycle_root, run_host_component_lifecycle,
};
use crate::cli::{HostBundleCliOptions, HostBundleComponentArg};

const CURSOR_MCP: &str = ".cursor/plugins/local/tracedecay/mcp.json";
const CURSOR_CORE_MANIFEST: &str = ".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json";
const OLD_BIN: &str = "/opt/tracedecay-old/bin/tracedecay";
const MID_BIN: &str = "/opt/tracedecay-mid/bin/tracedecay";
const NEW_BIN: &str = "/opt/tracedecay-new/bin/tracedecay";

fn run(
    home: &Path,
    operation: HostBundleCliOperation,
    component: Option<HostBundleComponentArg>,
    tracedecay_bin: &str,
) -> Result<(), String> {
    let profile = ProfileRoot::under_home(home);
    match run_host_component_lifecycle(
        &profile,
        "cursor",
        operation,
        &HostBundleCliOptions {
            component,
            dry_run: false,
            yes: true,
            adopt: false,
        },
        home,
        &resolved_lifecycle_root(&profile),
        &ComponentSetApplyContext::with_tracedecay_bin(tracedecay_bin),
    ) {
        Ok(HostLifecycleResult::Applied) => Ok(()),
        Ok(other) => Err(format!("{other:?}")),
        Err(HostLifecycleError::Failed(error)) => Err(error.to_string()),
        Err(HostLifecycleError::Absent { detail, .. }) => Err(detail),
    }
}

/// Every MCP identity whose live receipt claims `relative_path`.
fn live_mcp_claims(home: &Path, relative_path: &str) -> Vec<HostComponentV1> {
    let lifecycle_root = resolved_lifecycle_root(&ProfileRoot::under_home(home));
    [HostComponentV1::ContextMcp, HostComponentV1::OperatorMcp]
        .into_iter()
        .filter(|component| {
            latest_host_component_receipt_at(&lifecycle_root, HostKindV1::CursorDesktop, *component)
                .expect("receipt reads")
                .is_some_and(|receipt| {
                    receipt.operation != HostBundleLifecycleOpV1::Uninstall
                        && receipt
                            .artifacts
                            .iter()
                            .any(|artifact| artifact.relative_path == relative_path)
                })
        })
        .collect()
}

/// A released binary installed Core and then the optional `operator-mcp`
/// component, each through its own `--component` run.
fn install_legacy_operator_mcp(home: &Path) {
    run(
        home,
        HostBundleCliOperation::Install,
        Some(HostBundleComponentArg::Core),
        OLD_BIN,
    )
    .unwrap();
    run(
        home,
        HostBundleCliOperation::Install,
        Some(HostBundleComponentArg::OperatorMcp),
        OLD_BIN,
    )
    .unwrap();
    assert_eq!(
        live_mcp_claims(home, CURSOR_MCP),
        [HostComponentV1::OperatorMcp]
    );
}

#[test]
fn legacy_operator_mcp_install_updates_under_one_owner() {
    let home = tempfile::tempdir().unwrap();
    install_legacy_operator_mcp(home.path());

    run(home.path(), HostBundleCliOperation::Update, None, NEW_BIN)
        .expect("a plain update converges a legacy operator-mcp install");

    assert_eq!(
        live_mcp_claims(home.path(), CURSOR_MCP),
        [HostComponentV1::ContextMcp]
    );
    let mcp = std::fs::read_to_string(home.path().join(CURSOR_MCP)).unwrap();
    assert!(mcp.contains(NEW_BIN), "{mcp}");
    assert!(!mcp.contains(OLD_BIN), "{mcp}");
}

#[test]
fn plain_uninstall_removes_a_legacy_operator_mcp_route() {
    let home = tempfile::tempdir().unwrap();
    install_legacy_operator_mcp(home.path());

    run(
        home.path(),
        HostBundleCliOperation::Uninstall,
        None,
        OLD_BIN,
    )
    .unwrap();

    assert!(!home.path().join(CURSOR_MCP).exists());
    assert!(!home.path().join(CURSOR_CORE_MANIFEST).exists());
    assert!(live_mcp_claims(home.path(), CURSOR_MCP).is_empty());
}

#[test]
fn legacy_operator_mcp_selection_uninstalls_only_its_route() {
    let home = tempfile::tempdir().unwrap();
    install_legacy_operator_mcp(home.path());

    run(
        home.path(),
        HostBundleCliOperation::Uninstall,
        Some(HostBundleComponentArg::OperatorMcp),
        OLD_BIN,
    )
    .unwrap();

    assert!(!home.path().join(CURSOR_MCP).exists());
    assert!(home.path().join(CURSOR_CORE_MANIFEST).is_file());
    assert!(live_mcp_claims(home.path(), CURSOR_MCP).is_empty());
}

fn receipt_path(home: &Path, component_slug: &str) -> PathBuf {
    resolved_lifecycle_root(&ProfileRoot::under_home(home))
        .join(".tracedecay-host-bundle-v1")
        .join(format!("receipt.cursor-desktop.{component_slug}.v1.json"))
}

/// What a released binary's `--component operator-mcp` run left beside a
/// default install: a legacy receipt claiming the route the `context-mcp`
/// receipt claims, with the legacy component's own manifest digest.
fn seed_legacy_receipt_beside_context_mcp(home: &Path) {
    let lifecycle_root = resolved_lifecycle_root(&ProfileRoot::under_home(home));
    let mut legacy = latest_host_component_receipt_at(
        &lifecycle_root,
        HostKindV1::CursorDesktop,
        HostComponentV1::ContextMcp,
    )
    .unwrap()
    .expect("default install owns the MCP route");
    legacy.component = HostComponentV1::OperatorMcp;
    legacy.manifest_digest = [7; 32];
    for artifact in &mut legacy.artifacts {
        artifact.ownership_marker = artifact
            .ownership_marker
            .replace("context-mcp", "operator-mcp");
    }
    std::fs::write(
        receipt_path(home, "operator-mcp"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
}

fn dual_claim_install(home: &Path) {
    run(home, HostBundleCliOperation::Install, None, OLD_BIN).unwrap();
    seed_legacy_receipt_beside_context_mcp(home);
    assert_eq!(
        live_mcp_claims(home, CURSOR_MCP),
        [HostComponentV1::ContextMcp, HostComponentV1::OperatorMcp]
    );
}

#[test]
fn dual_claim_update_retires_the_legacy_receipt() {
    let home = tempfile::tempdir().unwrap();
    dual_claim_install(home.path());

    run(home.path(), HostBundleCliOperation::Update, None, NEW_BIN).unwrap();

    assert_eq!(
        live_mcp_claims(home.path(), CURSOR_MCP),
        [HostComponentV1::ContextMcp]
    );
    assert!(!receipt_path(home.path(), "operator-mcp").exists());
    let mcp = std::fs::read_to_string(home.path().join(CURSOR_MCP)).unwrap();
    assert!(mcp.contains(NEW_BIN), "{mcp}");
}

/// The legacy receipt records the bytes on disk while the `context-mcp`
/// receipt still records an older render, so only the legacy receipt proves
/// ownership of what is deployed.
#[test]
fn dual_claim_divergent_digests_update_converges() {
    let home = tempfile::tempdir().unwrap();
    run(home.path(), HostBundleCliOperation::Install, None, OLD_BIN).unwrap();
    let old_context_receipt = std::fs::read(receipt_path(home.path(), "context-mcp")).unwrap();
    run(home.path(), HostBundleCliOperation::Update, None, MID_BIN).unwrap();
    seed_legacy_receipt_beside_context_mcp(home.path());
    std::fs::write(
        receipt_path(home.path(), "context-mcp"),
        old_context_receipt,
    )
    .unwrap();

    run(home.path(), HostBundleCliOperation::Update, None, NEW_BIN)
        .expect("the receipt matching disk proves ownership");

    assert_eq!(
        live_mcp_claims(home.path(), CURSOR_MCP),
        [HostComponentV1::ContextMcp]
    );
    assert!(!receipt_path(home.path(), "operator-mcp").exists());
    let mcp = std::fs::read_to_string(home.path().join(CURSOR_MCP)).unwrap();
    assert!(mcp.contains(NEW_BIN), "{mcp}");
}

#[test]
fn dual_claim_operator_uninstall_leaves_no_dangling_owner() {
    let home = tempfile::tempdir().unwrap();
    dual_claim_install(home.path());

    run(
        home.path(),
        HostBundleCliOperation::Uninstall,
        Some(HostBundleComponentArg::OperatorMcp),
        OLD_BIN,
    )
    .unwrap();

    assert!(!home.path().join(CURSOR_MCP).exists());
    assert!(live_mcp_claims(home.path(), CURSOR_MCP).is_empty());
    assert!(home.path().join(CURSOR_CORE_MANIFEST).is_file());
}

/// The dry run of a plain update over a legacy install refreshes the route
/// the legacy receipt owns; it never reports that as adopting a foreign file.
#[test]
fn dry_run_labels_a_legacy_owned_route_as_replaced() {
    let home = tempfile::tempdir().unwrap();
    install_legacy_operator_mcp(home.path());

    let owned = super::receipt_owned_paths(
        &resolved_lifecycle_root(&ProfileRoot::under_home(home.path())),
        HostKindV1::CursorDesktop,
        HostComponentV1::ContextMcp,
    );

    assert_eq!(
        super::artifact_disposition(&HostArtifactActionV1::Replace, &owned, CURSOR_MCP),
        "replace"
    );
}

//! Lifecycle journeys for the one MCP route Claude, Cursor and Codex share
//! between the canonical `context-mcp` component and the legacy
//! `operator-mcp` selection.

use std::path::{Path, PathBuf};

use tracedecay_agent_hosts::agents::host_bundle::{
    HostArtifactActionV1, HostBundleLifecycleOpV1, HostComponentV1, HostKindV1,
    latest_host_component_receipt_at,
};
use tracedecay_agent_hosts::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin;
use tracedecay_runtime_core::config::ProfileRoot;

use super::{
    ComponentSetApplyContext, HostBundleCliOperation, HostLifecycleError, HostLifecycleResult,
    apply_canonical_component_set, canonical_host_component_set, component_is_not_applicable,
    resolved_lifecycle_root, run_host_component_lifecycle,
};
use crate::cli::{HostBundleCliOptions, HostBundleComponentArg};

const CURSOR_MCP: &str = ".cursor/plugins/local/tracedecay/mcp.json";
const CURSOR_CORE_MANIFEST: &str = ".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json";
const OLD_BIN: &str = "/opt/tracedecay-old/bin/tracedecay";
const MID_BIN: &str = "/opt/tracedecay-mid/bin/tracedecay";
const NEW_BIN: &str = "/opt/tracedecay-new/bin/tracedecay";

fn options(component: Option<HostBundleComponentArg>) -> HostBundleCliOptions {
    HostBundleCliOptions {
        component,
        dry_run: false,
        yes: true,
        adopt: false,
    }
}

fn applied(result: Result<HostLifecycleResult, HostLifecycleError>) -> Result<(), String> {
    match result {
        Ok(HostLifecycleResult::Applied) => Ok(()),
        Ok(other) => Err(format!("{other:?}")),
        Err(HostLifecycleError::Failed(error)) => Err(error.to_string()),
        Err(HostLifecycleError::Absent { detail, .. }) => Err(detail),
    }
}

fn run(
    home: &Path,
    operation: HostBundleCliOperation,
    component: Option<HostBundleComponentArg>,
    tracedecay_bin: &str,
) -> Result<(), String> {
    let profile = ProfileRoot::under_home(home);
    applied(run_host_component_lifecycle(
        &profile,
        "cursor",
        operation,
        &options(component),
        home,
        &resolved_lifecycle_root(&profile),
        &ComponentSetApplyContext::with_tracedecay_bin(tracedecay_bin),
    ))
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
/// component, each through its own `--component` run. Such a binary planned
/// the catalog's own `operator-mcp` set.
fn install_legacy_operator_mcp(home: &Path) {
    run(
        home,
        HostBundleCliOperation::Install,
        Some(HostBundleComponentArg::Core),
        OLD_BIN,
    )
    .unwrap();
    let profile = ProfileRoot::under_home(home);
    let legacy = verified_embedded_host_component_set_with_tracedecay_bin(
        HostKindV1::CursorDesktop,
        &[HostComponentV1::OperatorMcp],
        0,
        OLD_BIN,
        crate::product_runtime::PRODUCT_FULL_SHA,
    )
    .unwrap();
    applied(apply_canonical_component_set(
        &profile,
        "cursor",
        HostBundleCliOperation::Install,
        &legacy,
        &options(Some(HostBundleComponentArg::OperatorMcp)),
        home,
        &resolved_lifecycle_root(&profile),
        &ComponentSetApplyContext::with_tracedecay_bin(OLD_BIN),
    ))
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

/// Names of the MCP servers in Cursor's plugin `mcp.json` that launch
/// `tracedecay_bin`.
fn servers_launching(home: &Path, tracedecay_bin: &str) -> Vec<String> {
    let mcp: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(CURSOR_MCP)).unwrap()).unwrap();
    mcp["mcpServers"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, server)| server["command"] == tracedecay_bin)
        .map(|(name, _)| name.clone())
        .collect()
}

/// `--component operator-mcp` is the retired name of the `context-mcp` route:
/// a fresh selection writes the canonical receipt and serves the same single
/// server the canonical selection serves.
#[test]
fn operator_mcp_selection_installs_the_canonical_route() {
    let legacy_named = tempfile::tempdir().unwrap();
    let canonical = tempfile::tempdir().unwrap();
    for (home, selection) in [
        (legacy_named.path(), HostBundleComponentArg::OperatorMcp),
        (canonical.path(), HostBundleComponentArg::ContextMcp),
    ] {
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
            Some(selection),
            OLD_BIN,
        )
        .unwrap();
    }

    assert_eq!(
        live_mcp_claims(legacy_named.path(), CURSOR_MCP),
        [HostComponentV1::ContextMcp]
    );
    assert!(!receipt_path(legacy_named.path(), "operator-mcp").exists());
    assert_eq!(
        servers_launching(legacy_named.path(), OLD_BIN),
        ["tracedecay"]
    );
    assert_eq!(
        std::fs::read(legacy_named.path().join(CURSOR_MCP)).unwrap(),
        std::fs::read(canonical.path().join(CURSOR_MCP)).unwrap(),
        "both selections serve the same server"
    );
}

#[test]
fn operator_mcp_selection_updates_a_legacy_install_under_one_owner() {
    let home = tempfile::tempdir().unwrap();
    install_legacy_operator_mcp(home.path());

    run(
        home.path(),
        HostBundleCliOperation::Update,
        Some(HostBundleComponentArg::OperatorMcp),
        NEW_BIN,
    )
    .expect("the retired selection updates the route it always named");

    assert_eq!(
        live_mcp_claims(home.path(), CURSOR_MCP),
        [HostComponentV1::ContextMcp]
    );
    assert!(!receipt_path(home.path(), "operator-mcp").exists());
    assert_eq!(servers_launching(home.path(), NEW_BIN), ["tracedecay"]);
    assert!(servers_launching(home.path(), OLD_BIN).is_empty());
}

/// A legacy install whose `mcp.json` was deleted is recovered by a plain
/// repair, which leaves the canonical identity as the route's only owner.
#[test]
fn plain_repair_recovers_a_drifted_legacy_operator_mcp_route() {
    let home = tempfile::tempdir().unwrap();
    install_legacy_operator_mcp(home.path());
    std::fs::remove_file(home.path().join(CURSOR_MCP)).unwrap();

    run(home.path(), HostBundleCliOperation::Repair, None, OLD_BIN)
        .expect("repair restores the route the legacy receipt owned");

    assert_eq!(servers_launching(home.path(), OLD_BIN), ["tracedecay"]);
    assert_eq!(
        live_mcp_claims(home.path(), CURSOR_MCP),
        [HostComponentV1::ContextMcp]
    );
    assert!(!receipt_path(home.path(), "operator-mcp").exists());
}

/// The retired name keeps its old reach: the hosts that offered it plan
/// `context-mcp`, and every other host still skips or refuses it rather than
/// gaining an MCP route through it.
#[test]
fn operator_mcp_selection_keeps_its_host_scope() {
    for agent in ["claude", "cursor", "codex"] {
        let selected =
            canonical_host_component_set(agent, Some(HostBundleComponentArg::OperatorMcp), 0)
                .unwrap()
                .unwrap();
        let components = selected
            .component_set
            .components
            .iter()
            .map(|component| component.manifest.component)
            .collect::<Vec<_>>();
        assert_eq!(components, [HostComponentV1::ContextMcp], "{agent}");
        assert!(!component_is_not_applicable(
            agent,
            HostBundleComponentArg::OperatorMcp
        ));
    }
    for agent in ["cline", "opencode", "kiro"] {
        assert!(
            component_is_not_applicable(agent, HostBundleComponentArg::OperatorMcp),
            "{agent}"
        );
        assert!(
            canonical_host_component_set(agent, Some(HostBundleComponentArg::OperatorMcp), 0)
                .is_err(),
            "{agent}"
        );
    }
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

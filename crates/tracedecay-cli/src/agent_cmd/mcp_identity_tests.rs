//! Lifecycle journeys for the one MCP route Claude, Cursor and Codex share
//! between the canonical `context-mcp` component and the legacy
//! `operator-mcp` selection.

use std::path::Path;

use tracedecay_agent_hosts::agents::host_bundle::{
    HostBundleLifecycleOpV1, HostComponentV1, HostKindV1, latest_host_component_receipt_at,
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

    assert_eq!(live_mcp_claims(home.path(), CURSOR_MCP).len(), 1);
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

//! Hermes dashboard plugin-page deployment tests.
//!
//! `tokensave install --agent hermes` deploys the dashboard wrapper
//! (manifest.json + plugin_api.py + dist bundles) into the generated
//! plugin's `dashboard/` subdirectory, where Hermes' dashboard-plugin
//! discovery (`plugins/*/dashboard/manifest.json`) picks it up. These tests
//! cover the deploy itself, idempotent reinstall with pin preservation, the
//! `--no-dashboard` opt-out, and uninstall cleanup.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use tokensave::agents::{AgentIntegration, HermesIntegration, InstallContext};

fn make_ctx(home: &Path, dashboard: bool) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: Vec::new(),
        profile: None,
        project_root: None,
        dashboard,
    }
}

fn dashboard_dir(home: &Path) -> PathBuf {
    home.join(".hermes/plugins/tokensave/dashboard")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

const DIST_FILES: &[&str] = &[
    "index.js",
    "holographic.js",
    "lcm.js",
    "graph.js",
    "savings.js",
    "style.css",
];

#[test]
fn install_deploys_dashboard_plugin_page() {
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    let dash = dashboard_dir(home.path());
    for file in DIST_FILES {
        assert!(
            dash.join("dist").join(file).is_file(),
            "missing deployed dist file {file}"
        );
    }

    // Manifest is discoverable and stamped with the generating version.
    let manifest: serde_json::Value =
        serde_json::from_str(&read(&dash.join("manifest.json"))).unwrap();
    assert_eq!(manifest["name"], "tokensave");
    assert_eq!(manifest["label"], "TokenSave");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["entry"], "dist/index.js");
    assert_eq!(manifest["css"], "dist/style.css");
    // `api` must stay a relative path inside dashboard/ — Hermes rejects
    // absolute/traversal api paths (GHSA-5qr3-c538-wm9j).
    assert_eq!(manifest["api"], "plugin_api.py");

    // The proxy backend bakes in the installing binary (env still wins).
    let api = read(&dash.join("plugin_api.py"));
    assert!(api.contains(r#"DEPLOYED_TOKENSAVE_BIN = "/usr/local/bin/tokensave""#));
    // Unpinned installs serve the profile home's `.tokensave/` stores (the
    // hermes_profile storage scope), not whatever cwd Hermes spawns from.
    let encoded_home =
        serde_json::to_string(home.path().join(".hermes").to_string_lossy().as_ref()).unwrap();
    assert!(api.contains(&format!("DEPLOYED_PROJECT_ROOT = {encoded_home}")));
    assert!(api.contains("router = APIRouter()"));
}

#[test]
fn install_with_project_root_pins_dashboard_project() {
    let home = tempfile::tempdir().unwrap();
    let mut ctx = make_ctx(home.path(), true);
    ctx.project_root = Some(PathBuf::from("/pinned/project"));
    HermesIntegration.install(&ctx).unwrap();

    let api = read(&dashboard_dir(home.path()).join("plugin_api.py"));
    assert!(api.contains(r#"DEPLOYED_PROJECT_ROOT = "/pinned/project""#));
}

#[test]
fn reinstall_is_idempotent_and_preserves_pin() {
    let home = tempfile::tempdir().unwrap();
    let mut pinned = make_ctx(home.path(), true);
    pinned.project_root = Some(PathBuf::from("/pinned/project"));
    HermesIntegration.install(&pinned).unwrap();

    let dash = dashboard_dir(home.path());
    let first_api = read(&dash.join("plugin_api.py"));
    let first_manifest = read(&dash.join("manifest.json"));

    // Reinstall WITHOUT an explicit pin: the existing pin must survive
    // (mirrors the agent plugin's tools.py/config pin preservation).
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    assert_eq!(read(&dash.join("plugin_api.py")), first_api);
    assert_eq!(read(&dash.join("manifest.json")), first_manifest);
    assert!(
        read(&dash.join("plugin_api.py")).contains(r#"DEPLOYED_PROJECT_ROOT = "/pinned/project""#)
    );
}

#[test]
fn no_dashboard_skips_deploy() {
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), false))
        .unwrap();

    assert!(
        !dashboard_dir(home.path()).exists(),
        "--no-dashboard must not deploy the dashboard directory"
    );
    // The agent plugin itself still installs.
    assert!(home
        .path()
        .join(".hermes/plugins/tokensave/plugin.yaml")
        .is_file());
}

#[test]
fn no_dashboard_removes_previous_deploy() {
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();
    assert!(dashboard_dir(home.path()).join("manifest.json").is_file());

    HermesIntegration
        .install(&make_ctx(home.path(), false))
        .unwrap();
    assert!(
        !dashboard_dir(home.path()).exists(),
        "--no-dashboard reinstall must remove the previously deployed page"
    );
}

#[test]
fn uninstall_removes_dashboard_deploy() {
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    HermesIntegration
        .uninstall(&make_ctx(home.path(), true))
        .unwrap();

    assert!(!dashboard_dir(home.path()).exists());
    assert!(
        !home.path().join(".hermes/plugins/tokensave").exists(),
        "plugin dir should be fully removed once the dashboard is cleaned up"
    );
}

#[test]
fn uninstall_leaves_foreign_files_in_dashboard_dir() {
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    let foreign = dashboard_dir(home.path()).join("user-notes.txt");
    std::fs::write(&foreign, "mine").unwrap();

    HermesIntegration
        .uninstall(&make_ctx(home.path(), true))
        .unwrap();

    assert!(foreign.is_file(), "uninstall must not delete user files");
    // Generated files are still gone.
    assert!(!dashboard_dir(home.path()).join("manifest.json").exists());
    assert!(!dashboard_dir(home.path()).join("dist").exists());
}

#[test]
fn deployed_bundles_match_embedded_standalone_assets() {
    // The wrapper must serve the exact same UI the standalone dashboard
    // embeds — byte-identical bundles, no fork.
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    let dist = dashboard_dir(home.path()).join("dist");
    let holographic = read(&dist.join("holographic.js"));
    let entry = read(&dist.join("index.js"));
    let css = read(&dist.join("style.css"));

    assert!(holographic.contains("tokensave holographic-memory dashboard plugin"));
    assert!(entry.contains("\"tokensave\""));
    // Wrapper chrome first, then the child stylesheets concatenated.
    assert!(css.starts_with("/* Wrapper chrome"));
    assert!(css.contains(".tsiw-tab"));
}

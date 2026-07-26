//! Hermes dashboard plugin-page deployment tests.
//!
//! `tracedecay install --agent hermes` deploys the dashboard host adapter
//! (manifest.json + plugin_api.py + the required mount entry) into the generated
//! plugin's `dashboard/` subdirectory, where Hermes' dashboard-plugin
//! discovery (`plugins/*/dashboard/manifest.json`) picks it up. These tests
//! cover the deploy itself, idempotent reinstall with pin preservation, the
//! `--no-dashboard` opt-out, and uninstall cleanup.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use tracedecay::agents::{AgentIntegration, HermesIntegration, InstallContext};
use tracedecay::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, approve_managed_skill,
    create_managed_skill_draft, default_managed_skill_targets,
};

fn make_ctx(home: &Path, dashboard: bool) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: Vec::new(),
        project_root: None,
        dashboard,
    }
}

fn dashboard_dir(home: &Path) -> PathBuf {
    home.join(".hermes/plugins/tracedecay/dashboard")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn is_mountable_dashboard_entry(entry: &str) -> bool {
    entry.len() > 512
        && entry.contains("__HERMES_PLUGINS__")
        && entry.contains(".register(")
        && entry.contains("iframe")
        && entry.contains("/api/plugins/tracedecay/dashboard-url")
        && !entry.contains("placeholder")
        && !entry.contains("rewrite in progress")
}

#[tokio::test]
async fn install_and_update_reconcile_active_managed_skills() {
    let (env, _) = super::common::IsolatedEnv::acquire().await;
    let home = env.home();
    let profile_root = home.join(".tracedecay");
    create_managed_skill_draft(
        &profile_root,
        ManagedSkillDraft {
            id: "repo-hygiene".to_string(),
            title: "Repository hygiene".to_string(),
            summary: "Use when checking repository hygiene.".to_string(),
            category: "workflow".to_string(),
            targets: default_managed_skill_targets(),
            body_markdown: "Check repository hygiene.".to_string(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::UserDraft,
                actor: "test".to_string(),
                run_id: None,
            },
        },
    )
    .await
    .unwrap();
    approve_managed_skill(&profile_root, "repo-hygiene")
        .await
        .unwrap();

    let ctx = make_ctx(home, false);
    HermesIntegration.install(&ctx).unwrap();
    let skill_path =
        home.join(".hermes/plugins/tracedecay/skills/agent-managed/repo-hygiene/SKILL.md");
    assert!(skill_path.is_file());

    std::fs::remove_file(&skill_path).unwrap();
    let outcome = HermesIntegration.update_plugin(&ctx).unwrap();
    assert!(matches!(
        outcome,
        tracedecay::agents::UpdatePluginOutcome::Refreshed(_)
    ));
    assert!(skill_path.is_file());
}

#[test]
fn install_deploys_dashboard_plugin_page() {
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    let dash = dashboard_dir(home.path());
    let entry = read(&dash.join("dist/index.js"));
    assert!(
        is_mountable_dashboard_entry(&entry),
        "Hermes dashboard entry is not a real mount adapter"
    );
    for retired in [
        "holographic.js",
        "lcm.js",
        "graph.js",
        "savings.js",
        "style.css",
    ] {
        assert!(
            !dash.join("dist").join(retired).exists(),
            "retired Hermes UI asset was deployed: {retired}"
        );
    }

    // Manifest is discoverable and stamped with the generating version.
    let manifest: serde_json::Value =
        serde_json::from_str(&read(&dash.join("manifest.json"))).unwrap();
    assert_eq!(manifest["name"], "tracedecay");
    assert_eq!(manifest["label"], "TraceDecay");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["entry"], "dist/index.js");
    assert!(manifest.get("css").is_none());
    // `api` must stay a relative path inside dashboard/ — Hermes rejects
    // absolute/traversal api paths (GHSA-5qr3-c538-wm9j).
    assert_eq!(manifest["api"], "plugin_api.py");
    let description = manifest["description"].as_str().unwrap();
    for workspace in [
        "Brain",
        "Explorer",
        "Loom",
        "Sessions",
        "Agents",
        "Code",
        "Knowledge",
        "Delivery",
        "Automations",
        "Observatory",
        "Costs",
        "Settings",
    ] {
        assert!(
            description.contains(workspace),
            "manifest omits the {workspace} workspace"
        );
    }
    assert!(!description.split_whitespace().any(|word| word == "Work"));

    // The proxy backend bakes in the installing binary (env still wins).
    let api = read(&dash.join("plugin_api.py"));
    assert!(api.contains(r#"DEPLOYED_TRACEDECAY_BIN = "/usr/local/bin/tracedecay""#));
    assert!(!api.contains("DEPLOYED_PROJECT_ROOT"));
    assert!(!api.contains(home.path().to_string_lossy().as_ref()));
    assert!(api.contains("router = APIRouter()"));
    assert!(api.contains(r#"@router.get("/dashboard-url")"#));
}

#[test]
fn install_context_project_root_is_not_baked_into_dashboard() {
    let home = tempfile::tempdir().unwrap();
    let mut ctx = make_ctx(home.path(), true);
    ctx.project_root = Some(PathBuf::from("/pinned/project"));
    HermesIntegration.install(&ctx).unwrap();

    let api = read(&dashboard_dir(home.path()).join("plugin_api.py"));
    assert!(!api.contains("DEPLOYED_PROJECT_ROOT"));
    assert!(!api.contains("/pinned/project"));
}

#[test]
fn reinstall_is_idempotent_and_stays_unpinned() {
    let home = tempfile::tempdir().unwrap();
    let mut pinned = make_ctx(home.path(), true);
    pinned.project_root = Some(PathBuf::from("/pinned/project"));
    HermesIntegration.install(&pinned).unwrap();

    let dash = dashboard_dir(home.path());
    let first_api = read(&dash.join("plugin_api.py"));
    let first_manifest = read(&dash.join("manifest.json"));

    // Reinstall keeps the generated wrapper stable and unpinned.
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    assert_eq!(read(&dash.join("plugin_api.py")), first_api);
    assert_eq!(read(&dash.join("manifest.json")), first_manifest);
    assert!(!read(&dash.join("plugin_api.py")).contains("DEPLOYED_PROJECT_ROOT"));
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
    assert!(
        home.path()
            .join(".hermes/plugins/tracedecay/plugin.yaml")
            .is_file()
    );
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
        !home.path().join(".hermes/plugins/tracedecay").exists(),
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
fn deployed_entry_mounts_the_daemon_dashboard_without_copying_ui() {
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    let dist = dashboard_dir(home.path()).join("dist");
    let entry = read(&dist.join("index.js"));
    assert!(is_mountable_dashboard_entry(&entry));
    assert_eq!(
        std::fs::read_dir(&dist).unwrap().count(),
        1,
        "Hermes must receive only the required host mount entry, never a copied dashboard"
    );
    assert!(!is_mountable_dashboard_entry(
        "/* tracedecay dashboard placeholder — rewrite in progress. */"
    ));
}

#[test]
fn deployed_wrapper_preserves_canonical_api_proxy_surface() {
    // The Hermes dashboard wrapper must remain a thin proxy over the standalone
    // tracedecay dashboard API. This text-level contract catches accidental
    // route rewrites even when the Python wrapper is not executed by cargo test.
    let home = tempfile::tempdir().unwrap();
    HermesIntegration
        .install(&make_ctx(home.path(), true))
        .unwrap();

    let api = read(&dashboard_dir(home.path()).join("plugin_api.py"));
    for required in [
        r#"@router.get("/dashboard-url")"#,
        r#"return JSONResponse({"url": f"{base}/"})"#,
        r#"@router.get("/capabilities")"#,
        r#"_proxy("GET", "/api/capabilities", _DummyRequest(), None)"#,
        r#"@router.get("/holographic")"#,
        r#"@router.get("/holographic/")"#,
        r#"@router.get("/holographic/{path:path}")"#,
        r#"@router.post("/holographic/{path:path}")"#,
        r#"/api/plugins/holographic/"#,
        r#"@router.get("/lcm/{path:path}")"#,
        r#"@router.post("/lcm/{path:path}")"#,
        r#"/api/plugins/hermes-lcm/{path}"#,
        r#"@router.get("/graph/{path:path}")"#,
        r#"@router.post("/graph/{path:path}")"#,
        r#"/api/plugins/graph/{path}"#,
        r#"@router.get("/savings/{path:path}")"#,
        r#"@router.post("/savings/{path:path}")"#,
        r#"/api/plugins/savings/{path}"#,
        "url = f\"{base}{upstream_path}\" + (f\"?{query}\" if query else \"\")",
    ] {
        assert!(
            api.contains(required),
            "deployed plugin_api.py lost required proxy contract: {required}"
        );
    }
}

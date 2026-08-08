use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::client_identity::DaemonClientIdentity;
use crate::daemon::{DaemonHandshake, StoreAdministration};
use crate::mcp::McpServer;
use crate::mcp::server::McpServerConstructionContext;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

static REGISTERED_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

async fn initialize_test_project(
    project_root: &Path,
    profile_root: &Path,
) -> crate::storage::StoreLayout {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(profile_root, std::fs::Permissions::from_mode(0o700))
            .expect("secure fixture profile root");
    }
    let options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.to_path_buf()),
        global_db_path: Some(profile_root.join("registry.db")),
    };
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        profile_root,
        "truthful core Doctor fixture initialization",
    )
    .expect("acquire fixture lifecycle authority");
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "truthful core Doctor fixture initialization",
    )
    .expect("enter fixture maintenance database scope");
    let initialized =
        TraceDecay::init_with_exclusive_maintenance(project_root, options, &lifecycle)
            .await
            .expect("initialize core Doctor fixture");
    let layout = initialized.store_layout().clone();
    initialized.close();
    layout
}

fn handshake(
    project_path: PathBuf,
    profile_root: PathBuf,
    global_db_path: PathBuf,
) -> DaemonHandshake {
    DaemonHandshake {
        project_path: Some(project_path),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            profile_root,
            global_db_path,
        },
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        client_instance_id: "truthful-core-doctor-test".to_string(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn live_runtime_snapshot_does_not_fabricate_store_metadata_after_observation_fails() {
    let root = tempfile::TempDir::new().unwrap();
    let project = root.path().join("project");
    let profile = root.path().join("profile");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    let layout = initialize_test_project(&project, &profile).await;
    let handshake = handshake(
        project.clone(),
        profile.clone(),
        profile.join("registry.db"),
    );
    let store_administration = StoreAdministration::default().with_profile_identity(
        crate::daemon::profile_identity::load_or_create(&profile)
            .expect("load fixture profile identity"),
    );
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile,
        REGISTERED_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed),
        "core-doctor-missing-live-store-metadata",
    )
    .expect("enter daemon database scope");
    let graph =
        super::super::open_project_for_handshake(&project, &handshake, &store_administration)
            .await
            .expect("open retained project graph");
    let key = crate::daemon::ProjectServerKey::from_open_project(&graph, &handshake)
        .expect("project server key");
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(graph, None)
            .with_project_server_live(Arc::new(AtomicBool::new(true))),
    )
    .await;
    store_administration
        .project_servers()
        .lock()
        .await
        .insert(key, server);
    std::fs::remove_file(&layout.graph_db_path)
        .expect("remove graph file after its retained route has gone live");

    let value = super::doctor_runtime_value(&handshake, &store_administration, false, None).await;

    assert_eq!(
        value.pointer("/database/db_size_bytes"),
        Some(&serde_json::Value::Null),
        "unreadable store metadata must remain unavailable rather than become zero bytes"
    );
    assert_eq!(
        value.pointer("/database/schema_version"),
        Some(&serde_json::Value::Null),
        "a live route without a schema observation must not claim the compiled schema"
    );
    assert_eq!(
        value.pointer("/database/schema_state"),
        Some(&serde_json::Value::Null),
        "schema state requires an observed schema version"
    );
    assert_eq!(
        value.pointer("/database/schema_drift"),
        Some(&serde_json::Value::Null),
        "schema drift requires an observed schema version"
    );
}

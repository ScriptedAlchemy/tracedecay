#![cfg(unix)]

use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::ProjectId;

use super::bootstrap::run_git;
use super::{
    enter_test_daemon_database_scope, initialize_test_project, test_client_identity_for,
    test_daemon_engine_for_profile, test_handshake_defaults,
};
use crate::daemon::DaemonHandshake;

fn initialized_project_id(layout: &crate::storage::StoreLayout) -> ProjectId {
    ProjectId::new(
        layout
            .identity
            .project_id
            .clone()
            .expect("initialized project identity"),
    )
    .expect("typed project identity")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_quiesces_only_a_and_remounts_its_retry_route() {
    let home = TempDir::new().expect("isolated home");
    let home = home.path().canonicalize().expect("canonical home");
    let profile_root = home.join("profile");
    let project_a_root = home.join("project-a");
    let project_b_root = home.join("project-b");
    for project_root in [&project_a_root, &project_b_root] {
        std::fs::create_dir_all(project_root).expect("create project root");
        run_git(project_root, &["init", "-b", "main", "--quiet"]);
        std::fs::write(project_root.join("README.md"), "recovery lifecycle\n")
            .expect("write project fixture");
        run_git(project_root, &["add", "."]);
        run_git(project_root, &["commit", "-m", "fixture", "--quiet"]);
    }

    let client_identity = test_client_identity_for(profile_root.clone());
    let layout_a = initialize_test_project(&project_a_root, &client_identity).await;
    let layout_b = initialize_test_project(&project_b_root, &client_identity).await;
    let project_a = initialized_project_id(&layout_a);
    let project_b = initialized_project_id(&layout_b);
    assert_ne!(project_a, project_b);

    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "remote recovery lifecycle");
    let engine = test_daemon_engine_for_profile(&profile_root);
    let handshake_a = DaemonHandshake {
        project_path: Some(project_a_root.clone()),
        client_identity: client_identity.clone(),
        ..test_handshake_defaults()
    };
    let handshake_b = DaemonHandshake {
        project_path: Some(project_b_root.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let server_a = engine
        .project_server(&handshake_a)
        .await
        .expect("mount active project A");
    let server_b = engine
        .project_server(&handshake_b)
        .await
        .expect("mount active project B");

    let runtime_registry = engine
        .store_administration
        .session_runtime_registry()
        .await
        .expect("session runtime registry");
    let old_a = runtime_registry
        .mounted_project_sessions(&project_a)
        .await
        .expect("mounted ProjectSessions A");
    let old_a_binding = old_a.binding().clone();
    let old_a_locator = old_a.verified_locator().clone();
    let database_b = runtime_registry
        .mounted_project_sessions(&project_b)
        .await
        .expect("mounted ProjectSessions B");
    let database_b_binding = database_b.binding().clone();
    let database_b_locator = database_b.verified_locator().clone();
    let server_a_database = server_a.project_session_db().expect("server A database");
    assert_eq!(server_a_database.binding(), &old_a_binding);
    assert_eq!(server_a_database.verified_locator(), &old_a_locator);
    assert!(
        !server_a_database.shares_client_with(&old_a),
        "server and map reads issue independently counted leases"
    );
    let server_b_database = server_b.project_session_db().expect("server B database");
    assert_eq!(server_b_database.binding(), &database_b_binding);
    assert_eq!(server_b_database.verified_locator(), &database_b_locator);
    assert!(
        !server_b_database.shares_client_with(&database_b),
        "server and map reads issue independently counted leases"
    );
    drop(server_a_database);
    drop(server_b_database);

    let profile_id = old_a.binding().shard_id.profile_id.clone();
    let session_sync = engine.store_administration.session_sync_service();
    let lifecycle = engine
        .store_administration
        .remote_recovery_project_lifecycle()
        .expect("remote recovery lifecycle lookup")
        .expect("installed remote recovery lifecycle");
    drop(server_a);
    let quiescence = lifecycle
        .quiesce(&project_a, &old_a)
        .await
        .expect("quiesce exact project A");

    {
        let servers = engine.store_administration.project_servers().lock().await;
        assert!(
            servers
                .servers
                .keys()
                .all(|key| { key.owner.project_id.as_deref() != Some(project_a.as_str()) })
        );
        assert!(
            servers
                .servers
                .keys()
                .any(|key| { key.owner.project_id.as_deref() == Some(project_b.as_str()) })
        );
    }
    drop(old_a);
    runtime_registry
        .retire_project_session_relation_graph(&project_a)
        .await
        .expect("retire exact A replay and relation graph");
    assert!(
        runtime_registry
            .remote_replay_transaction()
            .target_descriptor(&project_a)
            .is_err(),
        "the interrupted route is absent until the exact remount"
    );
    assert!(
        runtime_registry
            .mounted_project_sessions(&project_a)
            .await
            .is_none()
    );
    let still_mounted_b = runtime_registry
        .mounted_project_sessions(&project_b)
        .await
        .expect("B remains mounted");
    assert_eq!(still_mounted_b.binding(), &database_b_binding);
    assert_eq!(still_mounted_b.verified_locator(), &database_b_locator);
    assert!(
        !still_mounted_b.shares_client_with(&database_b),
        "a fresh B map read must not reuse a client from before A retirement"
    );

    let replacement_a = runtime_registry
        .project_sessions(project_a.clone(), [project_a_root.clone()])
        .await
        .expect("remount exact ProjectSessions A");
    assert!(
        session_sync
            .rebind_project(&profile_id, &project_a, &replacement_a)
            .await
            .expect("rebind session-sync A")
    );
    assert!(
        runtime_registry
            .remote_replay_transaction()
            .target_descriptor(&project_a)
            .is_ok(),
        "the remount restores the retry route before admission reopens"
    );
    drop(quiescence);

    let reopened_a = engine
        .project_server(&handshake_a)
        .await
        .expect("reopen project A after recovery");
    let reopened_a_database = reopened_a
        .project_session_db()
        .expect("reopened server A database");
    assert_eq!(reopened_a_database.binding(), replacement_a.binding());
    assert_eq!(
        reopened_a_database.verified_locator(),
        replacement_a.verified_locator()
    );
    assert!(
        !reopened_a_database.shares_client_with(&replacement_a),
        "reopening A issues a new counted client from its replacement owner"
    );
    let still_live_b = engine
        .project_server(&handshake_b)
        .await
        .expect("project B stays live");
    assert!(Arc::ptr_eq(&still_live_b, &server_b));
    let still_live_b_database = still_live_b
        .project_session_db()
        .expect("server B database after A recovery");
    assert_eq!(still_live_b_database.binding(), &database_b_binding);
    assert_eq!(
        still_live_b_database.verified_locator(),
        &database_b_locator
    );
    assert!(
        !still_live_b_database.shares_client_with(&database_b),
        "B's server lease remains a separate counted client through A recovery"
    );
    drop(still_live_b_database);
    drop(reopened_a_database);
    drop(still_mounted_b);
    drop(replacement_a);

    engine.shutdown_all().await;
}

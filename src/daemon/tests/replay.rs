use super::*;

#[tokio::test]
async fn projectless_user_session_setup_failure_returns_json_rpc_error() {
    let temp = TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    let identity = test_client_identity_for(profile_root);
    let params = serde_json::json!({
        "name": "tracedecay_lcm_status",
        "arguments": {
            "storage_scope": "user",
            "provider": "cursor",
            "format": "json"
        }
    });

    let response = super::super::projectless_tools_call_response(
        serde_json::json!(1),
        Some(&params),
        &identity,
        &StoreAdministration::default(),
    )
    .await;

    let error = response
        .error
        .expect("profile setup failure must be returned as JSON-RPC");
    assert_eq!(error.code, -32603);
    assert!(
        error
            .message
            .contains("project route error (registered_authority_unavailable)"),
        "unexpected setup error: {}",
        error.message
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_identity_startup_replays_retained_profile_receipts() {
    let temp = TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    let first_admin = test_store_administration_for_profile(&profile_root);
    let _database_scope =
        enter_test_daemon_database_scope(&profile_root, "profile-host-replay-test");
    let profile_identity = first_admin
        .profile_identity()
        .expect("test profile identity")
        .clone();
    let identity = DaemonClientIdentity {
        profile_root: profile_root.clone(),
        global_db_path: profile_root.join("global.db"),
    };

    let user_db = first_admin
        .registered_profile_session_database()
        .await
        .unwrap();
    let broker = first_admin
        .host_admission_broker(&user_db)
        .await
        .unwrap()
        .broker()
        .cloned()
        .expect("fresh host admission spool");
    let automation_root = crate::automation::runner::user_automation_root(&profile_root);
    std::fs::write(&automation_root, "block canonical receipt apply").unwrap();
    let params = serde_json::json!({
        "name": "tracedecay_hook_runtime",
        "arguments": {
            "action": "hermes_receipt",
            "event": {
                "agent": "hermes",
                "event": "turnCompleted",
                "route": { "session_id": "startup-session" },
                "receipt": {
                    "tool_call_id": "startup-call",
                    "turn_id": "startup-turn",
                    "status": "success",
                    "duration_ms": 1,
                    "transcript_watermark": "startup-watermark"
                }
            }
        }
    });
    let response = super::super::projectless_tools_call_response(
        serde_json::json!(1),
        Some(&params),
        &identity,
        &first_admin,
    )
    .await;
    assert!(
        response.error.is_some(),
        "blocked canonical apply must retain the daemon-admitted receipt"
    );
    assert_eq!(broker.pending_count().await, 1);
    assert!(!profile_root.join("host_receipts.json").exists());
    first_admin.shutdown_host_admission_replay().await;
    // Retain the pending record after the first daemon's replay authority has
    // stopped, so restart replay remains the acceptance path under test.
    drop(broker);
    drop(user_db);
    drop(first_admin);
    std::fs::remove_file(&automation_root).unwrap();

    let restarted = StoreAdministration::default().with_profile_identity(profile_identity);
    super::super::replay_user_profile_host_admission_for_identity(&restarted, &identity)
        .await
        .unwrap();
    let recovered_db = restarted
        .registered_profile_session_database()
        .await
        .unwrap();
    let recovered = restarted
        .host_admission_broker(&recovered_db)
        .await
        .unwrap()
        .broker()
        .cloned()
        .expect("reopened host admission spool");
    let broker_path = super::super::authority::canonical_identity_path(
        &crate::sessions::user_sessions_db_path(&profile_root),
    )
    .unwrap();
    assert!(
        restarted
            .wait_user_profile_host_admission_replay_idle(
                &broker_path,
                std::time::Duration::from_secs(5),
            )
            .await,
        "restart replay worker must become idle"
    );
    assert_eq!(recovered.pending_count().await, 0);
    assert!(
        crate::automation::runner::user_automation_root(&profile_root)
            .join("host_receipts.json")
            .is_file()
    );
}

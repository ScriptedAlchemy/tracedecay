use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_identity_startup_replays_retained_profile_receipts() {
    let temp = TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let identity = DaemonClientIdentity {
        profile_root: profile_root.clone(),
        global_db_path: profile_root.join("global.db"),
    };

    let first_admin = StoreAdministration::default();
    let user_db = first_admin
        .user_session_database(&identity.global_db_path)
        .await
        .unwrap();
    let broker = first_admin
        .host_admission_broker(&user_db)
        .await
        .unwrap()
        .broker()
        .cloned()
        .expect("fresh host admission spool");
    let plan = crate::mcp::hook_events::HookEventPlan::RecordTerminalReceipt {
        route: Some(crate::daemon::HookRouteMetadata {
            session_id: Some("startup-session".to_string()),
            thread_id: None,
            cwd: None,
            worktree: None,
            branch: None,
        }),
        receipt: crate::daemon::HookTerminalReceipt {
            tool_call_id: Some("startup-call".to_string()),
            turn_id: Some("startup-turn".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(1),
            transcript_watermark: Some("startup-watermark".to_string()),
        },
    };
    let payload = crate::mcp::hook_events::encode_durable_hook_event_plan(&plan).unwrap();
    first_admin.shutdown_host_admission_replay().await;
    broker.admit("hermes:startup-test", &payload).await.unwrap();
    // Retain the pending record after the first daemon's replay authority has
    // drained, so restart replay remains the acceptance path under test.
    drop(broker);
    drop(user_db);
    drop(first_admin);

    let restarted = StoreAdministration::default();
    super::super::replay_user_profile_host_admission_for_identity(&restarted, &identity)
        .await
        .unwrap();
    let recovered_db = restarted
        .user_session_database(&identity.global_db_path)
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

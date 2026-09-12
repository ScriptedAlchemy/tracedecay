//! Verifies measured workloads remain functional when Hotpath compiles away.

#[cfg(not(feature = "hotpath"))]
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
#[cfg(not(feature = "hotpath"))]
use tracedecay_store::SessionRecord;

#[cfg(not(feature = "hotpath"))]
const PROVIDER: &str = "claude";
#[cfg(not(feature = "hotpath"))]
const SESSION: &str = "hotpath-coverage";

#[cfg(not(feature = "hotpath"))]
fn session_record() -> SessionRecord {
    SessionRecord {
        provider: PROVIDER.to_owned(),
        session_id: SESSION.to_owned(),
        project_key: "/project".to_owned(),
        project_path: "/project".to_owned(),
        title: None,
        started_at: None,
        ended_at: None,
        transcript_path: Some(format!("/tmp/{SESSION}.jsonl")),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

/// Drives the measured registered-store hot paths once: profile open
/// (admission and schema convergence), a committed write transaction, and a
/// session-activity read, mirroring the `session_activity_reads` bench.
#[cfg(not(feature = "hotpath"))]
fn exercise_measured_hot_paths() {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build coverage runtime");
    let profile = tempfile::tempdir().expect("temporary coverage profile");
    tokio.block_on(async {
        let runtime = RegisteredGlobalDbTestRuntime::profile(profile.path())
            .await
            .expect("open registered-store coverage fixture");
        let database = runtime.profile_database();
        assert!(database.upsert_session(&session_record()).await);
        let transaction = database
            .begin_write_transaction()
            .await
            .expect("begin coverage transaction");
        transaction
            .execute_batch(&format!(
                "INSERT INTO session_messages(
                     provider, message_id, session_id, role, timestamp, ordinal, text,
                     kind, model, tool_names, source_path, source_offset, metadata_json
                 )
                 VALUES
                     ('{PROVIDER}', 'message-000000', '{SESSION}', 'assistant', 1, 1,
                      'payload', 'activity', NULL, 'tool', NULL, NULL, NULL),
                     ('{PROVIDER}', 'message-000001', '{SESSION}', 'assistant', 2, 2,
                      'payload', 'activity', NULL, 'tool', NULL, NULL, NULL);"
            ))
            .await
            .expect("seed coverage session activity");
        transaction.commit().await.expect("commit coverage rows");
        let rows = database
            .session_messages_after(PROVIDER, SESSION, 0, 16)
            .await
            .expect("read coverage session activity");
        assert_eq!(rows.len(), 2);
    });
}

#[cfg(not(feature = "hotpath"))]
#[test]
fn measured_hot_paths_run_with_the_feature_off() {
    // Every `#[hotpath::measure]` in the crate expands to a no-op here; the
    // workload succeeding is the proof the instrumentation stays inert.
    exercise_measured_hot_paths();
}

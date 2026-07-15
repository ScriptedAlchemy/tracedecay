mod common;

use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(all(unix, feature = "test-transport"))]
use std::{path::Path, time::Duration};

#[cfg(all(unix, feature = "test-transport"))]
use serde_json::Value;
use serde_json::json;
#[cfg(all(unix, feature = "test-transport"))]
use tracedecay::client_identity::DaemonClientIdentity;
#[cfg(all(unix, feature = "test-transport"))]
use tracedecay::daemon::{DaemonHandshake, call_tool};
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1,
    ObservationScopeV1, PayloadReferenceV1, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId,
};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationReplayRequest, ObservationStore, ObservationStoreError,
    ObservationWrite,
};

#[cfg(all(unix, feature = "test-transport"))]
use common::{daemon_socket_path, spawn_tracedecay_daemon, tracedecay_command_with_home};
use common::{isolated_lcm_db_path, open_lcm_db, spawn_tracedecay_daemon_with, tempdir_or_panic};

const GENERATION: u64 = 23;

#[cfg(all(unix, feature = "test-transport"))]
const OBSERVATION_PERSIST_BARRIER_DIR_ENV: &str = "TRACEDECAY_TEST_OBSERVATION_PERSIST_BARRIER_DIR";
#[cfg(all(unix, feature = "test-transport"))]
const DAEMON_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(10);

fn source(stage: &str) -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new(format!("session.daemon-fault.{stage}")).unwrap())
        .unwrap()
}

fn cursor(stage: &str, byte_offset: u64) -> ClaudeSourceCursorV1 {
    ClaudeSourceCursorV1::new(
        source(stage),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(GENERATION).unwrap(),
        byte_offset,
    )
    .unwrap()
}

fn observation(stage: &str) -> DurableClaudeObservationV1 {
    let payload = json!({
        "kind": "assistant_message",
        "body": format!("sanitized daemon fault payload {stage}"),
    });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.daemon-fault.{stage}")).unwrap(),
            ComponentVersion::new("sanitizer.daemon-fault.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::new(
        source(stage),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(GENERATION).unwrap(),
        ClaudeByteRangeV1::new(0, 100).unwrap(),
    )
    .unwrap();

    DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.daemon-fault").unwrap(),
        payload,
    )
    .unwrap()
}

fn write(stage: &str, observation: DurableClaudeObservationV1) -> ObservationWrite {
    ObservationWrite::new(observation, None, cursor(stage, 100)).unwrap()
}

#[cfg(all(unix, feature = "test-transport"))]
fn json_tool_payload(response: &Value, operation: &str) -> Value {
    response["content"]
        .as_array()
        .and_then(|content| {
            content.iter().find_map(|item| {
                item["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
            })
        })
        .unwrap_or_else(|| panic!("{operation} should return JSON content"))
}

#[cfg(all(unix, feature = "test-transport"))]
async fn expect_bounded_tool_response(
    socket_path: &Path,
    handshake: &DaemonHandshake,
    tool_name: &str,
    arguments: Value,
    operation: &str,
) -> Value {
    tokio::time::timeout(
        DAEMON_TOOL_CALL_TIMEOUT,
        call_tool(socket_path, handshake, tool_name, arguments),
    )
    .await
    .unwrap_or_else(|_| panic!("{operation} timed out"))
    .unwrap_or_else(|error| panic!("{operation} failed: {error}"))
}

#[cfg(all(unix, feature = "test-transport"))]
async fn wait_for_observation_barrier_arrival(barrier_dir: &Path) {
    tokio::time::timeout(DAEMON_TOOL_CALL_TIMEOUT, async {
        loop {
            if barrier_dir
                .join("arrived")
                .try_exists()
                .expect("read observation barrier arrival")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("daemon should reach the observation persistence barrier");
}

async fn set_statement_fault(tmp: &tempfile::TempDir, stage: &str, table: &str, enabled: bool) {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let trigger = format!("fail_observation_{stage}");
    let sql = if enabled {
        format!(
            "CREATE TRIGGER {trigger}
             BEFORE INSERT ON {table} BEGIN
                SELECT RAISE(ABORT, 'injected {stage} statement failure');
             END;"
        )
    } else {
        format!("DROP TRIGGER {trigger};")
    };
    conn.execute_batch(&sql).await.unwrap();
}

async fn observation_state_counts(tmp: &tempfile::TempDir) -> [i64; 4] {
    let db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT
                (SELECT COUNT(*) FROM sanitization_receipts),
                (SELECT COUNT(*) FROM observations),
                (SELECT COUNT(*) FROM source_cursors),
                (SELECT COUNT(*) FROM projection_queue)",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    [
        row.get(0).unwrap(),
        row.get(1).unwrap(),
        row.get(2).unwrap(),
        row.get(3).unwrap(),
    ]
}

#[tokio::test]
async fn observation_store_statement_faults_roll_back_and_retry_exactly_once() {
    for (stage, table) in [
        ("receipt", "sanitization_receipts"),
        ("observation", "observations"),
        ("cursor", "source_cursors"),
        ("enqueue", "projection_queue"),
    ] {
        let tmp = tempdir_or_panic();
        let candidate = observation(stage);

        let db = open_lcm_db(&tmp).await;
        set_statement_fault(&tmp, stage, table, true).await;
        let store = GlobalDbObservationStore::new(&db);
        let error = store
            .persist_observation(write(stage, candidate.clone()))
            .await
            .expect_err("the injected statement fault must abort persistence");
        assert!(
            matches!(error, ObservationStoreError::Storage { .. }),
            "{stage} fault returned the wrong error: {error:?}"
        );
        db.close();

        let restarted = open_lcm_db(&tmp).await;
        let restarted_store = GlobalDbObservationStore::new(&restarted);
        assert_eq!(observation_state_counts(&tmp).await, [0, 0, 0, 0]);
        assert!(
            restarted_store
                .get_observation(candidate.observation_id())
                .await
                .unwrap()
                .is_none(),
            "{stage} fault leaked the observation after restart"
        );
        assert_eq!(
            restarted_store
                .get_source_cursor(candidate.source(), candidate.scope())
                .await
                .unwrap(),
            None,
            "{stage} fault advanced the cursor after restart"
        );
        assert!(
            restarted_store
                .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
                .await
                .unwrap()
                .is_empty(),
            "{stage} fault leaked replay state after restart"
        );

        set_statement_fault(&tmp, stage, table, false).await;
        let committed = match restarted_store
            .persist_observation(write(stage, candidate.clone()))
            .await
            .unwrap()
        {
            ObservationPersistOutcome::Committed(receipt) => receipt,
            other => panic!("{stage} retry must commit once, got {other:?}"),
        };
        assert_eq!(committed.sequence(), 1);
        restarted.close();

        let replayed = open_lcm_db(&tmp).await;
        let replayed_store = GlobalDbObservationStore::new(&replayed);
        let duplicate = replayed_store
            .persist_observation(write(stage, candidate.clone()))
            .await
            .unwrap();
        assert!(matches!(
            duplicate,
            ObservationPersistOutcome::ExactDuplicate(_)
        ));
        assert_eq!(duplicate.receipt(), &committed);

        let replay = replayed_store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(replay.len(), 1, "{stage} retry duplicated replay state");
        assert_eq!(replay[0].sequence(), 1);
        assert_eq!(replay[0].observation(), &candidate);
        assert_eq!(replay[0].commit_receipt(), &committed);
        assert_eq!(
            replayed_store
                .get_source_cursor(candidate.source(), candidate.scope())
                .await
                .unwrap(),
            Some(cursor(stage, 100))
        );
        assert_eq!(
            observation_state_counts(&tmp).await,
            [1, 1, 1, 1],
            "{stage} retry must leave one receipt, observation, cursor, and queue row"
        );
    }
}

#[test]
fn configured_daemon_can_be_killed_and_reaped() {
    let home = tempdir_or_panic();
    let configured = AtomicBool::new(false);
    let mut daemon = spawn_tracedecay_daemon_with(home.path(), |command| {
        configured.store(true, Ordering::Relaxed);
        command.env("TRACEDECAY_FAULT_HARNESS_TEST", "1");
    });

    assert!(configured.load(Ordering::Relaxed));
    let status = daemon
        .kill_and_wait()
        .expect("configured daemon should be killed and reaped");
    assert!(!status.success());

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        assert_eq!(status.signal(), Some(9));
    }
}

#[cfg(all(unix, feature = "test-transport"))]
#[tokio::test]
async fn killed_daemon_retries_in_flight_claude_observation_once_via_public_apis() {
    let home = tempdir_or_panic();
    let project = tempdir_or_panic();
    let general_chat = home.path().join("general-chat");
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(
        project.path().join("src/lib.rs"),
        "pub fn daemon_fault_fixture() {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(&general_chat).unwrap();

    let init = tracedecay_command_with_home(home.path())
        .arg("init")
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "fixture init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let barrier_dir = home.path().join("observation-persist-barrier");
    std::fs::create_dir_all(&barrier_dir).unwrap();
    let mut daemon = spawn_tracedecay_daemon_with(home.path(), |command| {
        command.env(OBSERVATION_PERSIST_BARRIER_DIR_ENV, &barrier_dir);
    });
    let profile_root = home.path().join(".tracedecay");
    let socket_path = daemon_socket_path(home.path());
    let handshake = DaemonHandshake {
        project_path: None,
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root: profile_root.clone(),
        },
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        client_instance_id: "daemon-fault-harness".to_string(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
    };
    let ingest_args = |session_id: &str| {
        json!({
            "action": "ingest_transcript",
            "provider": "claude",
            "user_scope": true,
            "session_id": session_id,
            "format": "json",
        })
    };
    expect_bounded_tool_response(
        &socket_path,
        &handshake,
        "tracedecay_hook_runtime",
        ingest_args("bootstrap-user-store"),
        "bootstrap user session store",
    )
    .await;

    let session_id = "claude-daemon-kill";
    let marker = "deterministic daemon observation boundary";
    let transcript_dir = home.path().join(".claude/projects/daemon-fault");
    std::fs::create_dir_all(&transcript_dir).unwrap();
    std::fs::write(
        transcript_dir.join(format!("{session_id}.jsonl")),
        format!(
            "{}\n",
            json!({
                "type": "user",
                "sessionId": session_id,
                "uuid": "message-daemon-kill",
                "timestamp": "2026-07-15T00:00:00Z",
                "cwd": general_chat,
                "message": { "role": "user", "content": marker },
            })
        ),
    )
    .unwrap();

    // Arm the feature-gated one-shot barrier. Its arrival receipt proves that the
    // public daemon request reached the production observation persistence boundary.
    std::fs::write(barrier_dir.join("armed"), format!("{session_id}\n")).unwrap();

    let interrupted_socket = socket_path.clone();
    let interrupted_handshake = handshake.clone();
    let interrupted_args = ingest_args(session_id);
    let interrupted = tokio::spawn(async move {
        call_tool(
            &interrupted_socket,
            &interrupted_handshake,
            "tracedecay_hook_runtime",
            interrupted_args,
        )
        .await
    });
    wait_for_observation_barrier_arrival(&barrier_dir).await;
    assert!(!interrupted.is_finished());

    let killed = daemon
        .kill_and_wait()
        .expect("in-flight daemon should be killed and reaped");
    assert!(!killed.success());
    std::fs::write(barrier_dir.join("release"), b"release\n").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), interrupted)
            .await
            .expect("interrupted daemon request should finish")
            .expect("interrupted daemon request task should join")
            .is_err(),
        "killed daemon must not acknowledge the interrupted request"
    );

    let _restarted = spawn_tracedecay_daemon(home.path());
    let committed = expect_bounded_tool_response(
        &socket_path,
        &handshake,
        "tracedecay_hook_runtime",
        ingest_args(session_id),
        "retry interrupted Claude observation",
    )
    .await;
    let committed_payload = json_tool_payload(&committed, "observation retry");
    assert_eq!(committed_payload["observations_committed"], 1);
    assert_eq!(committed_payload["messages_upserted"], 1);

    let replayed = expect_bounded_tool_response(
        &socket_path,
        &handshake,
        "tracedecay_hook_runtime",
        ingest_args(session_id),
        "replay committed Claude observation",
    )
    .await;
    let replayed_payload = json_tool_payload(&replayed, "observation replay");
    assert_eq!(replayed_payload["observations_committed"], 0);
    assert_eq!(replayed_payload["messages_upserted"], 0);

    let mut project_handshake = handshake.clone();
    project_handshake.project_path = Some(project.path().to_path_buf());
    let search = expect_bounded_tool_response(
        &socket_path,
        &project_handshake,
        "tracedecay_message_search",
        json!({
            "storage_scope": "user",
            "provider": "claude",
            "session_id": session_id,
            "query": marker,
            "catch_up": false,
            "format": "json",
        }),
        "user message search",
    )
    .await;
    let payload = json_tool_payload(&search, "message search");
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["count"], 1, "retry must remain exactly once");
    assert_eq!(payload["results"][0]["session"]["session_id"], session_id);
}

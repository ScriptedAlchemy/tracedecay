mod common;

use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
use std::{path::Path, time::Duration};

use rusqlite::Connection;
#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
use serde_json::Value;
use serde_json::json;
#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
use tracedecay::client_identity::DaemonClientIdentity;
#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
use tracedecay::daemon::{DaemonHandshake, call_tool};
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1,
    ObservationScopeV1, PayloadReferenceV1, ProjectionGenerationId, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationReplayRequest,
    ObservationStore, ObservationStoreError, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
use common::{daemon_socket_path, spawn_tracedecay_daemon, tracedecay_command_with_home};
use common::{isolated_lcm_db_path, open_lcm_db, spawn_tracedecay_daemon_with, tempdir_or_panic};

const GENERATION: u64 = 23;

fn observation_store(db: &common::LcmTestRuntime) -> GlobalDbObservationStore<'_> {
    db.observation_store()
        .expect("registered profile observation store")
}

#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
const OBSERVATION_PERSIST_BARRIER_DIR_ENV: &str = "TRACEDECAY_TEST_OBSERVATION_PERSIST_BARRIER_DIR";
#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
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

fn write(stage: &str, observation: DurableClaudeObservationV1) -> AnchoredObservationWrite {
    let write = ObservationWrite::new(observation, None, cursor(stage, 100)).unwrap();
    let generation = ProjectionGenerationId::new("projection.daemon-fault.v4").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "daemon-fault").unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, generation).unwrap()
}

#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
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

#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
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

#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
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
    let conn = Connection::open(isolated_lcm_db_path(tmp)).unwrap();
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
    conn.execute_batch(&sql).unwrap();
}

async fn observation_state_counts(tmp: &tempfile::TempDir) -> [i64; 4] {
    observation_state_counts_at(isolated_lcm_db_path(tmp)).await
}

async fn observation_state_counts_at(db_path: impl AsRef<std::path::Path>) -> [i64; 4] {
    let conn = Connection::open(db_path.as_ref()).unwrap();
    conn.query_row(
        "SELECT
                (SELECT COUNT(*) FROM sanitization_receipts),
                (SELECT COUNT(*) FROM observations),
                (SELECT COUNT(*) FROM source_cursors),
                (SELECT COUNT(*) FROM projection_queue)",
        [],
        |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?]),
    )
    .unwrap()
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

        let initialized = open_lcm_db(&tmp).await;
        initialized.close();
        set_statement_fault(&tmp, stage, table, true).await;
        let db = open_lcm_db(&tmp).await;
        let store = observation_store(&db);
        let error = store
            .persist_observation(write(stage, candidate.clone()))
            .await
            .expect_err("the injected statement fault must abort persistence");
        assert!(
            matches!(error, ObservationStoreError::Storage { .. }),
            "{stage} fault returned the wrong error: {error:?}"
        );
        db.close();

        assert_eq!(observation_state_counts(&tmp).await, [0, 0, 0, 0]);
        let restarted = open_lcm_db(&tmp).await;
        let restarted_store = observation_store(&restarted);
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

        restarted.close();
        set_statement_fault(&tmp, stage, table, false).await;
        let retry = open_lcm_db(&tmp).await;
        let retry_store = observation_store(&retry);
        let committed = match retry_store
            .persist_observation(write(stage, candidate.clone()))
            .await
            .unwrap()
        {
            ObservationPersistOutcome::Committed(receipt) => receipt,
            other => panic!("{stage} retry must commit once, got {other:?}"),
        };
        assert_eq!(committed.sequence(), 1);
        retry.close();

        let replayed = open_lcm_db(&tmp).await;
        let replayed_store = observation_store(&replayed);
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
        replayed.close();
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

#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
async fn assert_daemon_crash_stage(
    barrier_stage: &str,
    session_id: &str,
    marker: &str,
    committed_before_kill: bool,
) {
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

    crate::common::initialize_tracedecay_cli_project(home.path(), project.path());

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

    let transcript_dir = home.path().join(".claude/projects/daemon-fault");
    std::fs::create_dir_all(&transcript_dir).unwrap();
    // The private checked-cfg barrier is one-shot. Its arrival proves the public
    // daemon request reached the selected production transaction boundary.
    std::fs::write(
        barrier_dir.join("armed"),
        format!("{barrier_stage}\n{session_id}\n"),
    )
    .unwrap();
    std::fs::write(
        transcript_dir.join(format!("{session_id}.jsonl")),
        format!(
            "{}\n",
            json!({
                "type": "user",
                "sessionId": session_id,
                "uuid": format!("message-{barrier_stage}"),
                "timestamp": "2026-07-15T00:00:00Z",
                "cwd": general_chat,
                "message": { "role": "user", "content": marker },
            })
        ),
    )
    .unwrap();

    let interrupted_socket = socket_path.clone();
    let interrupted_handshake = handshake.clone();
    let interrupted_args = ingest_args(session_id);
    let mut interrupted = tokio::spawn(async move {
        call_tool(
            &interrupted_socket,
            &interrupted_handshake,
            "tracedecay_hook_runtime",
            interrupted_args,
        )
        .await
    });
    tokio::select! {
        () = wait_for_observation_barrier_arrival(&barrier_dir) => {}
        response = &mut interrupted => {
            panic!("daemon request completed before the observation persistence barrier: {response:?}");
        }
    }
    assert!(!interrupted.is_finished());

    let killed = daemon
        .kill_and_wait()
        .expect("in-flight daemon should be killed and reaped");
    assert!(!killed.success());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), interrupted)
            .await
            .expect("interrupted daemon request should finish")
            .expect("interrupted daemon request task should join")
            .is_err(),
        "killed daemon must not acknowledge the interrupted request"
    );

    let observation_db_path = tracedecay_sessions::runtime::user_sessions_db_path(&profile_root);
    assert_eq!(
        observation_state_counts_at(&observation_db_path).await,
        if committed_before_kill {
            [1, 1, 1, 1]
        } else {
            [0, 0, 0, 0]
        },
        "{barrier_stage} persisted an invalid authority boundary"
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
    assert_eq!(
        committed_payload["observations_committed"],
        if committed_before_kill { 0 } else { 1 }
    );
    assert_eq!(committed_payload["observation_duplicates"], 0);
    assert_eq!(committed_payload["messages_upserted"], 1);
    assert_eq!(
        observation_state_counts_at(&observation_db_path).await,
        [1, 1, 1, 0],
        "{barrier_stage} retry must leave one projected authority set"
    );

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
    assert_eq!(replayed_payload["observation_duplicates"], 0);
    assert_eq!(replayed_payload["messages_upserted"], 0);
    assert_eq!(
        observation_state_counts_at(&observation_db_path).await,
        [1, 1, 1, 0],
        "{barrier_stage} replay must not mutate authority state"
    );

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
    assert_eq!(
        payload["status"], "ok",
        "{barrier_stage} retry must leave the user session readable: {payload}"
    );
    assert_eq!(payload["count"], 1, "retry must remain exactly once");
    assert_eq!(payload["results"][0]["session"]["session_id"], session_id);
}

#[cfg(all(unix, tracedecay_observation_fault_harness, feature = "test-transport"))]
#[tokio::test]
async fn killed_daemon_retries_in_flight_claude_observation_once_via_public_apis() {
    assert_daemon_crash_stage(
        "post-write-pre-commit",
        "claude-daemon-kill-pre-commit",
        "deterministic pre-commit daemon observation boundary",
        false,
    )
    .await;
    assert_daemon_crash_stage(
        "post-commit-pre-ack",
        "claude-daemon-kill-post-commit",
        "deterministic post-commit daemon observation boundary",
        true,
    )
    .await;
}

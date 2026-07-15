mod common;

use serde_json::json;
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

use common::{isolated_lcm_db_path, open_lcm_db, tempdir_or_panic};

const GENERATION: u64 = 23;

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
async fn observation_transaction_faults_restart_and_replay_exactly_once() {
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
        drop(store);
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
        drop(restarted_store);
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

use libsql::params;
use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, CanonicalWorkflowEvidenceKindV1,
    CanonicalWorkflowSemanticKindV1, ComponentVersion, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProviderId, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId,
};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStore, ObservationStore, ObservationWrite,
    ProjectionPersistOutcome, ProjectionStoreError,
};

use crate::common::{global_message, isolated_lcm_db_path, open_lcm_db};

const FIXTURE_PROVIDER: &str = "provider-neutral-fixture";
const FIXTURE_SESSION: &str = "session.workflow-lifecycle";

#[derive(Debug, PartialEq, Eq)]
struct WorkflowRow {
    semantic_kind: String,
    provider_reference: Option<String>,
    item_id: Option<String>,
    parent_reference: Option<String>,
    list_reference: Option<String>,
    state: Option<String>,
    status: Option<String>,
    item_order: Option<i64>,
    revision: Option<String>,
    event_sequence: Option<i64>,
    content_text: String,
}

fn receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.workflow-fixture.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

struct WorkflowLifecycleFixture<'a> {
    semantic_kind: CanonicalWorkflowSemanticKindV1,
    reference: &'a str,
    item_id: Option<&'a str>,
    list_reference: Option<&'a str>,
    status: Option<&'a str>,
    item_order: Option<u64>,
    event_sequence: Option<u64>,
    text: &'a str,
}

fn lifecycle(fixture: WorkflowLifecycleFixture<'_>) -> CanonicalObservationFactV1 {
    let WorkflowLifecycleFixture {
        semantic_kind,
        reference,
        item_id,
        list_reference,
        status,
        item_order,
        event_sequence,
        text,
    } = fixture;
    CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind,
        provider_reference: Some(reference.to_owned()),
        item_id: item_id.map(str::to_owned),
        parent_reference: None,
        list_reference: list_reference.map(str::to_owned),
        state: None,
        status: status.map(str::to_owned),
        item_order,
        revision: None,
        event_sequence,
        content: Some(json!({"text": text})),
    }
}

fn observation(
    session_id: &str,
    record_id: &str,
    record_sequence: u64,
    facts: Vec<CanonicalObservationFactV1>,
) -> DurableObservationV1 {
    let provider = ProviderId::new(FIXTURE_PROVIDER).unwrap();
    let session_id = SessionId::new(session_id).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(record_sequence, record_sequence + 1).unwrap();
    let stable_record_id = ObservationId::new(record_id).unwrap();
    let has_message = facts
        .iter()
        .any(|fact| matches!(fact, CanonicalObservationFactV1::Message { .. }));
    let mut relations = CanonicalObservationRelationsV1::new(session_id);
    if has_message {
        relations = relations.with_message_id(stable_record_id.clone());
    }
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "workflow_fixture",
        stable_record_id.clone(),
        relations,
        facts,
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::DaemonSequence, range)
            .with_native_sequence(record_sequence)
            .with_native_timestamp(1_750_000_000 + i64::try_from(record_sequence).unwrap()),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::DaemonSequence,
        stable_record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        receipt(
            &format!("receipt.workflow-fixture.{record_sequence}"),
            &payload,
        ),
        RetentionClass::new("retention.workflow-fixture").unwrap(),
        payload,
    )
    .unwrap()
}

fn write(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> ObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap()
}

async fn persist_and_project(
    store: &GlobalDbObservationStore<'_>,
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> ObservationSourceCursorV1 {
    let observation_id = observation.observation_id().clone();
    let outcome = store
        .persist_observation(write(observation, expected_cursor))
        .await
        .unwrap();
    let receipt = match outcome {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("fixture observation must commit, got {other:?}"),
    };
    assert!(matches!(
        store.project_observation(&observation_id).await.unwrap(),
        ProjectionPersistOutcome::Projected(_)
    ));
    receipt.committed_cursor().clone()
}

async fn workflow_rows(tmp: &TempDir) -> Vec<WorkflowRow> {
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = raw_db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT semantic_kind, provider_reference, item_id, parent_reference,
                    list_reference, state, status, item_order, native_revision,
                    event_sequence, content_text
             FROM observation_workflow_facts
             ORDER BY observation_sequence, fact_ordinal",
            (),
        )
        .await
        .unwrap();
    let mut projected = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        projected.push(WorkflowRow {
            semantic_kind: row.get(0).unwrap(),
            provider_reference: row.get(1).unwrap(),
            item_id: row.get(2).unwrap(),
            parent_reference: row.get(3).unwrap(),
            list_reference: row.get(4).unwrap(),
            state: row.get(5).unwrap(),
            status: row.get(6).unwrap(),
            item_order: row.get(7).unwrap(),
            revision: row.get(8).unwrap(),
            event_sequence: row.get(9).unwrap(),
            content_text: row.get(10).unwrap(),
        });
    }
    projected
}

async fn table_count(tmp: &TempDir, table: &str) -> i64 {
    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(tmp))
        .build()
        .await
        .unwrap();
    let conn = raw_db.connect().unwrap();
    let quoted = table.replace('"', "\"\"");
    let mut rows = conn
        .query(&format!("SELECT COUNT(*) FROM \"{quoted}\""), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn workflow_count(tmp: &TempDir) -> i64 {
    table_count(tmp, "observation_workflow_facts").await
}

#[tokio::test]
async fn legacy_plan_and_task_facts_replay_into_canonical_workflow_rows() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        FIXTURE_SESSION,
        "record.workflow-legacy",
        1,
        vec![
            CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Plan,
                reference: Some("legacy.plan.1".to_owned()),
                content: Some(json!({"text": "legacy release plan"})),
            },
            CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Task,
                reference: Some("legacy.task.1".to_owned()),
                content: Some(json!({"text": "legacy release task"})),
            },
        ],
    );
    persist_and_project(&store, candidate, None).await;

    let rows = workflow_rows(&tmp).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].semantic_kind, "plan");
    assert_eq!(rows[1].semantic_kind, "task");
    assert_eq!(
        rows.iter()
            .map(|row| row.provider_reference.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("legacy.plan.1"), Some("legacy.task.1")]
    );
    assert!(rows.iter().all(|row| row.state.is_none()));
    assert!(rows.iter().all(|row| row.status.is_none()));
}

#[tokio::test]
async fn message_and_every_colocated_workflow_fact_project_independently() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        FIXTURE_SESSION,
        "record.workflow-multi",
        1,
        vec![
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": "workflow summary survives"}),
                model: None,
                timestamp: Some(1_750_000_001),
            },
            lifecycle(WorkflowLifecycleFixture {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Plan,
                reference: "plan.native.1",
                item_id: Some("plan.stable.1"),
                list_reference: None,
                status: Some("active"),
                item_order: None,
                event_sequence: Some(10),
                text: "release plan",
            }),
            lifecycle(WorkflowLifecycleFixture {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
                reference: "task.native.1",
                item_id: Some("task.stable.1"),
                list_reference: None,
                status: Some("pending"),
                item_order: Some(0),
                event_sequence: Some(11),
                text: "release task alpha",
            }),
            lifecycle(WorkflowLifecycleFixture {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
                reference: "task.native.2",
                item_id: Some("task.stable.2"),
                list_reference: None,
                status: Some("completed"),
                item_order: Some(1),
                event_sequence: Some(12),
                text: "release task beta",
            }),
        ],
    );
    let observation_id = candidate.observation_id().clone();
    persist_and_project(&store, candidate, None).await;

    let rows = workflow_rows(&tmp).await;
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|row| row.provider_reference.as_deref().unwrap())
            .collect::<Vec<_>>(),
        vec!["plan.native.1", "task.native.1", "task.native.2"]
    );
    assert!(
        db.search_session_messages(
            FIXTURE_PROVIDER,
            Some("user"),
            "workflow summary survives",
            10
        )
        .await
        .iter()
        .any(|result| result.message.text == "workflow summary survives")
    );
    let workflow_results = db
        .search_session_messages(FIXTURE_PROVIDER, Some("user"), "release task", 10)
        .await;
    assert_eq!(workflow_results.len(), 2);
    assert!(matches!(
        store.project_observation(&observation_id).await.unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(workflow_count(&tmp).await, 3);
}

#[tokio::test]
async fn latest_goal_state_filters_provider_session_and_status() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let first = observation(
        FIXTURE_SESSION,
        "record.goal-started",
        1,
        vec![lifecycle(WorkflowLifecycleFixture {
            semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
            reference: "goal.native.main",
            item_id: Some("goal.stable.main"),
            list_reference: None,
            status: Some("in_progress"),
            item_order: None,
            event_sequence: Some(1),
            text: "ship the release",
        })],
    );
    let cursor = persist_and_project(&store, first, None).await;
    let completed = observation(
        FIXTURE_SESSION,
        "record.goal-completed",
        2,
        vec![lifecycle(WorkflowLifecycleFixture {
            semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
            reference: "goal.native.main",
            item_id: Some("goal.stable.main"),
            list_reference: None,
            status: Some("completed"),
            item_order: None,
            event_sequence: Some(2),
            text: "ship the release",
        })],
    );
    persist_and_project(&store, completed, Some(cursor)).await;

    let goals = db
        .recent_session_goals_filtered(
            Some(FIXTURE_PROVIDER),
            Some("user"),
            Some(FIXTURE_SESSION),
            Some("completed"),
            10,
        )
        .await;
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0].message.kind.as_deref(), Some("goal"));
    let metadata: Value =
        serde_json::from_str(goals[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["status"], "completed");
    assert_eq!(metadata["item_id"], "goal.stable.main");
    assert_eq!(metadata["event_sequence"], 2);
    assert!(
        db.recent_session_goals_filtered(
            Some(FIXTURE_PROVIDER),
            Some("user"),
            Some(FIXTURE_SESSION),
            Some("in_progress"),
            10,
        )
        .await
        .is_empty(),
        "status filtering must apply after selecting the latest transition"
    );
    assert!(
        db.recent_session_goals_filtered(
            Some("another-provider"),
            Some("user"),
            Some(FIXTURE_SESSION),
            Some("completed"),
            10,
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn todo_item_search_uses_native_list_order_without_inventing_absent_fields() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        FIXTURE_SESSION,
        "record.todo-list",
        1,
        vec![
            lifecycle(WorkflowLifecycleFixture {
                semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
                reference: "todo.native.second",
                item_id: Some("todo.stable.second"),
                list_reference: Some("list.native.release"),
                status: None,
                item_order: Some(2),
                event_sequence: None,
                text: "release-item second",
            }),
            lifecycle(WorkflowLifecycleFixture {
                semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
                reference: "todo.native.first",
                item_id: Some("todo.stable.first"),
                list_reference: Some("list.native.release"),
                status: None,
                item_order: Some(1),
                event_sequence: None,
                text: "release-item first",
            }),
        ],
    );
    persist_and_project(&store, candidate, None).await;

    let results = db
        .search_session_messages(FIXTURE_PROVIDER, Some("user"), "release-item", 10)
        .await;
    assert_eq!(results.len(), 2);
    let metadata = results
        .iter()
        .map(|result| {
            serde_json::from_str::<Value>(result.message.metadata_json.as_deref().unwrap()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(metadata[0]["item_order"], 1);
    assert_eq!(metadata[1]["item_order"], 2);
    assert!(metadata.iter().all(|value| value.get("status").is_none()));
    assert!(metadata.iter().all(|value| value.get("revision").is_none()));
}

#[tokio::test]
async fn canonical_workflow_fact_survives_a_saturated_transcript_limit() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        FIXTURE_SESSION,
        "record.todo-search-saturation",
        1,
        vec![lifecycle(WorkflowLifecycleFixture {
            semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
            reference: "todo.native.saturation",
            item_id: Some("todo.stable.saturation"),
            list_reference: Some("list.native.release"),
            status: Some("pending"),
            item_order: Some(1),
            event_sequence: None,
            text: "canonical saturation marker",
        })],
    );
    persist_and_project(&store, candidate, None).await;

    for ordinal in 0..12 {
        assert!(
            db.upsert_session_message(&global_message(
                FIXTURE_PROVIDER,
                &format!("ordinary-saturation-{ordinal}"),
                FIXTURE_SESSION,
                &format!("canonical saturation marker in transcript {ordinal}"),
            ))
            .await
        );
    }

    let results = db
        .search_session_messages(
            FIXTURE_PROVIDER,
            Some("user"),
            "canonical saturation marker",
            4,
        )
        .await;

    assert_eq!(results.len(), 4);
    assert_eq!(results[0].message.kind.as_deref(), Some("todo_item"));
    assert!(results[0].message.message_id.starts_with("workflow/"));
}

#[tokio::test]
async fn untyped_message_fields_never_become_workflow_projection_rows() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        FIXTURE_SESSION,
        "record.untyped-task-shaped-message",
        1,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({
                "task": {"id": "must-not-project", "status": "completed"},
                "todo_items": [{"content": "must-not-project"}],
                "text": "ordinary authored content"
            }),
            model: None,
            timestamp: None,
        }],
    );
    persist_and_project(&store, candidate, None).await;

    assert_eq!(workflow_count(&tmp).await, 0);
}

#[tokio::test]
async fn workflow_projection_rolls_back_rebuilds_restarts_and_audits() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        FIXTURE_SESSION,
        "record.workflow-recovery",
        1,
        vec![lifecycle(WorkflowLifecycleFixture {
            semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
            reference: "task.native.recovery",
            item_id: Some("task.stable.recovery"),
            list_reference: None,
            status: Some("in_progress"),
            item_order: None,
            event_sequence: Some(1),
            text: "recovery lifecycle canary",
        })],
    );
    let observation_id = candidate.observation_id().clone();
    let persisted = store
        .persist_observation(write(candidate, None))
        .await
        .unwrap();
    assert!(matches!(persisted, ObservationPersistOutcome::Committed(_)));

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER fail_workflow_projection
             BEFORE INSERT ON observation_workflow_facts BEGIN
                SELECT RAISE(ABORT, 'injected workflow projection failure');
             END;",
        )
        .await
        .unwrap();
    let error = store
        .project_observation(&observation_id)
        .await
        .expect_err("workflow row failure must roll back");
    assert!(matches!(error, ProjectionStoreError::Storage { .. }));
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );
    assert_eq!(workflow_count(&tmp).await, 0);
    raw_conn
        .execute("DROP TRIGGER fail_workflow_projection", ())
        .await
        .unwrap();
    assert!(matches!(
        store.project_observation(&observation_id).await.unwrap(),
        ProjectionPersistOutcome::Projected(_)
    ));
    let before = workflow_rows(&tmp).await;
    assert_eq!(before.len(), 1);
    raw_conn
        .execute_batch(
            "CREATE TRIGGER fail_workflow_rebuild_activation
             BEFORE INSERT ON observation_workflow_facts BEGIN
                SELECT RAISE(ABORT, 'injected workflow rebuild activation failure');
             END;",
        )
        .await
        .unwrap();
    let error = store
        .rebuild_projection(1)
        .await
        .expect_err("activation failure must roll back the active projection atomically");
    assert!(matches!(error, ProjectionStoreError::Storage { .. }));
    assert_eq!(workflow_rows(&tmp).await, before);
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        1
    );
    assert_eq!(
        table_count(&tmp, "observation_projection_rebuilds").await,
        1,
        "the durable staged generation must survive a failed activation"
    );
    raw_conn
        .execute("DROP TRIGGER fail_workflow_rebuild_activation", ())
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);
    drop(db);

    let reopened = open_lcm_db(&tmp).await;
    let reopened_store = GlobalDbObservationStore::new(&reopened);
    let rebuilt = reopened_store.rebuild_projection(1).await.unwrap();
    assert_eq!(rebuilt.projected_rows(), 1);
    assert_eq!(workflow_rows(&tmp).await, before);
    assert_eq!(
        table_count(&tmp, "observation_projection_rebuilds").await,
        0
    );
    drop(reopened);

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute(
            "UPDATE observation_workflow_facts
             SET status = 'tampered'
             WHERE item_id = ?1",
            params!["task.stable.recovery"],
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    assert!(
        GlobalDb::try_open_at_without_structured_backfill(&isolated_lcm_db_path(&tmp))
            .await
            .is_err(),
        "authority audit must reject a workflow projection row that disagrees with its observation"
    );
}

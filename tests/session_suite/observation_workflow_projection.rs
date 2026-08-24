use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, CanonicalWorkflowEvidenceKindV1,
    CanonicalWorkflowSemanticKindV1, ComponentVersion, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationWrite, ProjectionPersistOutcome, ProjectionStoreError,
    SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageRecord,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};
use tracedecay_usecases::host_admission::HostAdmissionScope;

use crate::common::global_message;

const FIXTURE_PROVIDER: &str = "provider-neutral-fixture";
const FIXTURE_SESSION: &str = "session.workflow-lifecycle";

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

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
) -> AnchoredObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION).unwrap();
    let authorization = build_observation_resolution_authorization_v1(
        write.observation(),
        "observation-workflow-test.v1",
    )
    .unwrap();
    let retrieval_anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, retrieval_anchor, projection_generation).unwrap()
}

async fn persist_and_project<S>(
    store: &S,
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> ObservationSourceCursorV1
where
    S: ObservationStore + ObservationProjectionStore,
{
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

fn workflow_rows(database_path: &Path) -> Vec<WorkflowRow> {
    let conn = rusqlite::Connection::open(database_path).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT semantic_kind, provider_reference, item_id, parent_reference,
                    list_reference, state, status, item_order, native_revision,
                    event_sequence, content_text
             FROM observation_workflow_facts
             ORDER BY observation_sequence, fact_ordinal",
        )
        .unwrap();
    statement
        .query_map((), |row| {
            Ok(WorkflowRow {
                semantic_kind: row.get(0)?,
                provider_reference: row.get(1)?,
                item_id: row.get(2)?,
                parent_reference: row.get(3)?,
                list_reference: row.get(4)?,
                state: row.get(5)?,
                status: row.get(6)?,
                item_order: row.get(7)?,
                revision: row.get(8)?,
                event_sequence: row.get(9)?,
                content_text: row.get(10)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn table_count(database_path: &Path, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(database_path).unwrap();
    let quoted = table.replace('"', "\"\"");
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), (), |row| {
        row.get(0)
    })
    .unwrap()
}

fn workflow_count(database_path: &Path) -> i64 {
    table_count(database_path, "observation_workflow_facts")
}

struct SessionMessageSearchHit {
    message: SessionMessageRecord,
}

fn workflow_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionMessageRecord> {
    let observation_id: String = row.get(1)?;
    let fact_ordinal: i64 = row.get(2)?;
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "observation_id".to_owned(),
        Value::String(observation_id.clone()),
    );
    metadata.insert("fact_ordinal".to_owned(), Value::from(fact_ordinal));
    metadata.insert("ordering_domain".to_owned(), Value::String(row.get(17)?));
    for (key, value) in [
        ("provider_reference", row.get(5)?),
        ("item_id", row.get(6)?),
        ("parent_reference", row.get(7)?),
        ("list_reference", row.get(8)?),
        ("state", row.get(9)?),
        ("status", row.get(10)?),
        ("revision", row.get(12)?),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), Value::String(value));
        }
    }
    let event_sequence: Option<i64> = row.get(13)?;
    let source_sequence: Option<i64> = row.get(14)?;
    for (key, value) in [
        ("item_order", row.get(11)?),
        ("event_sequence", event_sequence),
        ("source_sequence", source_sequence),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), Value::from(value));
        }
    }
    if let Some(content_json) = row.get::<_, Option<String>>(18)?
        && let Ok(content) = serde_json::from_str(&content_json)
    {
        metadata.insert("content".to_owned(), content);
    }
    let observation_sequence: i64 = row.get(15)?;

    Ok(SessionMessageRecord {
        provider: row.get(0)?,
        message_id: format!("workflow/{observation_id}/{fact_ordinal}"),
        session_id: row.get(3)?,
        role: "system".to_owned(),
        timestamp: row.get(16)?,
        ordinal: event_sequence
            .or(source_sequence)
            .unwrap_or(observation_sequence),
        text: row.get(19)?,
        kind: row.get(4)?,
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: Some(Value::Object(metadata).to_string()),
    })
}

fn search_session_messages(
    database_path: &Path,
    provider: &str,
    project_key: Option<&str>,
    query: &str,
    limit: usize,
) -> Vec<SessionMessageSearchHit> {
    let conn = rusqlite::Connection::open(database_path).unwrap();
    let terms = query
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|character: char| {
                !character.is_alphanumeric() && character != '-' && character != '_'
            })
            .to_lowercase()
        })
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut workflow_sql = "SELECT
            w.provider, w.observation_id, w.fact_ordinal, w.session_id, w.semantic_kind,
            w.provider_reference, w.item_id, w.parent_reference, w.list_reference,
            w.state, w.status, w.item_order, w.native_revision, w.event_sequence,
            w.source_sequence, w.observation_sequence, w.native_timestamp,
            w.ordering_domain, w.content_json, w.content_text
         FROM observation_workflow_facts w
         JOIN sessions s ON s.provider = w.provider AND s.session_id = w.session_id
         WHERE w.projector_version = 'claude-session-message-v4'
           AND w.provider = ?1"
        .to_owned();
    let mut values = vec![rusqlite::types::Value::Text(provider.to_owned())];
    if let Some(project_key) = project_key {
        values.push(rusqlite::types::Value::Text(project_key.to_owned()));
        workflow_sql.push_str(&format!(
            " AND (s.project_key = ?{0} OR s.project_path = ?{0})",
            values.len()
        ));
    }
    for term in &terms {
        values.push(rusqlite::types::Value::Text(term.clone()));
        workflow_sql.push_str(&format!(
            " AND instr(lower(w.content_text), ?{}) > 0",
            values.len()
        ));
    }
    workflow_sql.push_str(
        " ORDER BY CASE WHEN w.item_order IS NULL THEN 1 ELSE 0 END,
                   w.item_order, COALESCE(w.native_timestamp, 0) DESC,
                   w.observation_sequence DESC, w.fact_ordinal",
    );
    let mut statement = conn.prepare(&workflow_sql).unwrap();
    let mut results = statement
        .query_map(rusqlite::params_from_iter(values), |row| {
            Ok(SessionMessageSearchHit {
                message: workflow_message(row)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    if results.len() < limit {
        let fts_query = terms
            .iter()
            .map(|term| format!("\"{}\"*", term.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut statement = conn
            .prepare(
                "SELECT message.provider, message.message_id, message.session_id, message.role,
                        message.timestamp, message.ordinal, message.text, message.kind,
                        message.model, message.tool_names, message.source_path,
                        message.source_offset, message.metadata_json
                 FROM session_messages_fts
                 JOIN session_messages AS message
                   ON message.rowid = session_messages_fts.rowid
                 JOIN sessions AS session
                   ON session.provider = message.provider
                  AND session.session_id = message.session_id
                 WHERE session_messages_fts MATCH ?1
                   AND message.provider = ?2
                   AND (?3 IS NULL OR session.project_key = ?3 OR session.project_path = ?3)
                 ORDER BY bm25(session_messages_fts)
                 LIMIT ?4",
            )
            .unwrap();
        let transcript_results = statement
            .query_map(
                rusqlite::params![
                    fts_query,
                    provider,
                    project_key,
                    i64::try_from(limit - results.len()).unwrap()
                ],
                |row| {
                    Ok(SessionMessageSearchHit {
                        message: SessionMessageRecord {
                            provider: row.get(0)?,
                            message_id: row.get(1)?,
                            session_id: row.get(2)?,
                            role: row.get(3)?,
                            timestamp: row.get(4)?,
                            ordinal: row.get(5)?,
                            text: row.get(6)?,
                            kind: row.get(7)?,
                            model: row.get(8)?,
                            tool_names: row.get(9)?,
                            source_path: row.get(10)?,
                            source_offset: row.get(11)?,
                            metadata_json: row.get(12)?,
                        },
                    })
                },
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        results.extend(transcript_results);
    }
    results.truncate(limit);
    results
}

fn recent_session_goals_filtered(
    database_path: &Path,
    provider: Option<&str>,
    project_key: Option<&str>,
    session_id: Option<&str>,
    status: Option<&str>,
    limit: usize,
) -> Vec<SessionMessageSearchHit> {
    let conn = rusqlite::Connection::open(database_path).unwrap();
    let mut statement = conn
        .prepare(
            "WITH ranked_goals AS (
                 SELECT w.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY w.provider, w.session_id
                            ORDER BY w.observation_sequence DESC, w.fact_ordinal DESC
                        ) AS goal_rank
                 FROM observation_workflow_facts w
                 WHERE w.projector_version = 'claude-session-message-v4'
                   AND w.semantic_kind = 'goal'
             )
             SELECT
                 w.provider, w.observation_id, w.fact_ordinal, w.session_id, w.semantic_kind,
                 w.provider_reference, w.item_id, w.parent_reference, w.list_reference,
                 w.state, w.status, w.item_order, w.native_revision, w.event_sequence,
                 w.source_sequence, w.observation_sequence, w.native_timestamp,
                 w.ordering_domain, w.content_json, w.content_text
             FROM ranked_goals w
             JOIN sessions s ON s.provider = w.provider AND s.session_id = w.session_id
             WHERE w.goal_rank = 1
               AND (?1 IS NULL OR w.provider = ?1)
               AND (?2 IS NULL OR s.project_key = ?2 OR s.project_path = ?2)
               AND (?3 IS NULL OR w.session_id = ?3)
               AND (?4 IS NULL OR w.status = ?4)
             ORDER BY COALESCE(w.native_timestamp, 0) DESC,
                      w.observation_sequence DESC, w.fact_ordinal DESC
             LIMIT ?5",
        )
        .unwrap();
    statement
        .query_map(
            rusqlite::params![
                provider,
                project_key,
                session_id,
                status,
                i64::try_from(limit).unwrap()
            ],
            |row| {
                Ok(SessionMessageSearchHit {
                    message: workflow_message(row)?,
                })
            },
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn upsert_session_message(database_path: &Path, message: &SessionMessageRecord) -> bool {
    rusqlite::Connection::open(database_path)
        .and_then(|conn| {
            conn.execute(
                "INSERT INTO session_messages
                     (provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                      model, tool_names, source_path, source_offset, metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(provider, message_id) DO UPDATE SET
                    session_id = excluded.session_id,
                    role = excluded.role,
                    timestamp = excluded.timestamp,
                    ordinal = excluded.ordinal,
                    text = excluded.text,
                    kind = excluded.kind,
                    model = excluded.model,
                    tool_names = excluded.tool_names,
                    source_path = excluded.source_path,
                    source_offset = excluded.source_offset,
                    metadata_json = excluded.metadata_json",
                rusqlite::params![
                    message.provider,
                    message.message_id,
                    message.session_id,
                    message.role,
                    message.timestamp,
                    message.ordinal,
                    message.text,
                    message.kind,
                    message.model,
                    message.tool_names,
                    message.source_path,
                    message.source_offset,
                    message.metadata_json,
                ],
            )
        })
        .is_ok()
}

#[tokio::test]
async fn legacy_plan_and_task_facts_replay_into_canonical_workflow_rows() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
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

    let rows = workflow_rows(&database_path);
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
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
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

    let rows = workflow_rows(&database_path);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|row| row.provider_reference.as_deref().unwrap())
            .collect::<Vec<_>>(),
        vec!["plan.native.1", "task.native.1", "task.native.2"]
    );
    assert!(
        search_session_messages(
            &database_path,
            FIXTURE_PROVIDER,
            Some("user"),
            "workflow summary survives",
            10
        )
        .iter()
        .any(|result| result.message.text == "workflow summary survives")
    );
    let workflow_results = search_session_messages(
        &database_path,
        FIXTURE_PROVIDER,
        Some("user"),
        "release task",
        10,
    );
    assert_eq!(workflow_results.len(), 2);
    assert!(matches!(
        store.project_observation(&observation_id).await.unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(workflow_count(&database_path), 3);
}

#[tokio::test]
async fn latest_goal_state_filters_provider_session_and_status() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
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

    let goals = recent_session_goals_filtered(
        &database_path,
        Some(FIXTURE_PROVIDER),
        Some("user"),
        Some(FIXTURE_SESSION),
        Some("completed"),
        10,
    );
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0].message.kind.as_deref(), Some("goal"));
    let metadata: Value =
        serde_json::from_str(goals[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["status"], "completed");
    assert_eq!(metadata["item_id"], "goal.stable.main");
    assert_eq!(metadata["event_sequence"], 2);
    assert!(
        recent_session_goals_filtered(
            &database_path,
            Some(FIXTURE_PROVIDER),
            Some("user"),
            Some(FIXTURE_SESSION),
            Some("in_progress"),
            10,
        )
        .is_empty(),
        "status filtering must apply after selecting the latest transition"
    );
    assert!(
        recent_session_goals_filtered(
            &database_path,
            Some("another-provider"),
            Some("user"),
            Some(FIXTURE_SESSION),
            Some("completed"),
            10,
        )
        .is_empty()
    );
}

#[tokio::test]
async fn todo_item_search_uses_native_list_order_without_inventing_absent_fields() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
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

    let results = search_session_messages(
        &database_path,
        FIXTURE_PROVIDER,
        Some("user"),
        "release-item",
        10,
    );
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
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
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
        assert!(upsert_session_message(
            &database_path,
            &global_message(
                FIXTURE_PROVIDER,
                &format!("ordinary-saturation-{ordinal}"),
                FIXTURE_SESSION,
                &format!("canonical saturation marker in transcript {ordinal}"),
            ),
        ));
    }

    let results = search_session_messages(
        &database_path,
        FIXTURE_PROVIDER,
        Some("user"),
        "canonical saturation marker",
        4,
    );

    assert_eq!(results.len(), 4);
    assert_eq!(results[0].message.kind.as_deref(), Some("todo_item"));
    assert!(results[0].message.message_id.starts_with("workflow/"));
}

#[tokio::test]
async fn untyped_message_fields_never_become_workflow_projection_rows() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
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

    assert_eq!(workflow_count(&database_path), 0);
}

#[tokio::test]
async fn workflow_projection_rolls_back_rebuilds_restarts_and_audits() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
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

    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    raw_conn
        .execute_batch(
            "CREATE TRIGGER fail_workflow_projection
             BEFORE INSERT ON observation_workflow_facts BEGIN
                SELECT RAISE(ABORT, 'injected workflow projection failure');
             END;",
        )
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
    assert_eq!(workflow_count(&database_path), 0);
    raw_conn
        .execute("DROP TRIGGER fail_workflow_projection", ())
        .unwrap();
    assert!(matches!(
        store.project_observation(&observation_id).await.unwrap(),
        ProjectionPersistOutcome::Projected(_)
    ));
    let before = workflow_rows(&database_path);
    assert_eq!(before.len(), 1);
    raw_conn
        .execute_batch(
            "CREATE TRIGGER fail_workflow_rebuild_activation
             BEFORE INSERT ON observation_workflow_facts BEGIN
                SELECT RAISE(ABORT, 'injected workflow rebuild activation failure');
             END;",
        )
        .unwrap();
    let error = store
        .rebuild_projection(1)
        .await
        .expect_err("activation failure must roll back the active projection atomically");
    assert!(matches!(error, ProjectionStoreError::Storage { .. }));
    assert_eq!(workflow_rows(&database_path), before);
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        1
    );
    assert_eq!(
        table_count(&database_path, "observation_projection_rebuilds"),
        1,
        "the durable staged generation must survive a failed activation"
    );
    raw_conn
        .execute("DROP TRIGGER fail_workflow_rebuild_activation", ())
        .unwrap();
    drop(raw_conn);
    drop(runtime);

    let reopened = profile_runtime(&tmp).await;
    let reopened_store = reopened
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let rebuilt = reopened_store.rebuild_projection(1).await.unwrap();
    assert!(rebuilt.is_complete());
    assert_eq!(rebuilt.projected_rows(), 1);
    assert_eq!(workflow_rows(&database_path), before);
    assert_eq!(
        table_count(&database_path, "observation_projection_rebuilds"),
        0
    );
    drop(reopened);

    let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
    raw_conn
        .execute(
            "UPDATE observation_workflow_facts
             SET status = 'tampered'
             WHERE item_id = ?1",
            rusqlite::params!["task.stable.recovery"],
        )
        .unwrap();
    drop(raw_conn);

    assert!(
        HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
            .await
            .is_err(),
        "authority audit must reject a workflow projection row that disagrees with its observation"
    );
}

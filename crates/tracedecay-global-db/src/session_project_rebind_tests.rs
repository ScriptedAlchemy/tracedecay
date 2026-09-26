//! A host session is keyed by provider and session id. LCM may insert that
//! row before the rollout projection, and a later project binding must replace
//! the placeholder. A genuine collision stays on that queue row and must not
//! stop later sessions.

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_sessions::runtime::shared::durable_project_path_key;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationWrite,
};

use crate::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, reopen_project_sessions_database,
};
use crate::{GlobalDbObservationStore, RegisteredGlobalDb};

const CODEX_SESSION: &str = "01a0da2f-45db-7572-bd15-e0b83c7a09d3";
const CURSOR_SESSION: &str = "session.cursor-catch-up";
const ORIGINAL_TEXT: &str = "codex original project body";
const CURRENT_TEXT: &str = "codex current project body";
const CURSOR_TEXT: &str = "cursor catch-up still projected";

fn project_scope(project_id: &str) -> ObservationScopeV1 {
    ObservationScopeV1::Project {
        project_id: ProjectId::new(project_id).unwrap(),
    }
}

fn receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.project-rebind.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn session_fact(cwd: &str) -> CanonicalObservationFactV1 {
    CanonicalObservationFactV1::Session {
        project_path: Some(cwd.to_owned()),
        location_path: Some(cwd.to_owned()),
        transcript_path: None,
        title: None,
        started_at: None,
        ended_at: None,
        source: Some("codex_rollout".to_owned()),
        native_source: None,
        profile: None,
        location_provenance: None,
    }
}

fn observation(
    provider: &str,
    session_id: &str,
    scope: ObservationScopeV1,
    record_id: &str,
    text: &str,
    receipt_id: &str,
    session: Option<CanonicalObservationFactV1>,
) -> DurableObservationV1 {
    let provider_id = ProviderId::new(provider).unwrap();
    let session_id = SessionId::new(session_id).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider_id.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let record = ObservationId::new(record_id).unwrap();
    let mut facts = Vec::new();
    if let Some(session) = session {
        facts.push(session);
    }
    facts.push(CanonicalObservationFactV1::Message {
        role: CanonicalMessageRoleV1::Assistant,
        content: json!({"text": text}),
        model: None,
        timestamp: Some(1_750_000_000),
    });
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider_id,
        "message",
        record.clone(),
        CanonicalObservationRelationsV1::new(session_id)
            .with_message_id(ObservationId::new(format!("message.{record_id}")).unwrap()),
        facts,
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            scope,
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record,
        )
        .unwrap(),
        receipt(receipt_id, &payload),
        RetentionClass::new("retention.project-rebind").unwrap(),
        payload,
    )
    .unwrap()
}

fn anchored(observation: DurableObservationV1) -> AnchoredObservationWrite {
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, None, next_cursor).unwrap();
    let generation = ProjectionGenerationId::new("projection.project-rebind.v1").unwrap();
    let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
        write.observation(),
        "project-rebind",
    )
    .unwrap();
    let anchor = tracedecay_store::build_observation_retrieval_anchor(
        write.observation(),
        generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, generation).unwrap()
}

async fn persist(store: &GlobalDbObservationStore, observation: DurableObservationV1) {
    match store
        .persist_observation(anchored(observation))
        .await
        .unwrap()
    {
        ObservationPersistOutcome::Committed(_) => {}
        other => panic!("observation must commit, got {other:?}"),
    }
}

async fn session_project_binding(
    database: &RegisteredGlobalDb,
    provider: &str,
    session_id: &str,
) -> (String, String) {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT project_key, project_path FROM sessions
             WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    (row.get::<String>(0).unwrap(), row.get::<String>(1).unwrap())
}

async fn projected_text_contains(
    database: &RegisteredGlobalDb,
    provider: &str,
    session_id: &str,
    needle: &str,
) -> bool {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COALESCE(content, placeholder_text, '') FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        let text: String = row.get(0).unwrap();
        if text.contains(needle) {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn changed_project_key_rebinds_without_blocking_other_sessions() {
    let tmp = TempDir::new().unwrap();
    let profile = tmp.path().join("profile");
    let project_root = tmp.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let cwd = project_root.to_string_lossy().into_owned();
    let alpha = HostAdmissionTestRuntimeV1::project(
        &profile,
        &project_root,
        ProjectId::new("project.alpha").unwrap(),
    )
    .await
    .unwrap();
    let alpha_store = alpha
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    persist(
        &alpha_store,
        observation(
            "codex",
            CODEX_SESSION,
            project_scope("project.alpha"),
            "record.codex.original",
            ORIGINAL_TEXT,
            "receipt.codex.original",
            Some(session_fact(&cwd)),
        ),
    )
    .await;
    alpha_store
        .project_observation(
            &alpha_store
                .next_queued_observation()
                .await
                .unwrap()
                .expect("the first projection must be queued"),
        )
        .await
        .expect("the session must project under the original project");

    // Re-enroll keeps this sessions file and admits the current project id.
    let sessions_db = alpha
        .database_path(HostAdmissionScope::Project)
        .expect("project sessions database")
        .to_path_buf();
    let (beta, _beta_owner) =
        reopen_project_sessions_database(&sessions_db, ProjectId::new("project.beta").unwrap())
            .await
            .expect("the re-enrolled project must reopen the existing sessions database");
    let beta_store = beta.observation_store();
    persist(
        &beta_store,
        observation(
            "codex",
            CODEX_SESSION,
            project_scope("project.beta"),
            "record.codex.current",
            CURRENT_TEXT,
            "receipt.codex.current",
            Some(session_fact(&cwd)),
        ),
    )
    .await;
    persist(
        &beta_store,
        observation(
            "cursor",
            CURSOR_SESSION,
            project_scope("project.beta"),
            "record.cursor.catch-up",
            CURSOR_TEXT,
            "receipt.cursor.catch-up",
            None,
        ),
    )
    .await;

    while let Some(observation_id) = beta_store.next_queued_observation().await.unwrap() {
        beta_store
            .project_observation(&observation_id)
            .await
            .expect("one session's project rebind must not abort catch-up");
    }

    let (project_key, project_path) = session_project_binding(&beta, "codex", CODEX_SESSION).await;
    assert_eq!(project_key, "project.beta");
    assert_eq!(project_path, durable_project_path_key(&cwd));
    assert!(
        projected_text_contains(&beta, "codex", CODEX_SESSION, ORIGINAL_TEXT).await,
        "the original projected message must stay"
    );
    assert!(
        projected_text_contains(&beta, "codex", CODEX_SESSION, CURRENT_TEXT).await,
        "the observation that changed the project must still project"
    );
    assert!(
        projected_text_contains(&beta, "cursor", CURSOR_SESSION, CURSOR_TEXT).await,
        "a later session must catch up after the project rebind"
    );

    assert!(
        beta_store
            .next_queued_observation()
            .await
            .unwrap()
            .is_none(),
        "catch-up must consume the rebinding observation instead of retrying it"
    );
}

#[tokio::test]
async fn lcm_ensure_session_then_rollout_keeps_the_real_project() {
    let tmp = TempDir::new().unwrap();
    let profile = tmp.path().join("profile");
    let project_root = tmp.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let cwd = project_root.to_string_lossy().into_owned();
    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile,
        &project_root,
        ProjectId::new("project.core").unwrap(),
    )
    .await
    .unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let transaction = database
        .runtime_database()
        .begin_write_transaction("ensure lcm session before rollout projection")
        .await
        .unwrap();
    tracedecay_lcm::compression::ensure_session(&transaction, "codex", CODEX_SESSION)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let (project_key, project_path) =
        session_project_binding(database, "codex", CODEX_SESSION).await;
    assert_eq!(
        project_key,
        tracedecay_lcm::compression::LCM_UNKNOWN_PROJECT_KEY,
        "LCM must not store a fake project key"
    );
    assert_eq!(
        project_path,
        tracedecay_lcm::compression::LCM_UNKNOWN_PROJECT_KEY
    );
    assert!(
        session_title(database, "codex", CODEX_SESSION)
            .await
            .is_none(),
        "the foreign-key shell must not invent a session title"
    );

    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    let rollout = observation(
        "codex",
        CODEX_SESSION,
        project_scope("project.core"),
        "record.codex.rollout",
        ORIGINAL_TEXT,
        "receipt.codex.rollout",
        Some(session_fact(&cwd)),
    );
    persist(&store, rollout).await;
    persist(
        &store,
        observation(
            "cursor",
            CURSOR_SESSION,
            project_scope("project.core"),
            "record.cursor.after-lcm",
            CURSOR_TEXT,
            "receipt.cursor.after-lcm",
            None,
        ),
    )
    .await;
    drain_projection_queue(&store).await;

    let (project_key, project_path) =
        session_project_binding(database, "codex", CODEX_SESSION).await;
    assert_eq!(project_key, "project.core");
    assert_eq!(project_path, durable_project_path_key(&cwd));
    assert!(
        projected_text_contains(database, "codex", CODEX_SESSION, ORIGINAL_TEXT).await,
        "the rollout message must project onto the placeholder session"
    );
    assert!(
        projected_text_contains(database, "cursor", CURSOR_SESSION, CURSOR_TEXT).await,
        "a later session must project after the placeholder is replaced"
    );
    assert!(
        store.next_queued_observation().await.unwrap().is_none(),
        "catch-up must finish both observations"
    );
}

#[tokio::test]
async fn genuine_session_collision_is_recorded_and_later_sessions_project() {
    let tmp = TempDir::new().unwrap();
    let profile = tmp.path().join("profile");
    let project_root = tmp.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let cwd = project_root.to_string_lossy().into_owned();
    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile,
        &project_root,
        ProjectId::new("project.collision").unwrap(),
    )
    .await
    .unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let durable_cwd = durable_project_path_key(&cwd);
    let transaction = database
        .runtime_database()
        .begin_write_transaction("seed a real session that will collide")
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO sessions (
                provider, session_id, project_key, project_path, transcript_path, started_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![
                "codex",
                CODEX_SESSION,
                "project.collision",
                durable_cwd.as_str(),
                "/private/old-transcript.jsonl",
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    let collided = observation(
        "codex",
        CODEX_SESSION,
        project_scope("project.collision"),
        "record.codex.collide",
        ORIGINAL_TEXT,
        "receipt.codex.collide",
        Some(session_fact_with_transcript(
            &cwd,
            "/private/new-transcript.jsonl",
        )),
    );
    let collided_id = collided.observation_id().clone();
    persist(&store, collided).await;
    persist(
        &store,
        observation(
            "cursor",
            CURSOR_SESSION,
            project_scope("project.collision"),
            "record.cursor.after-collision",
            CURSOR_TEXT,
            "receipt.cursor.after-collision",
            None,
        ),
    )
    .await;
    drain_projection_queue(&store).await;

    let error = queue_last_error(database, collided_id.as_str()).await;
    assert!(
        error
            .as_deref()
            .is_some_and(|text| text.contains("transcript_path")),
        "the collided observation must keep its error on the queue row, got {error:?}"
    );
    let (project_key, project_path) =
        session_project_binding(database, "codex", CODEX_SESSION).await;
    assert_eq!(project_key, "project.collision");
    assert_eq!(project_path, durable_cwd);
    assert!(
        projected_text_contains(database, "cursor", CURSOR_SESSION, CURSOR_TEXT).await,
        "a later session must project after the collided one is isolated"
    );
    assert!(
        store.next_queued_observation().await.unwrap().is_none(),
        "the terminal collision must not stay at the head of the queue"
    );
}

fn session_fact_with_transcript(cwd: &str, transcript_path: &str) -> CanonicalObservationFactV1 {
    CanonicalObservationFactV1::Session {
        project_path: Some(cwd.to_owned()),
        location_path: Some(cwd.to_owned()),
        transcript_path: Some(transcript_path.to_owned()),
        title: None,
        started_at: None,
        ended_at: None,
        source: Some("codex_rollout".to_owned()),
        native_source: None,
        profile: None,
        location_provenance: None,
    }
}

async fn drain_projection_queue(store: &GlobalDbObservationStore) {
    while let Some(observation_id) = store.next_queued_observation().await.unwrap() {
        store
            .project_observation(&observation_id)
            .await
            .expect("one session collision must not abort catch-up");
    }
}

async fn session_title(
    database: &RegisteredGlobalDb,
    provider: &str,
    session_id: &str,
) -> Option<String> {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT title FROM sessions WHERE provider = ?1 AND session_id = ?2",
            params![provider, session_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn queue_last_error(database: &RegisteredGlobalDb, observation_id: &str) -> Option<String> {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT last_error FROM projection_queue WHERE observation_id = ?1",
            params![observation_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

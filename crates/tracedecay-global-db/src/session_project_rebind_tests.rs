//! A host session is keyed by provider and session id. LCM may insert that
//! row before the rollout projection, under its store's scope; the rollout
//! then refines its working directory. A genuine collision stays on that
//! queue row and must not stop later sessions.

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
use tracedecay_lcm::{LcmCompressionRequest, LcmSummarizerMode};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_sessions::runtime::shared::durable_project_path_key;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationWrite, ProjectionPersistOutcome, ProjectionSkipReason,
    SESSION_MESSAGE_PROJECTOR_VERSION,
};
use tracedecay_temporal_query::execution::ExecutionControl;

use crate::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, RegisteredGlobalDbHarness,
    bind_test_session_relation_graph, reopen_project_sessions_database,
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
    observation_from_source(
        provider, session_id, None, scope, record_id, text, receipt_id, session,
    )
}

/// `source_key` names the rollout file the record came from, so one session can
/// be read through several sources, each with its own cursor.
#[allow(clippy::too_many_arguments)]
fn observation_from_source(
    provider: &str,
    session_id: &str,
    source_key: Option<&str>,
    scope: ObservationScopeV1,
    record_id: &str,
    text: &str,
    receipt_id: &str,
    session: Option<CanonicalObservationFactV1>,
) -> DurableObservationV1 {
    let provider_id = ProviderId::new(provider).unwrap();
    let session_id = SessionId::new(session_id).unwrap();
    let source = match source_key {
        Some(key) => ObservationSourceIdentityV1::for_provider_source(
            provider_id.clone(),
            session_id.clone(),
            SessionId::new(key).unwrap(),
        )
        .unwrap(),
        None => ObservationSourceIdentityV1::for_provider(provider_id.clone(), session_id.clone())
            .unwrap(),
    };
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

#[asupersync::test]
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

fn lcm_request(provider: &str, session_id: &str) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: provider.to_owned(),
        session_id: session_id.to_owned(),
        messages: vec![json!({"id": "lcm-active-1", "role": "user", "content": "active turn"})],
        current_tokens: Some(100),
        focus_topic: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        overrides: Default::default(),
        summarizer: LcmSummarizerMode::Noop,
    }
}

async fn lcm_compress(database: &RegisteredGlobalDb, provider: &str, session_id: &str) {
    let control = ExecutionControl::new(None);
    let response = database
        .lcm_compress_guarded(&lcm_request(provider, session_id), &control, || Ok(()))
        .await
        .unwrap();
    assert_eq!(response.status, "ok");
}

async fn stored_project(
    runtime: &HostAdmissionTestRuntimeV1,
    scope: HostAdmissionScope,
    provider: &str,
    session_id: &str,
) -> (String, String) {
    let session = runtime
        .session_for_test(scope, provider, session_id)
        .await
        .unwrap()
        .expect("LCM compression stores the session it ingested");
    (session.project_key, session.project_path)
}

#[asupersync::test]
async fn lcm_compression_before_the_rollout_stores_the_session_under_its_store_scope() {
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
    lcm_compress(database, "codex", CODEX_SESSION).await;
    lcm_compress(
        runtime
            .registered_database(HostAdmissionScope::Profile)
            .unwrap(),
        "codex",
        "session.profile-only",
    )
    .await;

    assert_eq!(
        stored_project(
            &runtime,
            HostAdmissionScope::Project,
            "codex",
            CODEX_SESSION
        )
        .await,
        ("project.core".to_owned(), "project.core".to_owned())
    );
    assert_eq!(
        stored_project(
            &runtime,
            HostAdmissionScope::Profile,
            "codex",
            "session.profile-only"
        )
        .await,
        ("user".to_owned(), "user".to_owned())
    );
    assert!(
        session_title(database, "codex", CODEX_SESSION)
            .await
            .is_none(),
        "LCM must not invent a session title"
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

    assert_eq!(
        stored_project(
            &runtime,
            HostAdmissionScope::Project,
            "codex",
            CODEX_SESSION
        )
        .await,
        ("project.core".to_owned(), durable_project_path_key(&cwd))
    );
    assert!(
        projected_text_contains(database, "codex", CODEX_SESSION, ORIGINAL_TEXT).await,
        "the rollout message must project onto the session LCM created"
    );
    assert!(
        projected_text_contains(database, "cursor", CURSOR_SESSION, CURSOR_TEXT).await,
        "a later session must project after the rollout refines the session"
    );
    assert!(
        store.next_queued_observation().await.unwrap().is_none(),
        "catch-up must finish both observations"
    );
}

#[asupersync::test]
async fn a_rollout_starting_after_lcm_compression_keeps_its_own_start_time() {
    const ROLLOUT_STARTED_AT: i64 = 4_102_444_800;
    let tmp = TempDir::new().unwrap();
    let profile = tmp.path().join("profile");
    let project_root = tmp.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let cwd = project_root.to_string_lossy().into_owned();
    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile,
        &project_root,
        ProjectId::new("project.start").unwrap(),
    )
    .await
    .unwrap();
    lcm_compress(
        runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap(),
        "codex",
        CODEX_SESSION,
    )
    .await;
    assert_eq!(
        runtime
            .session_for_test(HostAdmissionScope::Project, "codex", CODEX_SESSION)
            .await
            .unwrap()
            .expect("LCM compression stores the session it ingested")
            .started_at,
        None,
        "LCM did not observe the session start"
    );

    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    let mut rollout_session = session_fact(&cwd);
    let CanonicalObservationFactV1::Session { started_at, .. } = &mut rollout_session else {
        unreachable!("session_fact builds a session fact")
    };
    *started_at = Some(ROLLOUT_STARTED_AT);
    persist(
        &store,
        observation(
            "codex",
            CODEX_SESSION,
            project_scope("project.start"),
            "record.codex.late-start",
            ORIGINAL_TEXT,
            "receipt.codex.late-start",
            Some(rollout_session),
        ),
    )
    .await;
    drain_projection_queue(&store).await;

    assert_eq!(
        runtime
            .session_for_test(HostAdmissionScope::Project, "codex", CODEX_SESSION)
            .await
            .unwrap()
            .expect("the rollout projects onto the LCM session")
            .started_at,
        Some(ROLLOUT_STARTED_AT)
    );
}

#[asupersync::test]
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
    let window = store
        .project_queued_observations(8)
        .await
        .unwrap()
        .expect("the store drains in windows");
    assert!(
        window.items.is_empty() && !window.has_more,
        "a parked terminal collision is not pending work, or every pass reports backpressure"
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

/// beta.88 (950bf21f85) parked a session collision with a durable skip, an
/// advanced checkpoint, and a zero-output temporal effect. Exercise that full
/// state through a registered daemon remount, not just a queued retry deadline.
#[asupersync::test]
async fn registered_upgrade_recovers_parked_codex_messages_once() {
    check_parked_codex_upgrade(false).await;
}

#[asupersync::test]
async fn registered_upgrade_keeps_recovered_codex_messages_after_rebuild_resume() {
    check_parked_codex_upgrade(true).await;
}

async fn check_parked_codex_upgrade(resume_rebuild: bool) {
    let paths = TempDir::new().unwrap();
    let old_cwd = paths.path().join("repo");
    let new_cwd = paths.path().join("worktree");
    std::fs::create_dir_all(&old_cwd).unwrap();
    std::fs::create_dir_all(&new_cwd).unwrap();
    let old_cwd = durable_project_path_key(&old_cwd.to_string_lossy());
    let new_cwd = new_cwd.to_string_lossy().into_owned();
    let old_transcript = paths
        .path()
        .join("old.jsonl")
        .to_string_lossy()
        .into_owned();
    let new_transcript = paths
        .path()
        .join("new.jsonl")
        .to_string_lossy()
        .into_owned();
    let harness = RegisteredGlobalDbHarness::open("recover-parked-codex-messages").await;
    let store = harness.registered.observation_store();
    let sessions = [
        "session.recover-path",
        "session.keep-transcript",
        "session.keep-parent",
        "session.path-masked-transcript",
    ];
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    for (index, session) in sessions.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO sessions (
                    provider, session_id, project_key, project_path,
                    transcript_path, parent_session_id
                 ) VALUES ('codex', ?1, 'user', ?2, ?3, ?4)",
                params![
                    *session,
                    old_cwd.as_str(),
                    (index != 2).then_some(old_transcript.as_str()),
                    (index == 2).then_some("session.parent-before"),
                ],
            )
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();
    let mut parked = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        let record = format!("record.parked-{index}");
        let receipt_id = format!("receipt.parked-{index}");
        let mut write = observation(
            "codex",
            session,
            ObservationScopeV1::Profile,
            &record,
            ORIGINAL_TEXT,
            &receipt_id,
            Some(if index == 2 {
                session_fact(&new_cwd)
            } else {
                session_fact_with_transcript(&new_cwd, &new_transcript)
            }),
        );
        if index == 2 {
            let mut payload = write.payload().clone();
            payload["relations"]["parent_session_id"] = json!("session.parent-after");
            write = DurableObservationV1::new(
                write.identity().clone(),
                receipt(&receipt_id, &payload),
                write.retention_class().clone(),
                payload,
            )
            .unwrap();
        }
        persist(&store, write.clone()).await;
        assert!(matches!(
            store
                .project_observation(write.observation_id())
                .await
                .unwrap(),
            ProjectionPersistOutcome::Skipped {
                reason: ProjectionSkipReason::OutputCollision,
                ..
            }
        ));
        parked.push(write);
    }
    let parked_frontier = projection_checkpoint(&harness.registered).await;
    let later = observation(
        "codex",
        "session.after-parked",
        ObservationScopeV1::Profile,
        "record.after-parked",
        CURRENT_TEXT,
        "receipt.after-parked",
        None,
    );
    persist(&store, later.clone()).await;
    drain_projection_queue(&store).await;

    // The production collision writer above installed every durable side
    // effect of parking. Restore only the released project-path disagreement:
    // the first row has no real transcript collision, while the last also
    // carries one that the old path-first check masked.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "UPDATE sessions SET transcript_path = NULL
             WHERE provider = 'codex' AND session_id = ?1",
            params![sessions[0]],
        )
        .await
        .unwrap();
    for index in [0, 3] {
        transaction
            .execute(
                "UPDATE projection_queue SET last_error = ?2 WHERE observation_id = ?1",
                params![
                    parked[index].observation_id().as_str(),
                    format!(
                        "projection session output collided at codex/{}: project_path differs",
                        sessions[index]
                    ),
                ],
            )
            .await
            .unwrap();
    }
    transaction
        .execute(
            "DELETE FROM tracedecay_schema_version
             WHERE component = 'codex-project-path-collision-rearm-v1'",
            (),
        )
        .await
        .unwrap();
    if resume_rebuild {
        // A suspended rebuild already staged these zero-output skips before
        // upgrade. Its frozen frontier predates the later healthy message.
        transaction
            .execute(
                "INSERT INTO observation_projection_rebuilds (
                    projector_version, generation, frontier_sequence,
                    aliases_staged_through, staged_through, projected_rows,
                    skipped_observations, state
                 ) VALUES (?1, 'generation.parked-codex', ?2, ?2, ?2, 0, ?3, 'ready')",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    parked_frontier,
                    i64::try_from(parked.len()).unwrap(),
                ],
            )
            .await
            .unwrap();
        transaction
            .execute(
                "INSERT INTO observation_projection_rebuild_dispositions (
                    projector_version, generation, observation_id, receipt_id, reason
                 ) SELECT projector_version, 'generation.parked-codex',
                          observation_id, receipt_id, reason
                   FROM observation_projection_dispositions
                   WHERE projector_version = ?1",
                params![SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();
    let checkpoint_before = projection_checkpoint(&harness.registered).await;
    for (index, write) in parked.iter().enumerate() {
        assert_eq!(
            parked_projection_state(&harness.registered, write.observation_id().as_str()).await,
            (Some(i64::MAX), Some("output_collision".to_owned()), Some(0)),
        );
        assert!(
            harness
                .registered
                .get_session_message("codex", &format!("message.record.parked-{index}"))
                .await
                .unwrap()
                .is_none()
        );
    }
    let genuine_attempts = [
        queue_attempts(&harness.registered, parked[1].observation_id().as_str()).await,
        queue_attempts(&harness.registered, parked[2].observation_id().as_str()).await,
    ];
    if !resume_rebuild {
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        transaction
            .execute_batch(
                "CREATE TRIGGER interrupt_recovered_message BEFORE INSERT ON lcm_raw_messages
                 WHEN NEW.message_id = 'message.record.parked-0'
                 BEGIN SELECT RAISE(ABORT, 'interrupted project path recovery'); END",
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let error = crate::observation_projection::rearm_project_path_collisions(
            harness.registered.runtime_database(),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .durable_detail()
                .contains("interrupted project path recovery")
        );
        assert_eq!(
            parked_projection_state(&harness.registered, parked[0].observation_id().as_str()).await,
            (Some(i64::MAX), Some("output_collision".to_owned()), Some(0)),
            "a failed writer rolls the deadline, disposition, and temporal effect back together",
        );
        assert!(
            crate::schema_contract::authority_invariant_triggers_intact(
                &harness.registered.read_connection()
            )
            .await
            .unwrap(),
            "rollback restores the delete guard before another writer runs"
        );
        assert_eq!(
            tracedecay_runtime_core::db::schema_version::read_version(
                &harness.registered.read_connection(),
                "codex-project-path-collision-rearm-v1",
            )
            .await
            .unwrap(),
            None,
        );
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        transaction
            .execute_batch("DROP TRIGGER interrupt_recovered_message")
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }
    drop(store);

    let (harness, convergence) = harness.restart_for_daemon().await;
    bind_test_session_relation_graph(&harness.registered).unwrap();
    harness
        .registered
        .converge_schema(convergence)
        .await
        .unwrap();
    let store = harness.registered.observation_store();
    if resume_rebuild {
        let rebuilt = store
            .rebuild_projection(u64::try_from(parked_frontier).unwrap())
            .await
            .unwrap();
        assert!(rebuilt.is_complete());
        assert_eq!(rebuilt.projected_rows(), 1);
        assert_eq!(rebuilt.skipped_observations(), parked.len() - 1);
    }
    let pass = store
        .project_queued_observations(16)
        .await
        .unwrap()
        .unwrap();
    assert!(!pass.has_more, "one pass must settle the upgraded queue");
    let recovered = harness
        .registered
        .get_session_message("codex", "message.record.parked-0")
        .await
        .unwrap()
        .expect("the formerly parked message is readable after upgrade");
    assert_eq!(recovered.text, ORIGINAL_TEXT);
    assert_eq!(
        session_project_binding(&harness.registered, "codex", sessions[0]).await,
        ("user".to_owned(), old_cwd),
        "recovering a message keeps the existing concrete session directory",
    );
    assert_eq!(
        parked_projection_state(&harness.registered, parked[0].observation_id().as_str()).await,
        (None, None, Some(1)),
    );
    assert_eq!(
        projection_checkpoint(&harness.registered).await,
        checkpoint_before
    );
    for (index, observation) in parked.iter().enumerate().skip(1) {
        assert_eq!(
            parked_projection_state(&harness.registered, observation.observation_id().as_str())
                .await,
            (Some(i64::MAX), Some("output_collision".to_owned()), Some(0)),
        );
        assert!(
            harness
                .registered
                .get_session_message("codex", &format!("message.record.parked-{index}"))
                .await
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        [
            queue_attempts(&harness.registered, parked[1].observation_id().as_str()).await,
            queue_attempts(&harness.registered, parked[2].observation_id().as_str()).await,
        ],
        genuine_attempts,
        "unrelated transcript and lineage collisions retain their attempt history",
    );
    assert!(
        queue_last_error(&harness.registered, parked[3].observation_id().as_str())
            .await
            .unwrap()
            .contains("transcript_path")
    );
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut resets = snapshot
        .query(
            "SELECT COUNT(*) FROM session_temporal_resets WHERE session_id = ?1",
            params![sessions[0]],
        )
        .await
        .unwrap();
    assert_eq!(
        resets.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1,
        "temporal refresh must revisit the recovered zero-output frontier"
    );
    drop(resets);
    drop(snapshot);
    assert!(
        crate::schema_contract::authority_invariant_triggers_intact(
            &harness.registered.read_connection()
        )
        .await
        .unwrap(),
        "the migration must restore every immutability guard"
    );
    assert_eq!(
        harness
            .registered
            .get_session_message("codex", "message.record.after-parked")
            .await
            .unwrap()
            .unwrap()
            .text,
        CURRENT_TEXT
    );

    drop(store);
    let harness = harness.restart().await;
    let store = harness.registered.observation_store();
    assert!(matches!(
        store
            .project_observation(parked[0].observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
    assert_eq!(
        harness
            .registered
            .get_session_message("codex", "message.record.parked-0")
            .await
            .unwrap()
            .unwrap()
            .text,
        ORIGINAL_TEXT
    );
    for observation in parked.iter().skip(1) {
        assert_eq!(
            parked_projection_state(&harness.registered, observation.observation_id().as_str())
                .await,
            (Some(i64::MAX), Some("output_collision".to_owned()), Some(0)),
        );
    }
    assert_eq!(
        projection_checkpoint(&harness.registered).await,
        checkpoint_before
    );
}

async fn projection_checkpoint(database: &RegisteredGlobalDb) -> i64 {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot.query(
        "SELECT last_sequence FROM observation_projection_checkpoints WHERE projector_version = ?1",
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    ).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn parked_projection_state(
    database: &RegisteredGlobalDb,
    observation_id: &str,
) -> (Option<i64>, Option<String>, Option<i64>) {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot.query(
        "SELECT queue.next_retry_at_micros, disposition.reason, effect.output_count
         FROM observations AS observation
         LEFT JOIN projection_queue AS queue ON queue.observation_id = observation.observation_id
         LEFT JOIN observation_projection_dispositions AS disposition
           ON disposition.observation_id = observation.observation_id AND disposition.projector_version = ?2
         LEFT JOIN session_temporal_observation_effects AS effect
           ON effect.observation_id = observation.observation_id
         WHERE observation.observation_id = ?1",
        params![observation_id, SESSION_MESSAGE_PROJECTOR_VERSION],
    ).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    (
        row.get(0).unwrap(),
        row.get(1).unwrap(),
        row.get(2).unwrap(),
    )
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

async fn queue_attempts(
    database: &RegisteredGlobalDb,
    observation_id: &str,
) -> Option<(i64, Option<String>)> {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT attempt_count, last_error FROM projection_queue WHERE observation_id = ?1",
            params![observation_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap()?;
    Some((row.get(0).unwrap(), row.get(1).unwrap()))
}

/// A Codex session whose later records name another working directory (a
/// rollout resumed from a linked worktree) is one session. Its messages must
/// project and the session row keeps the first cwd instead of colliding on
/// `project_path` for every later record.
#[asupersync::test]
async fn session_seen_from_another_cwd_projects_every_message() {
    let tmp = TempDir::new().unwrap();
    let profile = tmp.path().join("profile");
    let first_cwd = tmp.path().join("repo");
    let second_cwd = tmp.path().join("repo-worktree");
    std::fs::create_dir_all(&first_cwd).unwrap();
    std::fs::create_dir_all(&second_cwd).unwrap();
    #[cfg(unix)]
    let (first_observed, second_observed) = {
        let first_alias = tmp.path().join("first-alias");
        let second_alias = tmp.path().join("second-alias");
        std::os::unix::fs::symlink(&first_cwd, &first_alias).unwrap();
        std::os::unix::fs::symlink(&second_cwd, &second_alias).unwrap();
        (first_alias, second_alias)
    };
    #[cfg(not(unix))]
    let (first_observed, second_observed) = (first_cwd.clone(), second_cwd.clone());
    let runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    let mut ids = Vec::new();
    for (record, text, cwd) in [
        (
            "record.codex.cwd-a",
            "message from the first cwd",
            &first_observed,
        ),
        (
            "record.codex.cwd-b-1",
            "message from the second cwd",
            &second_observed,
        ),
        (
            "record.codex.cwd-b-2",
            "another message from the second cwd",
            &second_observed,
        ),
    ] {
        let write = observation_from_source(
            "codex",
            CODEX_SESSION,
            Some(&format!("rollout.{record}")),
            ObservationScopeV1::Profile,
            record,
            text,
            &format!("receipt.{record}"),
            Some(session_fact(&cwd.to_string_lossy())),
        );
        ids.push(write.observation_id().clone());
        persist(&store, write).await;
    }
    let legacy = observation_from_source(
        "codex",
        CODEX_SESSION,
        Some("rollout.legacy"),
        ObservationScopeV1::Profile,
        "record.codex.legacy",
        "legacy message without directory evidence",
        "receipt.codex.legacy",
        None,
    );
    ids.push(legacy.observation_id().clone());
    persist(&store, legacy).await;
    drain_projection_queue(&store).await;

    let texts = [
        "message from the first cwd",
        "message from the second cwd",
        "another message from the second cwd",
        "legacy message without directory evidence",
    ];
    let expected_path = durable_project_path_key(&first_cwd.to_string_lossy());
    for id in &ids {
        assert_eq!(
            queue_attempts(database, id.as_str()).await,
            None,
            "a projected observation leaves the queue instead of parking on an error"
        );
    }
    for text in texts {
        assert!(
            projected_text_contains(database, "codex", CODEX_SESSION, text).await,
            "{text:?} must project"
        );
    }
    let (_, project_path) = session_project_binding(database, "codex", CODEX_SESSION).await;
    assert_eq!(project_path, expected_path);
    assert_cwd_scoped_messages(database, &first_cwd, &second_cwd).await;
    assert_cwd_scoped_messages(database, &first_observed, &second_observed).await;

    // Protection re-ingest owns the body, while projection retains location.
    let mut message = database
        .search_session_messages("codex", Some(second_cwd.to_str().unwrap()), "message", 1)
        .await
        .unwrap()
        .remove(0)
        .message;
    let mut metadata: Value =
        serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
    metadata["protection_rescan"] = json!("retained provider metadata");
    message.metadata_json = Some(metadata.to_string());
    let storage_root = database.db_path().parent().unwrap();
    let mut rollback =
        tracedecay_lcm::payload::PayloadFileRollback::begin_cancellation_safe(storage_root);
    let transaction = database
        .runtime_database()
        .begin_write_transaction("protect a projected body")
        .await
        .unwrap();
    tracedecay_lcm::raw::upsert_raw_message_with_payload_tracked(
        &transaction,
        storage_root,
        &message,
        &mut rollback,
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    assert_cwd_scoped_messages(database, &first_cwd, &second_cwd).await;

    // The shipped v15 schema lacked the storage-only location. Opening that
    // shape preserves rows and lets the bounded authority audit fill locations.
    crate::schema_contract::ensure_authority_invariants(database.runtime_database(), false, false)
        .await
        .unwrap();
    let immutable_before = message_renderings(database).await;
    let transaction = database
        .runtime_database()
        .begin_write_transaction("restore released raw message shape")
        .await
        .unwrap();
    transaction
        .execute_batch(
            "ALTER TABLE lcm_raw_messages DROP COLUMN location_path;
             UPDATE authority_audit_checkpoints SET audit_version = 2;",
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    drop(store);
    drop(runtime);
    let runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    crate::schema_contract::ensure_authority_invariants(database.runtime_database(), false, false)
        .await
        .unwrap();
    assert_eq!(message_renderings(database).await, immutable_before);
    assert_cwd_scoped_messages(database, &first_cwd, &second_cwd).await;
    assert_cwd_scoped_messages(database, &first_observed, &second_observed).await;

    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    // A prior capture can know an origin a later rendering no longer carries.
    // Rebuild must preserve that materialized identity for the same message.
    let legacy_message = database
        .search_session_messages("codex", Some("user"), "legacy", 1)
        .await
        .unwrap()
        .remove(0)
        .message;
    let transaction = database
        .runtime_database()
        .begin_write_transaction("retain earlier message origin")
        .await
        .unwrap();
    tracedecay_lcm::raw::upsert_projection_raw_message(
        &transaction,
        &legacy_message,
        Some(&expected_path),
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();

    // A rebuild replays the same records through the staging path.
    let frontier = store.projection_checkpoint().await.unwrap().last_sequence();
    let rebuilt = loop {
        let outcome = store.rebuild_projection(frontier).await.unwrap();
        if outcome.is_complete() {
            break outcome;
        }
    };
    assert_eq!(
        rebuilt.skipped_observations(),
        0,
        "no replayed record is skipped"
    );
    for text in texts {
        assert!(
            projected_text_contains(database, "codex", CODEX_SESSION, text).await,
            "{text:?} must survive the rebuild"
        );
    }
    let (_, project_path) = session_project_binding(database, "codex", CODEX_SESSION).await;
    assert_eq!(project_path, expected_path);
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot.query("SELECT location_path FROM lcm_raw_messages WHERE provider = 'codex' AND message_id = ?1", params![legacy_message.message_id.as_str()]).await.unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        expected_path,
        "a rebuild without cwd evidence retains the captured origin"
    );
    drop(rows);
    drop(snapshot);
    assert_cwd_scoped_messages(database, &first_cwd, &second_cwd).await;
    #[cfg(unix)]
    {
        let unrelated = tmp.path().join("unrelated");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::remove_file(&second_observed).unwrap();
        std::os::unix::fs::symlink(&unrelated, &second_observed).unwrap();
        crate::schema_contract::ensure_authority_invariants(
            database.runtime_database(),
            true,
            false,
        )
        .await
        .unwrap();
        while !store
            .rebuild_projection(frontier)
            .await
            .unwrap()
            .is_complete()
        {}
        assert_cwd_scoped_messages(database, &first_cwd, &second_cwd).await;
        assert_eq!(
            database
                .session_message_count_for_project(unrelated.to_str().unwrap())
                .await
                .unwrap(),
            0
        );
        std::fs::remove_file(&second_observed).unwrap();
        crate::schema_contract::ensure_authority_invariants(
            database.runtime_database(),
            true,
            false,
        )
        .await
        .unwrap();
        while !store
            .rebuild_projection(frontier)
            .await
            .unwrap()
            .is_complete()
        {}
        assert_cwd_scoped_messages(database, &first_cwd, &second_cwd).await;
    }
}

async fn assert_cwd_scoped_messages(
    database: &RegisteredGlobalDb,
    first_cwd: &std::path::Path,
    second_cwd: &std::path::Path,
) {
    for (cwd, expected) in [
        (
            first_cwd,
            vec![
                "legacy message without directory evidence",
                "message from the first cwd",
            ],
        ),
        (
            second_cwd,
            vec![
                "another message from the second cwd",
                "message from the second cwd",
            ],
        ),
    ] {
        let selector = cwd.to_str().unwrap();
        assert_eq!(
            database
                .session_message_count_for_project(selector)
                .await
                .unwrap(),
            2,
            "directory-scoped counts use each message's cwd, with the session fallback"
        );
        let mut found = database
            .search_session_messages("codex", Some(selector), "message", 10)
            .await
            .unwrap()
            .into_iter()
            .map(|result| result.message.text)
            .collect::<Vec<_>>();
        found.sort();
        assert_eq!(
            found, expected,
            "directory scopes must not leak other messages"
        );
    }
    assert_eq!(
        database
            .session_message_count_for_project("user")
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        database
            .session_message_count_for_project("/unrelated")
            .await
            .unwrap(),
        0
    );
}

async fn message_renderings(database: &RegisteredGlobalDb) -> Vec<(String, String, String)> {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT m.message_id,
                json_object('content', m.content, 'content_hash', m.content_hash,
                            'storage_kind', m.storage_kind, 'payload_ref', m.payload_ref,
                            'placeholder_text', m.placeholder_text, 'metadata_json', m.metadata_json),
                p.output_digest
         FROM lcm_raw_messages m JOIN observation_projection_provenance p
           ON p.output_provider = m.provider AND p.output_message_id = m.message_id
         ORDER BY m.message_id",
            (),
        )
        .await
        .unwrap();
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        values.push((
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
        ));
    }
    values
}

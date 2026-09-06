use std::collections::BTreeSet;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, AnchorProvenanceRelationV2, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    ComponentVersion, DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId, RepositoryId,
    RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2, RetrievalGrainV1,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_session_memory::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use tracedecay_session_memory::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRequestBinding, SessionRetrievalConfiguration, SessionRetrievalOutcome,
    SessionRetrievalService, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
    SessionTemporalQuery,
};
use tracedecay_session_temporal_store::{
    GlobalDbSessionTemporalStore, RegisteredGlobalDbSessionTemporalExecution,
    SessionTemporalRegisteredDb,
};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::runtime::SessionProvider;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    SessionRetrievalPageV1, SessionRetrievalStore, SessionStoreError,
    SessionTemporalRetrievalRequestV1, SessionTemporalSnapshotRequestV1,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};
use tracedecay_temporal_query::TemporalKernelResult;
use tracedecay_temporal_query::context::{ContextBudget, TokenPolicy, VersionedTokenEstimator};
use tracedecay_temporal_query::ports::ExecutionControl;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::cline_like::{vscode_storage_root, write_task};
use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{
    ingest_global_sources_for_provider, mark_test_project, observation_source_cursor,
    open_project_session_db,
};
use crate::support::{init_git_repo, setup};

const BINDING_DIGEST: [u8; 32] = [0x91; 32];
const LEGACY_QUERY: &str = "orchidlegacyzeta";
const REBUILT_QUERY: &str = "cobaltrebuiltomega";
const ASSERTION_QUERY: &str = "topazassertionchain";
const ASSERTION_PROVIDER: &str = "reset-assertion";
const LEGACY_PARENT: &str = "cline-temporal-reset-parent-legacy";
const REBUILT_PARENT: &str = "cline-temporal-reset-parent-rebuilt";

#[derive(Clone, Copy)]
struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.session-temporal-reset").unwrap(),
            1,
            context,
            binding,
            request,
        )
    }
}

struct Words;

impl VersionedTokenEstimator for Words {
    fn version(&self) -> &str {
        "words-v1"
    }

    fn token_policy(&self) -> TokenPolicy {
        TokenPolicy::Whitespace
    }
}

fn request_context(project_id: &ProjectId) -> (RequestContext, SessionRequestBinding) {
    let actor = ActorId::new("actor.session-temporal-reset").unwrap();
    let request_id = RequestId::new("request.session-temporal-reset").unwrap();
    let identity = ResolvedSessionIdentity::for_project(
        ProfileId::new("profile.session-temporal-reset").unwrap(),
        project_id.clone(),
        SessionStoreId::new(format!("store.{}", project_id.as_str())).unwrap(),
        SessionRootId::new("root.session-temporal-reset").unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.session-temporal-reset").unwrap(),
            WorktreeId::new("worktree.session-temporal-reset").unwrap(),
            BranchId::new("branch.session-temporal-reset").unwrap(),
        ),
    );
    let scope = identity.application_scope().unwrap();
    let capability = CapabilityDigest::new(BINDING_DIGEST);
    let access_policy = tracedecay_store::observation_capture_access_policy_digest_v1().unwrap();
    let policy = PolicyDigest::from_access_policy_digest(&access_policy).unwrap();
    let configuration = ConfigurationDigest::new(BINDING_DIGEST);
    let cancellation = CancellationToken::for_application_request(request_id.as_str());
    let budgets = RequestBudgets::new(64, 64 * 1024 * 1024, 100_000).unwrap();
    let observed_at = application_observed_at();
    let expires_at = UtcMicros(observed_at.0.saturating_add(30_000_000));
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.session-temporal-reset.application").unwrap(),
        1,
        session_application_grant_digest(capability, policy, configuration, &cancellation, budgets)
            .unwrap(),
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.session-temporal-reset").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at).unwrap(),
        CancellationContext::active(cancellation.application_token_id().unwrap()).unwrap(),
    )
    .unwrap();
    let binding = SessionRequestBinding::new(
        identity,
        capability,
        policy,
        configuration,
        cancellation,
        budgets,
    );
    (context, binding)
}

fn query(
    session_id: &SessionId,
    text: &str,
    cursor: Option<String>,
    limit: usize,
) -> SessionTemporalQuery {
    SessionTemporalQuery::new(
        session_id.clone(),
        Some("cline".to_owned()),
        text,
        cursor,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        limit,
        DiversityLimits::unbounded(),
        ContextBudget {
            max_bytes: 64 * 1024,
            max_tokens: 16 * 1024,
            estimator_version: "words-v1".to_owned(),
        },
    )
    .unwrap()
}

async fn retrieve(
    runtime: &HostAdmissionTestRuntimeV1,
    context: &RequestContext,
    binding: &SessionRequestBinding,
    query: SessionTemporalQuery,
) -> SessionRetrievalOutcome<TemporalKernelResult> {
    let database = runtime
        .registered_database(HostAdmissionScope::Project)
        .expect("registered project session database");
    let execution = RegisteredGlobalDbSessionTemporalExecution::new(database);
    SessionRetrievalService::new(
        AllowAuthorizer,
        &execution,
        Words,
        SessionRetrievalConfiguration::new(3, 5).unwrap(),
    )
    .retrieve(context, binding, query)
    .await
}

fn result_from(
    outcome: SessionRetrievalOutcome<TemporalKernelResult>,
    context: &str,
) -> TemporalKernelResult {
    let mut items = match outcome {
        SessionRetrievalOutcome::Complete { items, .. }
        | SessionRetrievalOutcome::Partial { items, .. } => items,
        other => panic!("{context} did not return temporal content: {other:?}"),
    };
    assert_eq!(items.len(), 1, "{context} returned multiple kernel pages");
    items.pop().expect("one temporal kernel page")
}

fn assert_hydrated_content(result: &TemporalKernelResult, expected: &str, rejected: Option<&str>) {
    let available = result
        .hydrated
        .iter()
        .filter_map(|hydrated| hydrated.content())
        .collect::<Vec<_>>();
    assert!(!available.is_empty(), "temporal query hydrated no content");
    for content in available {
        let content = std::str::from_utf8(content).unwrap();
        assert!(
            content.contains(expected),
            "hydrated content did not contain {expected:?}: {content}"
        );
        if let Some(rejected) = rejected {
            assert!(
                !content.contains(rejected),
                "hydrated content retained stale {rejected:?}: {content}"
            );
        }
    }
}

fn history(query: &str) -> Vec<serde_json::Value> {
    ["alpha", "beta", "gamma"]
        .into_iter()
        .enumerate()
        .map(|(ordinal, suffix)| {
            serde_json::json!({
                "role": if ordinal == 1 { "assistant" } else { "user" },
                "content": format!("{query} {suffix}"),
                "ts": 1_800_000_000_i64 + i64::try_from(ordinal).unwrap(),
            })
        })
        .collect()
}

async fn set_parent_session(
    runtime: &HostAdmissionTestRuntimeV1,
    session_id: &str,
    parent_session_id: &str,
) {
    let mut child = runtime
        .project_session_for_test("cline", session_id)
        .await
        .unwrap()
        .expect("projected Cline session");
    let mut parent = child.clone();
    parent.session_id = parent_session_id.to_owned();
    parent.parent_session_id = None;
    parent.is_subagent = false;
    runtime
        .upsert_session_for_test(HostAdmissionScope::Project, &parent)
        .await
        .expect("persist graph parent session");
    child.parent_session_id = Some(parent_session_id.to_owned());
    child.is_subagent = true;
    runtime
        .upsert_session_for_test(HostAdmissionScope::Project, &child)
        .await
        .expect("persist child session graph relation");
}

fn assertion_observation(
    project_id: &ProjectId,
    session_id: &SessionId,
    ordinal: u64,
) -> DurableObservationV1 {
    let provider = ProviderId::new(ASSERTION_PROVIDER).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let record_id =
        ObservationId::new(format!("record.temporal-reset-assertion.{ordinal}")).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id.clone()).with_message_id(
            ObservationId::new(format!("message.temporal-reset-assertion.{ordinal}")).unwrap(),
        ),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: serde_json::json!({"text": format!("{ASSERTION_QUERY} {ordinal}")}),
            model: Some("model.temporal-reset".to_owned()),
            timestamp: Some(1_900_000_000 + i64::try_from(ordinal).unwrap()),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(format!("receipt.temporal-reset-assertion.{ordinal}"))
                    .unwrap(),
                ComponentVersion::new("sanitizer.temporal-reset.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap(),
        RetentionClass::new("retention.temporal-reset").unwrap(),
        payload,
    )
    .unwrap()
}

fn anchored_assertion_observation(
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    previous_anchor: Option<RetrievalAnchorId>,
) -> (AnchoredObservationWrite, RetrievalAnchorId) {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    let valid_at = 1_900_000_000 + i64::try_from(identity.position().start()).unwrap();
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.temporal-reset.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "temporal-reset")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(valid_at),
        authorization,
    )
    .unwrap();
    let mut anchor_json = serde_json::to_value(anchor).unwrap();
    if let Some(previous_anchor) = previous_anchor {
        anchor_json["source_anchors"] = serde_json::json!([{
            "relation": AnchorProvenanceRelationV2::Supersedes,
            "anchor_id": previous_anchor,
            "owner": write.observation().scope(),
        }]);
    }
    anchor_json["occurred_at"] = serde_json::json!({
        "start": valid_at,
        "end": valid_at + 1,
    });
    let anchor: RetrievalAnchorRecordV2 = serde_json::from_value(anchor_json).unwrap();
    let anchor_id = anchor.anchor_id().clone();
    (
        AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap(),
        anchor_id,
    )
}

async fn seed_assertion_supersession(
    runtime: &HostAdmissionTestRuntimeV1,
    project_id: &ProjectId,
    session_id: &SessionId,
) {
    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .expect("registered project observation store");
    let mut previous_anchor = None;
    for ordinal in 0..3 {
        let observation = assertion_observation(project_id, session_id, ordinal);
        let observation_id = observation.observation_id().clone();
        let expected_cursor = store
            .get_source_cursor(observation.source(), observation.scope())
            .await
            .unwrap();
        let (write, anchor_id) =
            anchored_assertion_observation(observation, expected_cursor, previous_anchor);
        store
            .persist_observation(write)
            .await
            .expect("persist assertion-chain observation");
        store
            .project_observation(&observation_id)
            .await
            .expect("project assertion-chain observation");
        previous_anchor = Some(anchor_id);
    }
}

async fn supersession_count(
    database: &tracedecay_global_db::RegisteredGlobalDb,
    session_id: &SessionId,
    generation: u64,
) -> i64 {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*)
             FROM session_assertion_supersession
             WHERE session_id = ?1 AND generation = ?2",
            params![
                session_id.as_str(),
                i64::try_from(generation).expect("fixture generation fits SQLite")
            ],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn temporal_projection_page(
    database: &tracedecay_global_db::RegisteredGlobalDb,
    session_id: &SessionId,
) -> SessionRetrievalPageV1 {
    let store = GlobalDbSessionTemporalStore::new(database);
    let snapshot = store
        .freeze_session_temporal_snapshot(SessionTemporalSnapshotRequestV1::new(session_id.clone()))
        .await
        .expect("freeze active temporal projection");
    store
        .retrieve_session_temporal_page(
            SessionTemporalRetrievalRequestV1::new(
                session_id.clone(),
                TemporalModeV1::Evolution,
                RetrievalGrainV1::Occurrence,
                snapshot,
                100,
                None,
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .expect("query active temporal projection")
}

fn graph_context(
    database: &tracedecay_global_db::RegisteredGlobalDb,
    session_id: &SessionId,
    generation: u64,
) -> tracedecay_session_temporal_store::relations::SessionContextRelations {
    let (scope, graph) = SessionTemporalRegisteredDb::session_relation_store(database)
        .expect("registered session relation graph");
    graph
        .session_context(&scope, session_id, generation, 16, Arc::new(NeverCancelled))
        .expect("query session relation graph")
}

#[cfg(not(windows))]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn reset_reingest_rebuilds_temporal_and_relation_queries_without_stale_coverage() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    let project_id = mark_test_project(&project);
    let session_name = "cline-temporal-reset";
    let session_id = SessionId::new(session_name).unwrap();
    let history_path = write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        session_name,
    );
    let (context, binding) = request_context(&project_id);

    let (database_path, legacy_source_cursor, legacy_generation, legacy_cursor, legacy_anchors) = {
        let db = open_project_session_db(&project).await.unwrap();
        ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cline)).await;
        std::fs::write(
            &history_path,
            serde_json::to_vec_pretty(&history(LEGACY_QUERY)).unwrap(),
        )
        .unwrap();
        ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cline)).await;
        set_parent_session(db.runtime(), session_name, LEGACY_PARENT).await;
        let legacy_source_cursor = observation_source_cursor(&db, "cline", session_name, &project)
            .await
            .expect("legacy observation source cursor");
        let database = db
            .runtime()
            .registered_database(HostAdmissionScope::Project)
            .expect("registered project session database");
        seed_assertion_supersession(db.runtime(), &project_id, &session_id).await;
        GlobalDbSessionTemporalStore::new(database)
            .materialize_pending_session_refresh_for_test(&session_id)
            .await
            .expect("materialize the legacy temporal projection");
        let cursor_page = result_from(
            retrieve(
                db.runtime(),
                &context,
                &binding,
                query(&session_id, LEGACY_QUERY, None, 1),
            )
            .await,
            "legacy temporal query",
        );
        assert_hydrated_content(&cursor_page, LEGACY_QUERY, None);
        let legacy_cursor = cursor_page
            .next_cursor
            .clone()
            .expect("legacy query cursor");
        let full_page = result_from(
            retrieve(
                db.runtime(),
                &context,
                &binding,
                query(&session_id, LEGACY_QUERY, None, 16),
            )
            .await,
            "complete legacy temporal query",
        );
        assert_hydrated_content(&full_page, LEGACY_QUERY, None);
        assert!(full_page.coverage.visible > 0, "legacy temporal coverage");
        let legacy_generation = full_page.snapshot.watermarks().generation;
        assert_eq!(
            cursor_page.snapshot.watermarks().generation,
            legacy_generation
        );
        let assertion_page = temporal_projection_page(database, &session_id).await;
        assert_eq!(
            assertion_page
                .snapshot()
                .watermarks()
                .active_generation()
                .value(),
            legacy_generation
        );
        assert!(
            assertion_page.assertions().len() >= 2,
            "the pre-reset temporal query must expose real assertion records"
        );
        assert!(
            supersession_count(database, &session_id, legacy_generation).await > 0,
            "the pre-reset generation must materialize assertion supersession"
        );
        let legacy_anchors = full_page
            .ranked
            .iter()
            .map(|candidate| candidate.anchor_id.clone())
            .collect::<BTreeSet<_>>();
        let (graph_generation, relations) = database
            .active_session_summary_relations(&session_id, &[], 16, Arc::new(NeverCancelled))
            .await
            .expect("query the legacy relation graph");
        assert_eq!(graph_generation.value(), legacy_generation);
        assert!(relations.is_empty());
        assert_eq!(
            graph_context(database, &session_id, legacy_generation)
                .parent_session_id
                .as_ref()
                .map(SessionId::as_str),
            Some(LEGACY_PARENT)
        );
        (
            database.db_path().to_path_buf(),
            legacy_source_cursor,
            legacy_generation,
            legacy_cursor,
            legacy_anchors,
        )
    };

    {
        let mut raw = rusqlite::Connection::open(&database_path).unwrap();
        assert_eq!(
            raw.execute(
                "DELETE FROM global_schema_migrations WHERE migration = ?1",
                [tracedecay_global_db::observation::OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
            )
            .unwrap(),
            1,
            "fixture must enter the refused pre-reset source scheme"
        );
        let report =
            tracedecay_global_db::observation::reset_refused_observation_authority(&mut raw)
                .expect("canonical observation reset");
        assert!(
            report.cleared_derived_temporal_rows > 0,
            "the journey must reset a materialized temporal projection: {report:?}"
        );
    }

    let reset = open_project_session_db(&project).await.unwrap();
    let reset_database = reset
        .runtime()
        .registered_database(HostAdmissionScope::Project)
        .expect("reset project session database");
    assert!(matches!(
        retrieve(
            reset.runtime(),
            &context,
            &binding,
            query(&session_id, LEGACY_QUERY, Some(legacy_cursor.clone()), 1),
        )
        .await,
        SessionRetrievalOutcome::Unavailable | SessionRetrievalOutcome::CursorStale
    ));
    assert_eq!(
        graph_context(reset_database, &session_id, legacy_generation)
            .parent_session_id
            .as_ref()
            .map(SessionId::as_str),
        Some(LEGACY_PARENT),
        "the raw relation graph must still contain the stale projection or the \
         coverage refusal below is vacuous"
    );
    assert!(matches!(
        reset_database
            .active_session_summary_relations(&session_id, &[], 16, Arc::new(NeverCancelled),)
            .await,
        Err(SessionStoreError::Storage {
            operation: "reconstruct native session relation projection",
            ..
        })
    ));
    assert_eq!(
        supersession_count(reset_database, &session_id, legacy_generation).await,
        0,
        "the reset must delete pre-reset assertion supersession"
    );

    std::fs::write(
        &history_path,
        serde_json::to_vec_pretty(&history(REBUILT_QUERY)).unwrap(),
    )
    .unwrap();
    ingest_global_sources_for_provider(&reset, &project, Some(SessionProvider::Cline)).await;
    set_parent_session(reset.runtime(), session_name, REBUILT_PARENT).await;
    let rebuilt_source_cursor = observation_source_cursor(&reset, "cline", session_name, &project)
        .await
        .expect("rebuilt observation source cursor");
    assert_ne!(
        rebuilt_source_cursor.generation(),
        legacy_source_cursor.generation(),
        "the rebuilt native stream must carry only the replacement source generation"
    );
    GlobalDbSessionTemporalStore::new(reset_database)
        .materialize_pending_session_refresh_for_test(&session_id)
        .await
        .expect("materialize the rebuilt temporal projection");

    let rebuilt_page = result_from(
        retrieve(
            reset.runtime(),
            &context,
            &binding,
            query(&session_id, REBUILT_QUERY, None, 16),
        )
        .await,
        "rebuilt temporal query",
    );
    assert_hydrated_content(&rebuilt_page, REBUILT_QUERY, Some(LEGACY_QUERY));
    let rebuilt_generation = rebuilt_page.snapshot.watermarks().generation;
    assert!(
        rebuilt_generation > legacy_generation,
        "the reset must not reuse pre-reset generation {legacy_generation}; got \
         {rebuilt_generation}"
    );
    assert!(
        rebuilt_page
            .ranked
            .iter()
            .all(|candidate| !legacy_anchors.contains(&candidate.anchor_id)),
        "the rebuilt temporal query served a pre-reset occurrence anchor"
    );
    assert!(
        rebuilt_page.lineage.iter().all(|edge| {
            !legacy_anchors.contains(&edge.subject_anchor_id)
                && !legacy_anchors.contains(&edge.object_anchor_id)
        }),
        "the rebuilt temporal query served a pre-reset assertion"
    );
    assert!(
        rebuilt_page.lineage.is_empty(),
        "the rebuilt observation stream must not retain deleted assertion lineage"
    );
    assert!(
        rebuilt_page.conflicts.iter().all(|conflict| {
            !legacy_anchors.contains(&conflict.anchor_id)
                && conflict.supporting_anchor_ids.is_disjoint(&legacy_anchors)
        }),
        "the rebuilt temporal query served pre-reset supersession evidence"
    );
    assert!(matches!(
        retrieve(
            reset.runtime(),
            &context,
            &binding,
            query(&session_id, LEGACY_QUERY, Some(legacy_cursor.clone()), 1),
        )
        .await,
        SessionRetrievalOutcome::Unavailable | SessionRetrievalOutcome::CursorStale
    ));
    assert!(matches!(
        retrieve(
            reset.runtime(),
            &context,
            &binding,
            query(&session_id, LEGACY_QUERY, None, 16),
        )
        .await,
        SessionRetrievalOutcome::CompleteZero { .. }
    ));
    assert_eq!(
        supersession_count(reset_database, &session_id, rebuilt_generation).await,
        0,
        "the rebuilt generation must not retain pre-reset supersession"
    );
    let rebuilt_projection_page = temporal_projection_page(reset_database, &session_id).await;
    assert!(rebuilt_projection_page.assertions().is_empty());
    assert_eq!(
        rebuilt_projection_page
            .snapshot()
            .watermarks()
            .active_generation()
            .value(),
        rebuilt_generation
    );
    let (rebuilt_graph_generation, rebuilt_relations) = reset_database
        .active_session_summary_relations(&session_id, &[], 16, Arc::new(NeverCancelled))
        .await
        .expect("query the rebuilt relation graph");
    assert_eq!(rebuilt_graph_generation.value(), rebuilt_generation);
    assert!(rebuilt_relations.is_empty());
    assert_eq!(
        graph_context(reset_database, &session_id, rebuilt_generation)
            .parent_session_id
            .as_ref()
            .map(SessionId::as_str),
        Some(REBUILT_PARENT)
    );
    drop(reset);

    let reopened = open_project_session_db(&project).await.unwrap();
    let durable_page = result_from(
        retrieve(
            reopened.runtime(),
            &context,
            &binding,
            query(&session_id, REBUILT_QUERY, None, 16),
        )
        .await,
        "durable rebuilt temporal query",
    );
    assert_eq!(
        durable_page.snapshot.watermarks().generation,
        rebuilt_generation
    );
    assert_hydrated_content(&durable_page, REBUILT_QUERY, Some(LEGACY_QUERY));
    assert!(durable_page.lineage.is_empty());
    let reopened_database = reopened
        .runtime()
        .registered_database(HostAdmissionScope::Project)
        .expect("reopened project session database");
    assert!(matches!(
        retrieve(
            reopened.runtime(),
            &context,
            &binding,
            query(&session_id, LEGACY_QUERY, Some(legacy_cursor), 1),
        )
        .await,
        SessionRetrievalOutcome::Unavailable | SessionRetrievalOutcome::CursorStale
    ));
    let (durable_graph_generation, durable_relations) = reopened_database
        .active_session_summary_relations(&session_id, &[], 16, Arc::new(NeverCancelled))
        .await
        .expect("query the durable rebuilt relation graph");
    assert_eq!(durable_graph_generation.value(), rebuilt_generation);
    assert_eq!(durable_relations, rebuilt_relations);
    assert_eq!(
        supersession_count(reopened_database, &session_id, rebuilt_generation).await,
        0
    );
    let durable_projection_page = temporal_projection_page(reopened_database, &session_id).await;
    assert!(durable_projection_page.assertions().is_empty());
    assert_eq!(
        durable_projection_page
            .snapshot()
            .watermarks()
            .active_generation()
            .value(),
        rebuilt_generation
    );
    assert_eq!(
        graph_context(reopened_database, &session_id, rebuilt_generation)
            .parent_session_id
            .as_ref()
            .map(SessionId::as_str),
        Some(REBUILT_PARENT)
    );
}

#![allow(dead_code, unused_imports)]

pub(crate) use std::fs;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::Command;
pub(crate) use std::sync::Arc;
pub(crate) use std::thread;

pub(crate) use crate::common::{
    EnvVarGuard, GLOBAL_DB_ENV, GLOBAL_DB_ENV_LOCK, MessageRecordBuilder, create_runtime,
    fake_codex_bin, get_json, http_agent, http_agent_with_timeout, install_fake_codex_launcher,
    pick_free_port, response_to_json, tempdir_or_panic, wait_for_dashboard,
};
pub(crate) use crate::runtime::DashboardTestRuntimeV1;
pub(crate) use serde_json::Value;
pub(crate) use tempfile::TempDir;
pub(crate) use tracedecay::config::USER_DATA_DIR_ENV;
pub(crate) use tracedecay::dashboard;
pub(crate) use tracedecay::tracedecay::TraceDecay;
pub(crate) use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1, FactEventId, FactId, ProjectId,
};
pub(crate) use tracedecay_sessions::runtime::lcm::{LcmSourceRef, LcmSummaryNodeDraft};
pub(crate) use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
pub(crate) use tracedecay_usecases::host_admission::HostAdmissionScope;

pub(crate) fn test_fact_write_control() -> tracedecay_store::FactWriteControl {
    use std::sync::atomic::{AtomicBool, Ordering};

    let interrupted = Arc::new(AtomicBool::new(false));
    let commit_started = Arc::new(AtomicBool::new(false));
    tracedecay_store::FactWriteControl::new(
        {
            let interrupted = Arc::clone(&interrupted);
            Arc::new(move || interrupted.load(Ordering::Acquire))
        },
        Arc::new(move || {
            commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        }),
    )
}

pub(crate) struct MessageDetails<'a> {
    pub(crate) timestamp: i64,
    pub(crate) model: Option<&'a str>,
    pub(crate) metadata_json: Option<&'a str>,
}

pub(crate) fn message(
    message_id: &str,
    session_id: &str,
    role: &str,
    ordinal: i64,
    text: &str,
    details: MessageDetails<'_>,
) -> SessionMessageRecord {
    MessageRecordBuilder::new(
        "cursor", message_id, session_id, role, ordinal, text, "message",
    )
    .with_timestamp(Some(details.timestamp))
    .with_model(details.model)
    .with_metadata(details.metadata_json)
    .build()
}

/// Longer than 200 chars on purpose: list/projection payloads truncate
/// `content` at 200, so this fact proves the `/fact/{id}` detail endpoint
/// returns the full text.
pub(crate) const LONG_FACT_CONTENT: &str = "LCM dashboard empty states need explicit copy. \
The drawer, search results, charts, and overview panels must each explain why \
they are empty and what action will populate them, because first-run users \
otherwise assume the integration is broken when the store simply has no rows yet.";

pub(crate) struct DashboardFixture {
    pub(crate) _tmp: TempDir,
    pub(crate) _env_guard: EnvVarGuard,
    pub(crate) _data_dir_guard: EnvVarGuard,
    pub(crate) _home_guard: EnvVarGuard,
    pub(crate) _userprofile_guard: EnvVarGuard,
    pub(crate) home: std::path::PathBuf,
    pub(crate) global_db_path: std::path::PathBuf,
    pub(crate) base_url: String,
    pub(crate) project_root: std::path::PathBuf,
    pub(crate) host_runtime: Arc<DashboardTestRuntimeV1>,
    pub(crate) project_graphs: dashboard::DashboardTestProjectGraphsV1,
    pub(crate) server: DashboardServer,
}

impl Drop for DashboardFixture {
    fn drop(&mut self) {
        self.server.stop();
    }
}

pub(crate) struct DashboardServer {
    pub(crate) shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    pub(crate) thread: Option<thread::JoinHandle<()>>,
}

impl DashboardServer {
    pub(crate) fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DashboardServer {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn spawn_dashboard_server(cg: TraceDecay, port: u16) -> DashboardServer {
    spawn_dashboard_server_with_runner(cg, None, false, port)
}

pub(crate) fn spawn_dashboard_server_lightweight(cg: TraceDecay, port: u16) -> DashboardServer {
    spawn_dashboard_server_with_runner(cg, None, false, port)
}

pub(crate) fn spawn_dashboard_server_with_host_runtime(
    cg: TraceDecay,
    host_runtime: Arc<DashboardTestRuntimeV1>,
    project_graphs: dashboard::DashboardTestProjectGraphsV1,
    port: u16,
) -> DashboardServer {
    spawn_dashboard_server_with_runner(cg, Some((host_runtime, project_graphs)), false, port)
}

pub(crate) fn spawn_dashboard_server_with_configuration_runtime(
    cg: TraceDecay,
    host_runtime: Arc<DashboardTestRuntimeV1>,
    project_graphs: dashboard::DashboardTestProjectGraphsV1,
    port: u16,
) -> DashboardServer {
    spawn_dashboard_server_with_runner(cg, Some((host_runtime, project_graphs)), true, port)
}

fn spawn_dashboard_server_with_runner(
    cg: TraceDecay,
    host_authority: Option<(
        Arc<DashboardTestRuntimeV1>,
        dashboard::DashboardTestProjectGraphsV1,
    )>,
    mount_configuration_runtime: bool,
    port: u16,
) -> DashboardServer {
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let thread = thread::spawn(move || {
        let runtime = create_runtime();
        runtime.block_on(async move {
            let cg = Arc::new(cg);
            let (host_runtime, project_graphs) = match host_authority {
                Some(authority) => authority,
                None => (
                    open_dashboard_host_runtime(&cg).await,
                    dashboard::DashboardTestProjectGraphsV1::default(),
                ),
            };
            let authority = if mount_configuration_runtime {
                host_runtime
                    .dashboard_test_authority_with_configuration(&cg)
                    .await
            } else {
                host_runtime
                    .dashboard_test_authority_with_session_reads(&cg)
                    .await
            }
            .expect("dashboard test authority");
            let result = dashboard::run_until_shutdown_for_tests_with_host_admission(
                cg.clone(),
                authority,
                project_graphs,
                "127.0.0.1",
                port,
                dashboard::spa_router(),
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await;
            let _ = cg.checkpoint().await;
            let _ = result;
        });
    });
    DashboardServer {
        shutdown: Some(shutdown),
        thread: Some(thread),
    }
}

pub(crate) fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        panic!("failed to create {}: {err}", parent.display());
    }
    if let Err(err) = fs::write(path, content) {
        panic!("failed to write {}: {err}", path.display());
    }
}

pub(crate) async fn setup_project(
    project_root: &Path,
) -> (TraceDecay, Arc<DashboardTestRuntimeV1>) {
    write_file(
        &project_root.join("src/lib.rs"),
        "pub fn seed_fixture() -> &'static str { \"dashboard\" }\n",
    );
    let project_id = tracedecay::storage::read_repository_identity_marker(project_root)
        .unwrap_or_else(|error| panic!("read dashboard fixture identity: {error}"))
        .and_then(|marker| ProjectId::new(marker.project_id).ok())
        .unwrap_or_else(|| {
            let suffix = project_root
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("project")
                .replace(|character: char| !character.is_ascii_alphanumeric(), "_");
            ProjectId::new(format!("dashboard_fixture_{suffix}"))
                .unwrap_or_else(|error| panic!("mint dashboard fixture identity: {error}"))
        });
    let profile_root = tracedecay::storage::default_profile_root()
        .unwrap_or_else(|error| panic!("resolve dashboard fixture profile root: {error}"));
    let open_options = tracedecay::tracedecay::TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: None,
    };
    let runtime = Arc::new(
        DashboardTestRuntimeV1::project(&profile_root, project_root, project_id)
            .await
            .unwrap_or_else(|error| panic!("open dashboard fixture authority: {error}")),
    );
    let graph = runtime
        .initialize_project_graph_for_test(project_root, open_options)
        .await
        .unwrap_or_else(|error| panic!("initialize dashboard fixture graph: {error}"));
    (graph, runtime)
}

pub(crate) async fn open_dashboard_host_runtime(cg: &TraceDecay) -> Arc<DashboardTestRuntimeV1> {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .and_then(|project_id| ProjectId::new(project_id.to_owned()).ok())
        .unwrap_or_else(|| panic!("dashboard fixture requires an authoritative project id"));
    let project_id_text = project_id.as_str().to_owned();
    let runtime = Arc::new(
        DashboardTestRuntimeV1::project(
            tracedecay::storage::default_profile_root()
                .unwrap_or_else(|error| panic!("resolve dashboard test profile root: {error}")),
            cg.project_root(),
            project_id,
        )
        .await
        .unwrap_or_else(|error| panic!("open dashboard host-admission runtime: {error}")),
    );
    runtime
        .upsert_code_project(&project_id_text, cg.project_root(), None, None, None)
        .await
        .unwrap_or_else(|error| panic!("register dashboard fixture project: {error}"));
    runtime
}

pub(crate) struct DashboardAutomaticFactReceipt {
    pub(crate) apply_id: String,
    pub(crate) fact_id: String,
}

fn dashboard_fixture_project_owner(cg: &TraceDecay) -> tracedecay_domain::FactOwnerV1 {
    let raw_project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .unwrap_or_else(|| panic!("dashboard fixture requires an authoritative project id"));
    let project_id = tracedecay_domain::ProjectId::new(raw_project_id.to_owned())
        .unwrap_or_else(|error| panic!("invalid dashboard fixture project id: {error}"));
    tracedecay_domain::FactOwnerV1::Project { project_id }
}

/// Records an automatic fact effect through the canonical receipt authority.
/// No sidecar JSON participates in this fixture.
pub(crate) async fn record_dashboard_automatic_fact(
    cg: &TraceDecay,
    run_id: &str,
    content: &str,
) -> DashboardAutomaticFactReceipt {
    use tracedecay::store::memory::DatabaseFactStore;
    use tracedecay_agent_hosts::automation::AutomationRunControl;
    use tracedecay_agent_hosts::automation::automatic_facts::{
        AutomaticFactState, record_session_automatic_facts,
    };
    use tracedecay_usecases::memory::MemoryApplication;

    let owner = dashboard_fixture_project_owner(cg);
    let memory = MemoryApplication::new(owner, DatabaseFactStore::new(cg.db()))
        .unwrap_or_else(|error| panic!("initialize outcome memory application: {error}"));
    let request = tracedecay_usecases::memory::ProjectMemoryFactAddRequest {
        content: content.to_string(),
        category: FactCategoryV1::Project,
        source_label: Some("dashboard-outcome-test".to_string()),
        tags: vec!["automation".to_string(), "outcome".to_string()],
        entities: vec!["TraceDecay".to_string()],
        trust: Some(
            Confidence::new(0.9)
                .unwrap_or_else(|error| panic!("build automatic fact trust: {error}")),
        ),
        metadata: serde_json::json!({"origin": "dashboard-outcome-test"}),
    };
    let run_control = AutomationRunControl::from_interrupted(Arc::new(|| false));
    let batch = record_session_automatic_facts(
        &memory,
        &run_control,
        run_id,
        Some("dashboard-outcome-test"),
        &[serde_json::json!({
            "add_fact_request": request,
            "validation": { "fixture": "dashboard-outcome-test" },
        })],
    )
    .await
    .unwrap_or_else(|error| panic!("record outcome automatic fact receipt: {error}"));
    assert!(
        batch.retry_error.is_none(),
        "outcome automatic fact receipt must not require retry: {:?}",
        batch.retry_error
    );
    let receipt = batch
        .receipts
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("outcome automatic fact application must return a receipt"));
    assert_eq!(receipt.state, AutomaticFactState::Applied);
    DashboardAutomaticFactReceipt {
        apply_id: receipt.apply_id,
        fact_id: receipt.applied_fact_id.unwrap_or_else(|| {
            panic!("applied outcome automatic fact receipt needs canonical fact identity")
        }),
    }
}

pub(crate) async fn delete_dashboard_automatic_fact(
    cg: &TraceDecay,
    receipt: &DashboardAutomaticFactReceipt,
) {
    use tracedecay::store::memory::DatabaseFactStore;
    use tracedecay_usecases::memory::{MemoryApplication, MemoryOperationContext};

    let owner = dashboard_fixture_project_owner(cg);
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db()))
        .unwrap_or_else(|error| panic!("initialize outcome memory application: {error}"));
    let context = MemoryOperationContext::from_request_id(
        &owner,
        "dashboard-outcome-test-delete",
        &receipt.apply_id,
        None,
    )
    .unwrap_or_else(|error| panic!("derive outcome deletion identity: {error}"));
    let canonical_fact_id = FactId::new(receipt.fact_id.clone())
        .unwrap_or_else(|error| panic!("parse outcome canonical fact identity: {error}"));
    assert!(
        memory
            .remove_canonical_fact(canonical_fact_id, context, &test_fact_write_control(),)
            .await
            .unwrap_or_else(|error| panic!("delete outcome fact: {error:?}"))
            .was_removed()
    );
}

pub(crate) struct DashboardMemoryFixture {
    pub(crate) near_duplicate_fact_id: FactId,
    pub(crate) near_duplicate_last_event_id: FactEventId,
}

pub(crate) async fn seed_dashboard_fact(
    cg: &TraceDecay,
    content: &str,
    category: FactCategoryV1,
    trust: f64,
    tags: &[&str],
    entities: &[&str],
) -> FactId {
    use tracedecay::store::memory::DatabaseFactStore;
    use tracedecay_store::ProjectMemoryFactProjectionV1;
    use tracedecay_usecases::memory::{
        MemoryApplication, ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome,
    };

    let owner = dashboard_fixture_project_owner(cg);
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db()))
        .unwrap_or_else(|error| panic!("initialize dashboard memory fixture: {error}"));
    let request = ProjectMemoryFactAddRequest {
        content: content.to_owned(),
        category,
        source_label: Some("dashboard-fixture".to_owned()),
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        entities: entities.iter().map(|entity| (*entity).to_owned()).collect(),
        trust: Some(
            Confidence::new(trust)
                .unwrap_or_else(|error| panic!("dashboard fixture trust: {error}")),
        ),
        metadata: serde_json::json!({}),
    };
    let actor = ActorId::new("actor.dashboard-fixture".to_owned())
        .unwrap_or_else(|error| panic!("build dashboard fixture actor: {error}"));
    let preflight = memory
        .preflight_project_memory_fact_add(request, Some(actor))
        .unwrap_or_else(|error| panic!("preflight dashboard fact: {error}"));
    let write_control = test_fact_write_control();
    let outcome = memory
        .add_preflighted_project_memory_fact(preflight, &write_control)
        .await
        .unwrap_or_else(|error| panic!("seed dashboard fact: {error}"));
    let ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome else {
        panic!("dashboard fixture fact was rejected by the privacy authority: {content}");
    };
    let ProjectMemoryFactProjectionV1::Available(fact) = outcome.fact() else {
        panic!("dashboard fixture fact payload is unavailable: {content}");
    };
    assert_eq!(fact.content(), content);
    fact.fact_id().clone()
}

pub(crate) async fn seed_memory_fixture(cg: &TraceDecay) -> DashboardMemoryFixture {
    // Seed through the canonical application preflight and write authorities
    // so fixture facts use the same sanitization and commit path as production.
    let fixtures = [
        (
            "Cache invalidation policy must be explicit",
            FactCategoryV1::Project,
            0.97,
            vec!["cache", "policy"],
            vec!["CachePolicy"],
        ),
        (
            "Cache invalidation policy must stay explicit",
            FactCategoryV1::Project,
            0.95,
            vec!["cache", "policy"],
            vec!["CachePolicy"],
        ),
        (
            LONG_FACT_CONTENT,
            FactCategoryV1::Tool,
            0.71,
            vec!["lcm", "ux"],
            vec!["LCMTab", "SimilarityView"],
        ),
    ];
    let mut fact_ids = Vec::with_capacity(fixtures.len());
    for (content, category, trust, tags, entities) in fixtures {
        fact_ids.push(seed_dashboard_fact(cg, content, category, trust, &tags, &entities).await);
    }
    let tool_fact_id = fact_ids[2].clone();
    for (action, note) in [
        (
            tracedecay_store::ProjectMemoryFactFeedbackActionV1::Helpful,
            Some("confirmed durable".to_string()),
        ),
        (
            tracedecay_store::ProjectMemoryFactFeedbackActionV1::Unhelpful,
            None,
        ),
    ] {
        use tracedecay::store::memory::DatabaseFactStore;
        use tracedecay_store::{ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactIdV1};
        use tracedecay_usecases::memory::{MemoryApplication, MemoryOperationContext};

        let owner = dashboard_fixture_project_owner(cg);
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db()))
            .unwrap_or_else(|error| panic!("initialize dashboard feedback fixture: {error}"));
        let context = MemoryOperationContext::from_logical_effect(
            &owner,
            "dashboard-fixture-feedback",
            &serde_json::json!({
                "fact_id": tool_fact_id.as_str(),
                "action": format!("{action:?}"),
            }),
            None,
        )
        .unwrap_or_else(|error| panic!("derive dashboard feedback identity: {error}"));
        memory
            .record_project_memory_fact_feedback(
                ProjectMemoryFactFeedbackCommandV1::new(
                    ProjectMemoryFactIdV1::new(owner, tool_fact_id.clone())
                        .unwrap_or_else(|error| panic!("dashboard feedback target: {error}")),
                    context.operation_id().clone(),
                    None,
                    action,
                    None,
                    Some("dashboard-test".to_owned()),
                    note,
                )
                .unwrap_or_else(|error| panic!("dashboard feedback command: {error}")),
                &test_fact_write_control(),
            )
            .await
            .unwrap_or_else(|error| panic!("seed dashboard feedback: {error:?}"));
    }
    use tracedecay::store::memory::DatabaseFactStore;
    use tracedecay_store::{FactReadControl, ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1};
    use tracedecay_usecases::memory::MemoryApplication;
    let near_duplicate_fact_id = fact_ids[1].clone();
    let owner = dashboard_fixture_project_owner(cg);
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db()))
        .unwrap_or_else(|error| panic!("initialize dashboard memory fixture: {error}"));
    let projection = memory
        .get_project_memory_fact(
            ProjectMemoryFactIdV1::new(owner, near_duplicate_fact_id.clone())
                .unwrap_or_else(|error| panic!("dashboard fixture target: {error}")),
            &FactReadControl::new(Arc::new(|| false)),
        )
        .await
        .unwrap_or_else(|error| panic!("read dashboard fixture target: {error}"))
        .unwrap_or_else(|| panic!("dashboard fixture target disappeared"));
    let ProjectMemoryFactProjectionV1::Available(projection) = projection else {
        panic!("dashboard fixture target payload unavailable");
    };
    DashboardMemoryFixture {
        near_duplicate_fact_id,
        near_duplicate_last_event_id: projection.last_event_id().clone(),
    }
}

/// Resolve a seeded fact through the served dashboard instead of coupling
/// tests to legacy numeric IDs or storage rows.
pub(crate) fn fixture_fact_id(
    agent: &ureq::Agent,
    fixture: &DashboardFixture,
    content_prefix: &str,
) -> FactId {
    let (status, overview) = get_json(
        agent,
        &format!("{}/api/plugins/holographic/?limit=100", fixture.base_url),
    );
    assert_eq!(status, 200, "dashboard fixture overview must succeed");
    overview["payload"]["holographic"]["facts"]
        .as_array()
        .and_then(|facts| {
            facts.iter().find_map(|fact| {
                fact.get("content")
                    .and_then(Value::as_str)
                    .filter(|content| content.starts_with(content_prefix))
                    .and_then(|_| fact.get("fact_id").and_then(Value::as_str))
                    .and_then(|fact_id| FactId::new(fact_id.to_owned()).ok())
            })
        })
        .unwrap_or_else(|| panic!("seeded dashboard fact not found for prefix: {content_prefix}"))
}

pub(crate) async fn seed_lcm_fixture(runtime: &DashboardTestRuntimeV1, project_path: &Path) {
    let session = SessionRecord {
        provider: "cursor".to_string(),
        session_id: "sess-dashboard-1".to_string(),
        // Production registers project sessions under the registered project
        // identity; the canonical observation projection reconciles its
        // session row against this one, so a divergent ad-hoc key would be
        // an output collision, not a merge.
        project_key: runtime.project_id().as_str().to_string(),
        project_path: project_path.display().to_string(),
        title: Some("Dashboard fixture session".to_string()),
        started_at: Some(1_700_001_000),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    if !runtime
        .upsert_session_for_test(HostAdmissionScope::Project, &session)
        .await
        .unwrap_or_else(|error| panic!("failed to upsert session fixture: {error}"))
    {
        panic!("failed to upsert session fixture");
    }

    let messages = [
        SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "msg-1".to_string(),
            session_id: "sess-dashboard-1".to_string(),
            role: "user".to_string(),
            timestamp: Some(1_700_001_010),
            ordinal: 1,
            text: "Need a vector projection for memory similarity.".to_string(),
            kind: Some("chat".to_string()),
            model: Some("gpt".to_string()),
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        },
        SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "msg-2".to_string(),
            session_id: "sess-dashboard-1".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(1_700_001_020),
            ordinal: 2,
            text: "Similarity pair detected for cache policy facts.".to_string(),
            kind: Some("chat".to_string()),
            model: Some("gpt".to_string()),
            tool_names: Some("tracedecay_search".to_string()),
            source_path: None,
            source_offset: None,
            metadata_json: None,
        },
        SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "msg-3".to_string(),
            session_id: "sess-dashboard-1".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(1_700_001_030),
            ordinal: 3,
            text: "LCM tab should render non-empty overview cards.".to_string(),
            kind: Some("chat".to_string()),
            model: Some("gpt".to_string()),
            tool_names: Some("tracedecay_lcm_status".to_string()),
            source_path: None,
            source_offset: None,
            metadata_json: None,
        },
    ];

    for message in messages {
        // Production ingest persists every message as a canonical durable
        // observation (which projects the session_messages row itself) plus
        // the raw LCM payload row; the session-temporal refresh discovers
        // sessions ONLY from the observation effects, so the fixture walks
        // the same two writes instead of raw session_messages upserts the
        // temporal projection would never see.
        runtime
            .lcm_ingest_raw_message_for_test(HostAdmissionScope::Project, &message)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "failed to ingest raw LCM fixture message {}: {error}",
                    message.message_id
                )
            });
        runtime
            .seed_session_message_observation_for_test(
                tracedecay::dashboard::observation_seed::DashboardSessionMessageSeedV1 {
                    project_id: runtime.project_id().as_str(),
                    provider: &message.provider,
                    session_id: &message.session_id,
                    message_id: &message.message_id,
                    role: &message.role,
                    content: &message.text,
                    model: message.model.as_deref(),
                    timestamp: message.timestamp.unwrap_or_else(|| {
                        panic!("fixture message {} has no timestamp", message.message_id)
                    }),
                    ordinal: u64::try_from(message.ordinal).unwrap_or_else(|_| {
                        panic!(
                            "fixture message {} has a negative ordinal",
                            message.message_id
                        )
                    }),
                },
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "failed to seed canonical observation for {}: {error}",
                    message.message_id
                )
            });
    }
    let msg_1 = match runtime
        .lcm_raw_store_id_for_test(HostAdmissionScope::Project, "cursor", "msg-1")
        .await
        .unwrap_or_else(|error| panic!("load seeded message msg-1: {error}"))
    {
        Some(store_id) => store_id,
        None => panic!("missing seeded message msg-1"),
    };
    let msg_2 = match runtime
        .lcm_raw_store_id_for_test(HostAdmissionScope::Project, "cursor", "msg-2")
        .await
        .unwrap_or_else(|error| panic!("load seeded message msg-2: {error}"))
    {
        Some(store_id) => store_id,
        None => panic!("missing seeded message msg-2"),
    };

    let draft = LcmSummaryNodeDraft {
        provider: "cursor".to_string(),
        conversation_id: "conv-dashboard".to_string(),
        session_id: "sess-dashboard-1".to_string(),
        depth: 1,
        summary_text: "Vector projection summary for cache policy similarities.".to_string(),
        source_refs: vec![
            LcmSourceRef::RawMessage { store_id: msg_1 },
            LcmSourceRef::RawMessage { store_id: msg_2 },
        ],
        source_token_count: 180,
        summary_token_count: 72,
        source_time_start: Some(1_700_001_010),
        source_time_end: Some(1_700_001_030),
        expand_hint: Some("Use summary detail drawer".to_string()),
        metadata_json: Some(
            "{\"category\":\"analysis\",\"tags\":[\"vector\"],\"entities\":[\"cache\"]}"
                .to_string(),
        ),
    };
    if let Err(err) = runtime
        .lcm_insert_summary_node_for_test(HostAdmissionScope::Project, draft)
        .await
    {
        panic!("failed to insert summary node fixture: {err}");
    }
    // Materialize AFTER every seeded write (messages and the summary node):
    // the frozen temporal generation serves reads, so anything published
    // after materialization would be invisible to generation-bound reads.
    runtime
        .materialize_session_temporal_refresh_for_test("sess-dashboard-1")
        .await
        .unwrap_or_else(|error| {
            panic!("failed to materialize LCM fixture session refresh: {error}")
        });
}

pub(crate) fn post_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = crate::common::http_call_with_retry(&format!("POST {url}"), || {
        agent.post(url).send_empty()
    });
    response_to_json(response)
}

pub(crate) fn post_json_body(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let response = crate::common::http_call_with_retry(&format!("POST {url} (with body)"), || {
        agent.post(url).send_json(body)
    });
    response_to_json(response)
}

pub(crate) fn patch_json_body(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let response = crate::common::http_call_with_retry(&format!("PATCH {url} (with body)"), || {
        agent.patch(url).send_json(body)
    });
    response_to_json(response)
}

pub(crate) fn delete_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response =
        crate::common::http_call_with_retry(&format!("DELETE {url}"), || agent.delete(url).call());
    response_to_json(response)
}

pub(crate) struct FakeCodexAppServer {
    pub(crate) _temp: TempDir,
    pub(crate) bin: PathBuf,
}

impl FakeCodexAppServer {
    pub(crate) fn new_memory_curator(fact_id: FactId, last_event_id: FactEventId) -> Self {
        let temp = tempdir_or_panic();
        let script_path = temp.path().join("codex.py");
        let bin = fake_codex_bin(temp.path());
        let script = r#"#!/usr/bin/env python3
import json
import os
import sys

if len(sys.argv) != 2 or sys.argv[1] != "app-server":
    sys.exit(42)
if os.environ.get("TRACEDECAY_CODEX_SUMMARY_CHILD") != "1":
    sys.exit(43)
fact_id = __FACT_ID__
last_event_id = __LAST_EVENT_ID__

for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        print(json.dumps({"id": msg.get("id"), "result": {}}), flush=True)
    elif method == "thread/start":
        print(json.dumps({
            "id": msg.get("id"),
            "result": {"thread": {"id": "thread-dashboard", "model": "dashboard-fake-model"}}
        }), flush=True)
    elif method == "turn/start":
        payload = {
            "ops": [{
                "op": "normalize_tags",
                "target": {
                    "fact_id": fact_id,
                    "expected_last_event_id": last_event_id,
                },
                "tags": ["cache", "curated", "policy"],
                "evidence_facts": [{
                    "fact_id": fact_id,
                    "expected_last_event_id": last_event_id,
                }],
                "confidence": 0.98
            }]
        }
        print(json.dumps({
            "method": "item/agentMessage/delta",
            "params": {"delta": json.dumps(payload), "model": "dashboard-fake-model"}
        }), flush=True)
        print(json.dumps({"method": "turn/completed"}), flush=True)
        break
"#
        .replace(
            "__FACT_ID__",
            &serde_json::to_string(fact_id.as_str())
                .unwrap_or_else(|error| panic!("encode fake curator fact id: {error}")),
        )
        .replace(
            "__LAST_EVENT_ID__",
            &serde_json::to_string(last_event_id.as_str())
                .unwrap_or_else(|error| panic!("encode fake curator event id: {error}")),
        );
        write_file(&script_path, &script);
        install_fake_codex_launcher(&script_path, &bin);
        Self { _temp: temp, bin }
    }
}

pub(crate) async fn start_dashboard_fixture(seed_lcm: bool) -> DashboardFixture {
    start_dashboard_fixture_with_options(seed_lcm, true, false).await
}

pub(crate) async fn start_dashboard_fixture_without_memory() -> DashboardFixture {
    start_dashboard_fixture_with_options(false, false, false).await
}

pub(crate) async fn start_dashboard_configuration_fixture() -> DashboardFixture {
    start_dashboard_fixture_with_options(false, false, true).await
}

pub(crate) async fn start_dashboard_retained_memory_fixture() -> DashboardFixture {
    start_dashboard_fixture_with_options(false, true, true).await
}

async fn start_dashboard_fixture_with_options(
    seed_lcm: bool,
    seed_memory: bool,
    mount_configuration_runtime: bool,
) -> DashboardFixture {
    let tmp = tempdir_or_panic();
    let tmp_root = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
    let project_root = tmp_root.join("project");
    let profile_root = tmp_root.join("profile").join(".tracedecay");
    let requested_global_db_path = profile_root.join("global.db");
    // Skill lifecycle endpoints re-export managed skills into agent configs
    // under the process home; point HOME at the fixture so tests never touch
    // the developer's real agent installations.
    let home = tmp_root.join("home");
    std::fs::create_dir_all(&home)
        .unwrap_or_else(|err| panic!("failed to create fixture home: {err}"));
    let env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &requested_global_db_path);
    let data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
    let home_guard = EnvVarGuard::set("HOME", &home);
    let userprofile_guard = EnvVarGuard::set("USERPROFILE", &home);
    std::fs::create_dir_all(&project_root)
        .unwrap_or_else(|err| panic!("failed to create fixture project root: {err}"));
    if let Err(err) = tracedecay::storage::pin_fixture_repository_identity(
        &project_root,
        "dashboard_fixture_project",
    ) {
        panic!("failed to enroll dashboard fixture in profile storage: {err}");
    }

    // Root composition retains the exact graph and registered database
    // authorities for the server lifetime.
    let (cg, host_runtime) = setup_project(&project_root).await;
    let global_db_path = host_runtime
        .database_path(HostAdmissionScope::Profile)
        .expect("dashboard fixture profile database path")
        .to_path_buf();
    if seed_memory {
        seed_memory_fixture(&cg).await;
    }

    let project_graphs = dashboard::DashboardTestProjectGraphsV1::default();
    if seed_lcm {
        seed_lcm_fixture(&host_runtime, &project_root).await;
    }
    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server = spawn_dashboard_server_with_runner(
        cg,
        Some((Arc::clone(&host_runtime), project_graphs.clone())),
        mount_configuration_runtime,
        port,
    );

    let agent = http_agent();
    wait_for_dashboard(&agent, &base_url).await;

    DashboardFixture {
        _tmp: tmp,
        _env_guard: env_guard,
        _data_dir_guard: data_dir_guard,
        _home_guard: home_guard,
        _userprofile_guard: userprofile_guard,
        home,
        global_db_path,
        base_url,
        project_root,
        host_runtime,
        project_graphs,
        server,
    }
}

pub(crate) fn git(project: &Path, args: &[&str]) {
    assert!(
        project.is_dir(),
        "git cwd {project:?} should exist before running git {args:?}"
    );
    let git = crate::common::git_program();
    // Retry a transient spawn ENOENT: under heavy parallel test load the
    // initial fork/exec can spuriously fail with "No such file or directory".
    let mut last_err: Option<std::io::Error> = None;
    let mut output = None;
    for attempt in 0..5 {
        match Command::new(&git).args(args).current_dir(project).output() {
            Ok(out) => {
                output = Some(out);
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && attempt < 4 => {
                last_err = Some(err);
                thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
            Err(err) => panic!("failed to run git {args:?} (program {git:?}): {err}"),
        }
    }
    let output = output.unwrap_or_else(|| {
        panic!("failed to run git {args:?} (program {git:?}) after retries: {last_err:?}")
    });
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(crate) fn commit_all(project: &Path, message: &str) {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "commit",
            "-m",
            message,
        ],
    );
}

/// Opens the resolved registered project session authority.
pub(crate) async fn open_project_session_store(project_root: &Path) -> Arc<DashboardTestRuntimeV1> {
    let project_id = tracedecay::storage::read_repository_identity_marker(project_root)
        .unwrap_or_else(|error| panic!("read dashboard project identity: {error}"))
        .and_then(|marker| ProjectId::new(marker.project_id).ok())
        .unwrap_or_else(|| panic!("dashboard fixture requires an authoritative project identity"));
    Arc::new(
        DashboardTestRuntimeV1::project(
            tracedecay::storage::default_profile_root()
                .unwrap_or_else(|error| panic!("resolve dashboard test profile root: {error}")),
            project_root,
            project_id,
        )
        .await
        .unwrap_or_else(|error| panic!("open dashboard project session authority: {error}")),
    )
}

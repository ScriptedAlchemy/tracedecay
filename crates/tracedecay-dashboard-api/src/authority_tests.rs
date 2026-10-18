//! Behavioral coverage of dashboard admission, routes, and retained authority.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Json;
use axum::routing::{get, post};
use serde_json::{Value, json};
use tower::ServiceExt;
use tracedecay_api::{WorkOperation, WorkflowOperation};
use tracedecay_domain::{Confidence, FactCategoryV1, ProjectId, ProvenanceId};
use tracedecay_privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_session_memory::fact_store::DatabaseFactStore;
use tracedecay_store::{
    FactWriteControl, ProjectMemoryFactAddDispositionV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactStore,
};

use crate::application_routes::ActiveProjectApplicationRoutes;
use crate::capabilities::capabilities;
use crate::http_admission::{
    DASHBOARD_REQUEST_CONTROL_MISSING_CODE, DASHBOARD_REQUEST_CONTROL_MISSING_DETAIL,
};
use crate::project_gateway::{
    SelectedProjectApplicationRead, is_selected_project_event_delivery_ack,
    selected_project_application_read,
};
use crate::request_deadline::{
    DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS, DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
    dashboard_http_request_deadline_micros,
};
use crate::routes::router_with_active_application;
use crate::state::{project_memory_owner_for_layout, resolve_lcm_store_for_layout};
use crate::{
    DashboardAccessToken, DashboardApplicationRouters, DashboardApplicationRuntime,
    DashboardConfigurationApplyError, DashboardConfigurationApplyFuture,
    DashboardHttpRequestControlV1, DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1,
    DashboardLcmCanonicalStatsV1, DashboardLcmReadFutureV1, DashboardLcmReadOutcomeV1,
    DashboardLcmReadPortV1, DashboardLcmReadRequestV1, DashboardState, RequestControl,
    application_surface, events_api, events_delivery, memory_analysis, memory_service,
    multi_root_api, native_integration_api, scope, snapshot_cache,
    standalone_dashboard_automation_writer, storage_mode_label, token_count,
    validate_dashboard_host, with_dashboard_http_admission,
};

#[test]
fn automation_run_routes_receive_the_backend_sized_deadline_budget() {
    assert_eq!(
        dashboard_http_request_deadline_micros("/api/application/retained/fact_store_curate"),
        DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS,
    );
    assert_eq!(
        dashboard_http_request_deadline_micros("/api/automation/jobs/nightly-review/run"),
        DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS,
    );
    assert_eq!(
        dashboard_http_request_deadline_micros(
            "/api/projects/project-7/application/retained/fact_store_curate"
        ),
        DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS,
    );
    assert_eq!(
        dashboard_http_request_deadline_micros("/api/automation/runs"),
        DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
    );
    assert_eq!(
        dashboard_http_request_deadline_micros("/api/plugins/holographic/status"),
        DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
    );
    for near_match in [
        "/api/application/retained/fact_store_curate/extra",
        "/api/application/retained/fact_store_curateish",
        "/api/projects//application/retained/fact_store_curate",
        "/api/projects/project-7/application/retained/fact_store_curate/extra",
        "/api/automation/run/session-reflection",
        "/api/automation/run/skill-writing",
        "/api/projects/project-7/automation/run/session-reflection",
        "/api/projects/project-7/automation/run/skill-writing",
        "/api/projects/project-7/automation/run/skill-writer",
        "/api/projects/project-7/automation/runs",
    ] {
        assert_eq!(
            dashboard_http_request_deadline_micros(near_match),
            DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
            "near-match route must retain the ordinary deadline: {near_match}",
        );
    }
}

#[tokio::test]
async fn admitted_automation_route_carries_the_backend_sized_deadline() {
    async fn deadline_budget(
        axum::Extension(control): axum::Extension<DashboardHttpRequestControlV1>,
    ) -> Json<Value> {
        Json(json!({
            "budget_micros": control.deadline().expires_at.0 - control.observed_at().0,
        }))
    }

    let port = 47_123;
    let app = with_dashboard_http_admission(
        Router::new()
            .route(
                "/api/application/retained/fact_store_curate",
                post(deadline_budget),
            )
            .route("/api/automation/runs", get(deadline_budget)),
        std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        test_dashboard_access().clone(),
    );
    for (method, path, expected) in [
        (
            Method::POST,
            "/api/application/retained/fact_store_curate",
            DASHBOARD_AUTOMATION_RUN_REQUEST_DEADLINE_MICROS,
        ),
        (
            Method::GET,
            "/api/automation/runs",
            DASHBOARD_CODE_GRAPH_REQUEST_DEADLINE_MICROS,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::HOST, format!("127.0.0.1:{port}"))
                    .header(
                        header::AUTHORIZATION,
                        test_dashboard_access().basic_authorization(),
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("admitted request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("response body");
        let payload: Value = serde_json::from_slice(&body).expect("deadline response");
        assert_eq!(payload["budget_micros"], json!(expected));
    }
}

mod automatic_fact_receipts_routes;
mod credential_isolation;

/// The loopback authority every admitted-router fixture binds. Requests
/// must carry it as their `Host` header for the admission layer to admit
/// them.
const TEST_DASHBOARD_AUTHORITY: &str = "127.0.0.1:43127";

fn test_dashboard_access() -> &'static DashboardAccessToken {
    static ACCESS: std::sync::LazyLock<DashboardAccessToken> = std::sync::LazyLock::new(|| {
        DashboardAccessToken::mint().expect("dashboard test access token")
    });
    &ACCESS
}

/// A GET that the dashboard HTTP admission layer admits.
fn admitted_request(uri: impl AsRef<str>) -> Request<Body> {
    Request::builder()
        .uri(uri.as_ref())
        .header(header::HOST, TEST_DASHBOARD_AUTHORITY)
        .header(
            header::AUTHORIZATION,
            test_dashboard_access().basic_authorization(),
        )
        .body(Body::empty())
        .expect("admitted dashboard request")
}

/// Other local accounts reach the same loopback port, so the Host and
/// Origin checks alone must not admit a request: only the listener's
/// token does, as a Basic password or a launch URL. The page shell is the one
/// public surface: it carries no secret and a browser cannot attach headers to
/// a navigation or `<script>` fetch.
#[tokio::test]
async fn loopback_requests_need_the_listener_access_token() {
    async fn probe() -> StatusCode {
        StatusCode::NO_CONTENT
    }
    let port = 43_128;
    let access = test_dashboard_access();
    let other = DashboardAccessToken::mint().expect("second access token");
    let app = with_dashboard_http_admission(
        Router::new()
            .route("/", get(probe))
            .route("/api/automation/scheduler/pause", post(probe)),
        std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        access.clone(),
    );
    let host = format!("127.0.0.1:{port}");
    let send = |request: Request<Body>| {
        let app = app.clone();
        async move { app.oneshot(request).await.expect("admission response") }
    };
    let request = |method: Method, uri: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, &host)
    };

    let bare = send(
        request(Method::POST, "/api/automation/scheduler/pause")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(bare.status(), StatusCode::UNAUTHORIZED);
    let foreign = send(
        request(Method::POST, "/api/automation/scheduler/pause")
            .header(header::AUTHORIZATION, other.basic_authorization())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::UNAUTHORIZED);
    let basic = send(
        request(Method::POST, "/api/automation/scheduler/pause")
            .header(header::AUTHORIZATION, access.basic_authorization())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(basic.status(), StatusCode::NO_CONTENT);

    let launch_url = access.launch_url(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
    let launch_path = launch_url
        .strip_prefix(&format!("http://{host}"))
        .expect("launch URL names the bound listener");
    let wrong_launch = send(
        request(Method::GET, &format!("/?token={}", "0".repeat(64)))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        wrong_launch.status(),
        StatusCode::NO_CONTENT,
        "a forged launch URL is just the public shell, never a redirect that carries a token"
    );
    assert!(!wrong_launch.headers().contains_key(header::LOCATION));
    let posted_launch = send(
        request(Method::POST, launch_path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(posted_launch.status(), StatusCode::UNAUTHORIZED);
    let api_launch = send(
        request(
            Method::GET,
            &format!("/api/automation/scheduler/status?{}", &launch_path[2..]),
        )
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(
        api_launch.status(),
        StatusCode::UNAUTHORIZED,
        "the launch token in an API query authenticates nothing"
    );
    let token = launch_path.strip_prefix("/?token=").expect("launch token");
    let launch = send(
        request(Method::GET, &format!("{launch_path}&view=brain"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(launch.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        launch.headers()[header::LOCATION],
        format!("/?view=brain#token={token}").as_str()
    );
    assert!(
        !launch.headers().contains_key(header::SET_COOKIE),
        "a cookie would be replayed to every other loopback port"
    );
    assert_eq!(launch.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(launch.headers()[header::REFERRER_POLICY], "no-referrer");

    let relaunch = send(
        request(Method::GET, launch_path)
            .header(header::AUTHORIZATION, access.basic_authorization())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(relaunch.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        relaunch.headers()[header::LOCATION],
        format!("/#token={token}").as_str()
    );
    let session_cookie = send(
        request(Method::POST, "/api/automation/scheduler/pause")
            .header(
                header::COOKIE,
                format!("tracedecay_dashboard_{port}={token}"),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(session_cookie.status(), StatusCode::UNAUTHORIZED);
}

/// The router as production serves it.
///
/// [`run_until_shutdown_for_tests_with_host_admission`] wraps every served route in
/// [`with_dashboard_http_admission`], which is what supplies the
/// [`DashboardHttpRequestControlV1`] extension every canonical read
/// requires. A bare [`router_with_active_application`] is not a stack that
/// exists in production: its handlers can only fail closed on the missing
/// control, so route behaviour must be asserted through this one.
/// One GET through `app`, decoded: the status and the JSON body.
async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::HOST, TEST_DASHBOARD_AUTHORITY)
                .header(
                    header::AUTHORIZATION,
                    test_dashboard_access().basic_authorization(),
                )
                .body(Body::empty())
                .expect("dashboard GET request"),
        )
        .await
        .expect("dashboard GET response");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("dashboard GET body");
    let value = serde_json::from_slice(&body).expect("dashboard GET JSON");
    (status, value)
}

fn admitted_router(state: DashboardState) -> Router {
    with_dashboard_http_admission(
        router_with_active_application(state, None, Router::new()),
        TEST_DASHBOARD_AUTHORITY
            .parse()
            .expect("loopback dashboard authority"),
        test_dashboard_access().clone(),
    )
}

struct FakeDashboardLcmRead;

impl DashboardLcmReadPortV1 for FakeDashboardLcmRead {
    fn read(
        &self,
        _control: DashboardHttpRequestControlV1,
        _project_id: Option<&str>,
        request: DashboardLcmReadRequestV1,
    ) -> DashboardLcmReadFutureV1<'_> {
        Box::pin(async move {
            let next_cursor = match request {
                DashboardLcmReadRequestV1::Overview { .. }
                | DashboardLcmReadRequestV1::Timeline { .. } => None,
                DashboardLcmReadRequestV1::Search { .. } => Some("opaque-search-cursor".to_owned()),
                DashboardLcmReadRequestV1::Session { .. } => {
                    Some("opaque-session-cursor".to_owned())
                }
            };
            DashboardLcmReadOutcomeV1::Ready(DashboardLcmCanonicalPageV1 {
                messages: vec![DashboardLcmCanonicalMessageV1 {
                    session_id: "session.dashboard".to_owned(),
                    provider: "claude".to_owned(),
                    role: "assistant".to_owned(),
                    timestamp: Some(1),
                    ordinal: 1,
                    content: "canonically hydrated".to_owned(),
                    message_id: "message.dashboard".to_owned(),
                    metadata_json: None,
                    tool_names: None,
                }],
                summary_nodes: Vec::new(),
                overview_matches: None,
                stats: DashboardLcmCanonicalStatsV1 {
                    message_count: 1,
                    ..DashboardLcmCanonicalStatsV1::default()
                },
                has_more: true,
                next_cursor,
            })
        })
    }
}

pub(super) struct DashboardStateFixture {
    pub(super) state: DashboardState,
    layout: StoreLayout,
    _database_authority: tracedecay_runtime_core::db::DatabaseAuthority,
    _temporary: tempfile::TempDir,
}

impl DashboardStateFixture {
    pub(super) async fn open(project_id: &str) -> Self {
        let temporary = tempfile::tempdir().expect("dashboard fixture");
        let project_root = temporary.path().join("project");
        let profile_root = temporary.path().join("profile");
        std::fs::create_dir_all(&project_root).expect("project root");
        let git_init = std::process::Command::new("git")
            .current_dir(&project_root)
            .args(["init", "-q"])
            .status()
            .expect("git init starts");
        assert!(git_init.success(), "git init failed for dashboard fixture");
        assert!(
            tracedecay_runtime_core::storage::write_repository_identity_marker(
                &project_root,
                project_id,
            )
            .expect("dashboard fixture enrollment marker"),
            "dashboard fixture project root must accept its enrollment marker"
        );
        let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
            &project_root,
            &profile_root,
            project_id,
        )
        .expect("dashboard store layout");
        std::fs::create_dir_all(
            layout
                .graph_db_path
                .parent()
                .expect("dashboard database parent"),
        )
        .expect("dashboard database parent");
        crate::register_test_schema_installer();
        let database_authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
            &layout.graph_db_path,
            "dashboard state fixture",
        )
        .expect("dashboard database authority");
        let (database, _) = Database::publish_test_runtime(
            &layout.graph_db_path,
            &database_authority,
            tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("dashboard database");
        let database = Arc::new(database);
        let memory_owner =
            project_memory_owner_for_layout(&layout).expect("dashboard project memory owner");
        let state = DashboardState {
            build_version: "0.0.0-fixture+aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            host_io: crate::test_support::fixture_host_io(),
            project_id: layout.identity.project_id.clone(),
            resolved_scope: scope::resolve_dashboard_scope(
                &project_root,
                layout.identity.project_id.as_deref(),
            ),
            code_graph_read_admission: None,
            code_graph_projection_read_port: None,
            code_read_authority: None,
            project_graph: None,
            project_graph_resolver: None,
            memory_owner,
            graph_conn: database.read_connection(),
            _database_guards: vec![Arc::clone(&database)],
            graph_telemetry_handle: database.storage_telemetry_handle().ok(),
            graph_db_path: layout.graph_db_path.display().to_string(),
            mem_db: Arc::clone(&database),
            mem_db_path: layout.graph_db_path.display().to_string(),
            lcm_db: None,
            lcm_db_path: layout.sessions_db_path.display().to_string(),
            lcm_scope: "unavailable".to_owned(),
            session_authority: crate::DashboardSessionAuthorityStateV1::Unavailable,
            session_mount: None,
            lcm_read_authority: None,
            git_correlation_read_authority: None,
            delivery_read_authority: None,
            savings_db: None,
            savings_db_path: String::new(),
            project_root: project_root.clone(),
            code_index_freshness_reader: None,
            feedback_status_reader: None,
            pr_autotrack_reader: None,
            storage_mode: storage_mode_label(&layout.storage_mode).to_owned(),
            store_root: layout.data_root.clone(),
            profile_root: profile_root.clone(),
            dashboard_root: layout.dashboard_root.clone(),
            retention_config: tracedecay_configuration::RetentionConfig::default(),
            user_settings: Arc::new(
                tracedecay_configuration::ProductionUserSettingsDaemonClient::default(),
            ),
            profile_code_index_worker_settings: None,
            token_counts: Arc::new(token_count::TokenCountCache::new()),
            derived_snapshots: Arc::new(snapshot_cache::DerivedSnapshotCaches::new()),
            code_diagnostics_authority: None,
            automation_authority: None,
            automation_observation: None,
            automation_scheduler_reconciler: None,
            automation_writer: standalone_dashboard_automation_writer(),
            doctor_report_reader: None,
            remote_operational_status_reader: None,
            application_invocation_executor: None,
            delivery_settlements: Arc::new(
                events_delivery::DashboardDeliverySettlementRegistryV1::new(None),
            ),
            event_source_poll: events_api::SharedSourcePoll::default(),
        };
        Self {
            state,
            layout,
            _database_authority: database_authority,
            _temporary: temporary,
        }
    }

    async fn add_vector_fact(&self, index: usize) {
        let checksum = (index as u64).wrapping_mul(1_000_003);
        let content = format!(
            "cachetoken{index} archive vector checksum{checksum:016x} payload{:016x}",
            checksum.rotate_left(17)
        );
        let entity = format!("CacheEntity{index}");
        let metadata = json!({"fixture": "dashboard-cache-freshness", "index": index});
        let payload = json!({
            "content": content,
            "category": "project",
            "tags": ["dashboard-cache"],
            "entities": [entity],
            "metadata": metadata,
        });
        let receipt = match sanitize_memory_fact_payload(payload.clone())
            .expect("sanitize dashboard cache fixture")
        {
            MemoryFactSanitizationV1::Durable {
                payload: sanitized,
                receipt,
            } => {
                assert_eq!(sanitized, payload);
                receipt
            }
            MemoryFactSanitizationV1::Quarantined => {
                panic!("dashboard cache fixture must remain durable")
            }
        };
        let command = ProjectMemoryFactAddMaterialV1::new(
            self.state.memory_owner.clone(),
            content,
            FactCategoryV1::Project,
            None,
            vec!["dashboard-cache".to_owned()],
            vec![entity],
            metadata,
            receipt,
            None,
            Confidence::new(0.8).expect("dashboard cache fixture trust"),
            None,
        )
        .and_then(|material| {
            material.into_command(
                ProvenanceId::new(format!("dashboard.cache.seed.{index}"))
                    .expect("dashboard cache fixture operation"),
            )
        })
        .expect("dashboard cache add command");
        let outcome = DatabaseFactStore::new(&self.state.mem_db)
            .add_project_memory_fact(
                command,
                &FactWriteControl::new(Arc::new(|| false), Arc::new(|| true)),
            )
            .await
            .expect("add dashboard cache fixture fact");
        assert_eq!(
            outcome.disposition(),
            ProjectMemoryFactAddDispositionV1::Added
        );
    }

    async fn add_vector_facts(&self, count: usize) {
        for index in 0..count {
            self.add_vector_fact(index).await;
        }
    }
}

#[derive(Clone, Copy)]
struct HeartbeatMeasurement {
    p95_lateness_micros: u128,
    max_lateness_micros: u128,
}

fn percentile_micros(samples: &[Duration], percentile: usize) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut micros = samples.iter().map(Duration::as_micros).collect::<Vec<_>>();
    micros.sort_unstable();
    let rank = micros.len().saturating_mul(percentile).div_ceil(100);
    micros[rank.saturating_sub(1).min(micros.len() - 1)]
}

async fn with_tokio_heartbeat<T>(future: impl Future<Output = T>) -> (T, HeartbeatMeasurement) {
    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat_stop = Arc::clone(&stop);
    let heartbeat = tokio::spawn(async move {
        let interval = Duration::from_millis(1);
        let mut lateness = Vec::new();
        while !heartbeat_stop.load(Ordering::Acquire) {
            let started = Instant::now();
            tokio::time::sleep(interval).await;
            lateness.push(started.elapsed().saturating_sub(interval));
        }
        lateness
    });
    tokio::task::yield_now().await;
    let result = future.await;
    stop.store(true, Ordering::Release);
    let lateness = heartbeat.await.expect("heartbeat task");
    (
        result,
        HeartbeatMeasurement {
            p95_lateness_micros: percentile_micros(&lateness, 95),
            max_lateness_micros: lateness.iter().map(Duration::as_micros).max().unwrap_or(0),
        },
    )
}

#[tokio::test]
async fn store_write_refreshes_projection_and_similarity_once() {
    let fixture = DashboardStateFixture::open("project.dashboard-cache-freshness").await;
    let control = tracedecay_store::FactReadControl::new(Arc::new(|| false));

    let projection_before =
        memory_service::projection_payload(&fixture.state, "", 2_000, &control).await;
    let similarity_before =
        memory_service::similarity_payload(&fixture.state, 0.5, 100, &control).await;
    fixture.add_vector_fact(1).await;
    let projection_after =
        memory_service::projection_payload(&fixture.state, "", 2_000, &control).await;
    let similarity_after =
        memory_service::similarity_payload(&fixture.state, 0.5, 100, &control).await;
    let projection_warm =
        memory_service::projection_payload(&fixture.state, "", 2_000, &control).await;
    let similarity_warm =
        memory_service::similarity_payload(&fixture.state, 0.5, 100, &control).await;

    assert_eq!(projection_before.points.len(), 0);
    assert_eq!(similarity_before.count, 0);
    assert_eq!(projection_after.scan.as_ref().unwrap().cache_state, "miss");
    assert_eq!(projection_after.scan.as_ref().unwrap().vector_rows_read, 1);
    assert_eq!(projection_after.points.len(), 1);
    assert_eq!(similarity_after.scan.as_ref().unwrap().cache_state, "miss");
    assert_eq!(similarity_after.scan.as_ref().unwrap().vector_rows_read, 1);
    assert_eq!(similarity_after.count, 1);
    assert_eq!(projection_warm.scan.as_ref().unwrap().cache_state, "hit");
    assert_eq!(projection_warm.scan.as_ref().unwrap().vector_rows_read, 0);
    assert_eq!(similarity_warm.scan.as_ref().unwrap().cache_state, "hit");
    assert_eq!(similarity_warm.scan.as_ref().unwrap().vector_rows_read, 0);
}

#[tokio::test]
async fn retiring_the_dashboard_state_releases_its_derived_snapshots() {
    let fixture = DashboardStateFixture::open("project.dashboard-derived-retirement").await;
    let control = tracedecay_store::FactReadControl::new(Arc::new(|| false));
    fixture.add_vector_facts(3).await;

    let projection = memory_service::projection_payload(&fixture.state, "", 2_000, &control).await;
    let similarity = memory_service::similarity_payload(&fixture.state, 0.5, 100, &control).await;
    assert_eq!(projection.scan.as_ref().unwrap().cache_state, "miss");
    assert_eq!(projection.points.len(), 3);
    assert_eq!(similarity.scan.as_ref().unwrap().cache_state, "miss");
    assert_eq!(similarity.count, 3);

    // The populated caches are owned by the state (and its clones), not by
    // the process: once the last handle to this store's dashboard state is
    // gone, the derived snapshots are gone with it.
    let DashboardStateFixture {
        state,
        layout: _,
        _database_authority,
        _temporary,
    } = fixture;
    let retained = Arc::downgrade(&state.derived_snapshots);
    let clone = state.clone();
    drop(state);
    assert!(
        retained.upgrade().is_some(),
        "a live clone of the state still owns the derived snapshots"
    );
    drop(clone);
    assert!(
        retained.upgrade().is_none(),
        "retiring the last dashboard state must release its derived snapshots"
    );
    drop((_database_authority, _temporary));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual cache measurement; seeds bounded stores and prints operation counts"]
async fn measure_projection_and_similarity_cache_costs() {
    const COLD_RUNS: usize = 5;
    const WARM_RUNS: usize = 20;

    println!(
        "caps: projection={} similarity={}; cold_runs={} warm_runs={}",
        memory_service::projection_point_cap(),
        memory_analysis::SIMILARITY_FACT_CAP,
        COLD_RUNS,
        WARM_RUNS,
    );
    println!(
        "| rows | operation | cold p50 us | cold p95 us | warm p50 us | warm p95 us | cold rows read | warm rows read | heartbeat cold p95/max us | heartbeat warm p95/max us |"
    );
    println!("|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|");

    for row_count in [1_000_usize, 2_000] {
        let mut fixtures = Vec::with_capacity(COLD_RUNS);
        for run in 0..COLD_RUNS {
            let fixture = DashboardStateFixture::open(&format!(
                "project.dashboard-cache-measurement-{row_count}-{run}"
            ))
            .await;
            fixture.add_vector_facts(row_count).await;
            fixtures.push(fixture);
        }
        let control = tracedecay_store::FactReadControl::new(Arc::new(|| false));

        let mut projection_cold = Vec::with_capacity(COLD_RUNS);
        let mut projection_cold_rows = Vec::with_capacity(COLD_RUNS);
        let mut similarity_cold = Vec::with_capacity(COLD_RUNS);
        let mut similarity_cold_rows = Vec::with_capacity(COLD_RUNS);
        let mut similarity_cold_heartbeat = None;
        for (index, fixture) in fixtures.iter().enumerate() {
            let started = Instant::now();
            let projection = tracing::Instrument::instrument(
                memory_service::projection_payload(
                    &fixture.state,
                    "",
                    memory_service::projection_point_cap(),
                    &control,
                ),
                tracing::trace_span!("dashboard.measure.projection_cold"),
            )
            .await;
            projection_cold.push(started.elapsed());
            projection_cold_rows.push(
                projection
                    .scan
                    .as_ref()
                    .expect("projection cold row count")
                    .vector_rows_read,
            );
            assert_eq!(projection.scan.as_ref().unwrap().cache_state, "miss");

            let started = Instant::now();
            let (similarity, heartbeat) = if index == 0 {
                let (payload, heartbeat) = with_tokio_heartbeat(tracing::Instrument::instrument(
                    memory_service::similarity_payload(&fixture.state, 0.5, 100, &control),
                    tracing::trace_span!("dashboard.measure.similarity_cold"),
                ))
                .await;
                (payload, Some(heartbeat))
            } else {
                (
                    tracing::Instrument::instrument(
                        memory_service::similarity_payload(&fixture.state, 0.5, 100, &control),
                        tracing::trace_span!("dashboard.measure.similarity_cold"),
                    )
                    .await,
                    None,
                )
            };
            similarity_cold.push(started.elapsed());
            similarity_cold_rows.push(
                similarity
                    .scan
                    .as_ref()
                    .expect("similarity cold row count")
                    .vector_rows_read,
            );
            assert_eq!(similarity.scan.as_ref().unwrap().cache_state, "miss");
            if heartbeat.is_some() {
                similarity_cold_heartbeat = heartbeat;
            }
        }

        let warm_fixture = &fixtures[0];
        let mut projection_warm = Vec::with_capacity(WARM_RUNS);
        let mut projection_warm_rows = Vec::with_capacity(WARM_RUNS);
        for _ in 0..WARM_RUNS {
            let started = Instant::now();
            let projection = tracing::Instrument::instrument(
                memory_service::projection_payload(
                    &warm_fixture.state,
                    "",
                    memory_service::projection_point_cap(),
                    &control,
                ),
                tracing::trace_span!("dashboard.measure.projection_warm"),
            )
            .await;
            projection_warm.push(started.elapsed());
            projection_warm_rows.push(
                projection
                    .scan
                    .as_ref()
                    .expect("projection warm row count")
                    .vector_rows_read,
            );
            assert_eq!(projection.scan.as_ref().unwrap().cache_state, "hit");
        }

        let ((similarity_warm, similarity_warm_rows), similarity_warm_heartbeat) =
            with_tokio_heartbeat(async {
                let mut timings = Vec::with_capacity(WARM_RUNS);
                let mut rows = Vec::with_capacity(WARM_RUNS);
                for _ in 0..WARM_RUNS {
                    let started = Instant::now();
                    let similarity = tracing::Instrument::instrument(
                        memory_service::similarity_payload(&warm_fixture.state, 0.5, 100, &control),
                        tracing::trace_span!("dashboard.measure.similarity_warm"),
                    )
                    .await;
                    timings.push(started.elapsed());
                    rows.push(
                        similarity
                            .scan
                            .as_ref()
                            .expect("similarity warm row count")
                            .vector_rows_read,
                    );
                    assert_eq!(similarity.scan.as_ref().unwrap().cache_state, "hit");
                }
                (timings, rows)
            })
            .await;
        let cold_heartbeat =
            similarity_cold_heartbeat.expect("cold similarity heartbeat measurement");

        assert!(
            projection_warm_rows.iter().all(|rows| *rows == 0),
            "warm projection must perform zero vector-row reads"
        );
        assert!(
            similarity_warm_rows.iter().all(|rows| *rows == 0),
            "warm similarity must perform zero vector-row reads"
        );
        println!(
            "| {row_count} | projection | {} | {} | {} | {} | {} | 0 | - | - |",
            percentile_micros(&projection_cold, 50),
            percentile_micros(&projection_cold, 95),
            percentile_micros(&projection_warm, 50),
            percentile_micros(&projection_warm, 95),
            projection_cold_rows.iter().max().copied().unwrap_or(0),
        );
        println!(
            "| {row_count} | similarity | {} | {} | {} | {} | {} | 0 | {}/{} | {}/{} |",
            percentile_micros(&similarity_cold, 50),
            percentile_micros(&similarity_cold, 95),
            percentile_micros(&similarity_warm, 50),
            percentile_micros(&similarity_warm, 95),
            similarity_cold_rows.iter().max().copied().unwrap_or(0),
            cold_heartbeat.p95_lateness_micros,
            cold_heartbeat.max_lateness_micros,
            similarity_warm_heartbeat.p95_lateness_micros,
            similarity_warm_heartbeat.max_lateness_micros,
        );
    }

    println!(
        "10k/50k stores omitted: canonical fixture seeding commits each fact through the production write authority; both dashboard vector reads are already capped at 2000 rows."
    );
}

/// Serves exactly one persisted named collection, mirroring the daemon
/// scope-set read: an exact-id hit answers the frozen scope set, anything
/// else is a truthful absent read.
#[derive(Clone)]
struct SingleCollectionRuntime {
    scope_set: tracedecay_contracts::AuthorizedScopeSet,
    native_integration_status: Option<tracedecay_contracts::NativeIntegrationSurfaceResultV1>,
    rebound_roots: Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
}

impl SingleCollectionRuntime {
    fn persisted(collection: &str) -> Self {
        use std::collections::BTreeSet;

        let capability = "capability.multi-root.query";
        let use_case = "use-case.multi-root.query";
        let scope = tracedecay_contracts::ResolvedScope::new(
            ProjectId::new("project.dashboard-collection").expect("project"),
            tracedecay_domain::RepositoryId::new("repository.dashboard-collection")
                .expect("repository"),
            tracedecay_domain::WorktreeId::new("worktree.main").expect("worktree"),
            Some(tracedecay_domain::RefId::new("refs/heads/main").expect("reference")),
        )
        .expect("scope");
        let grant = tracedecay_contracts::CapabilityGrantSnapshot::new(
            "grant.dashboard-collection"
                .to_owned()
                .try_into()
                .expect("grant id"),
            1,
            tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("digest"),
            tracedecay_domain::ActorId::new("actor.issuer").expect("issuer"),
            tracedecay_domain::UtcMicros(1),
            tracedecay_domain::UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([
                tracedecay_tool_catalog::CapabilityId::new(capability).expect("capability")
            ]),
            BTreeSet::from([tracedecay_tool_catalog::UseCaseId::new(use_case).expect("use case")]),
            tracedecay_contracts::DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = tracedecay_contracts::RequestContext::new(
            tracedecay_domain::ActorId::new("actor.requester").expect("actor"),
            scope,
            grant,
            tracedecay_contracts::RequestId::new("request.dashboard-collection")
                .expect("request id"),
            tracedecay_contracts::Deadline::new(tracedecay_domain::UtcMicros(900))
                .expect("deadline"),
            tracedecay_contracts::CancellationContext::active("cancel.dashboard-collection")
                .expect("cancellation"),
        )
        .expect("context");
        let scope_set = tracedecay_contracts::AuthorizedScopeSetAuthority::authorize(
            tracedecay_domain::ScopeSetId::new(collection).expect("collection id"),
            tracedecay_domain::ScopeSetRevision::new(1).expect("revision"),
            vec![context],
            &tracedecay_tool_catalog::CapabilityId::new(capability).expect("capability"),
            &tracedecay_tool_catalog::UseCaseId::new(use_case).expect("use case"),
            tracedecay_domain::UtcMicros(10),
        )
        .expect("authorized scope set");
        Self {
            scope_set,
            native_integration_status: None,
            rebound_roots: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

impl DashboardApplicationRuntime for SingleCollectionRuntime {
    fn user_profile_id(&self) -> Option<&tracedecay_domain::UserProfileId> {
        None
    }

    fn for_project_root(
        &self,
        project_root: &std::path::Path,
    ) -> std::result::Result<Arc<dyn DashboardApplicationRuntime>, String> {
        self.rebound_roots
            .lock()
            .expect("selected project bindings")
            .push(project_root.to_path_buf());
        Ok(Arc::new(self.clone()))
    }

    fn routers(
        &self,
        _active_project_id: ProjectId,
    ) -> std::result::Result<DashboardApplicationRouters, String> {
        Err("the single-collection test runtime mounts no application routers".to_owned())
    }

    fn apply_configuration_batch(
        &self,
        _request_id: tracedecay_contracts::RequestId,
        _mutations: Vec<
            tracedecay_global_db::configuration::contracts::types::DirectConfigurationMutation,
        >,
        _expected_revision: tracedecay_domain::configuration::ConfigurationRevisionId,
        _idempotency_key: tracedecay_domain::configuration::ConfigurationIdempotencyKey,
    ) -> DashboardConfigurationApplyFuture<'_> {
        Box::pin(async {
            Err(
                DashboardConfigurationApplyError::ApplicationContractViolation(
                    tracedecay_contracts::ApplicationContractError::Inconsistent {
                        field: "single-collection test runtime configuration",
                    },
                ),
            )
        })
    }

    fn read_multi_root_scope_set(
        &self,
        _control: DashboardHttpRequestControlV1,
        scope_set_id: tracedecay_domain::ScopeSetId,
    ) -> application_surface::DashboardScopeSetReadFuture<'_> {
        let read = (self.scope_set.scope_set_id() == &scope_set_id).then(|| self.scope_set.clone());
        Box::pin(async move { Ok(read) })
    }

    fn native_integration_status(
        &self,
        _control: DashboardHttpRequestControlV1,
        _transaction_id: tracedecay_domain::NativeIntegrationTransactionId,
    ) -> application_surface::DashboardNativeIntegrationStatusFuture<'_> {
        let result = self.native_integration_status.clone();
        Box::pin(async move {
            result.ok_or(application_surface::DashboardDaemonReadUnavailableV1 {
                detail: "the single-collection test runtime scripts no native-integration status"
                    .to_owned(),
            })
        })
    }
}

#[tokio::test]
async fn capabilities_with_admitted_transport_report_the_typed_no_collection_state() {
    let fixture = DashboardStateFixture::open("project.dashboard-collection-capabilities").await;
    let mut state = fixture.state;
    state.application_invocation_executor = Some(Arc::new(SingleCollectionRuntime::persisted(
        "scope-set.dashboard-capabilities",
    )));

    let Json(capabilities) = capabilities(
        State(state),
        RequestControl(DashboardHttpRequestControlV1::test_fixture(
            "dashboard-lcm-test",
        )),
    )
    .await;

    assert_eq!(capabilities["features"]["multi_root"], true);
    assert_eq!(capabilities["multi_root"]["status"], "unavailable");
    assert_eq!(
        capabilities["multi_root"]["reason"],
        "no default multi-root collection is configured; name an explicit collection"
    );
}

#[tokio::test]
async fn dashboard_resolves_a_named_multi_root_collection() {
    let fixture = DashboardStateFixture::open("project.dashboard-collection-resolve").await;
    let mut state = fixture.state;
    let runtime = SingleCollectionRuntime::persisted("scope-set.dashboard-resolve");
    let expected_digest = runtime.scope_set.digest().clone();
    state.application_invocation_executor = Some(Arc::new(runtime));

    let mounted = multi_root_api::resolve_collection_capability(
        &state,
        DashboardHttpRequestControlV1::test_fixture("dashboard-lcm-test"),
        Some(tracedecay_domain::ScopeSetId::new("scope-set.dashboard-resolve").unwrap()),
    )
    .await;
    let tracedecay_api::read_model::multi_root::MultiRootCapabilityV1::Mounted {
        scope_set_id,
        revision,
        scope_set_digest,
        root_count,
    } = mounted
    else {
        panic!("an explicit persisted collection must resolve as mounted");
    };
    assert_eq!(scope_set_id.as_str(), "scope-set.dashboard-resolve");
    assert_eq!(revision.get(), 1);
    assert_eq!(scope_set_digest, expected_digest);
    assert_eq!(root_count, 1);

    // An explicit target naming an unpersisted collection is a typed
    // not-persisted state, never a silent fallback to another scope set.
    let missing = multi_root_api::resolve_collection_capability(
        &state,
        DashboardHttpRequestControlV1::test_fixture("dashboard-lcm-test"),
        Some(tracedecay_domain::ScopeSetId::new("scope-set.dashboard-missing").unwrap()),
    )
    .await;
    let tracedecay_api::read_model::multi_root::MultiRootCapabilityV1::Unavailable { reason } =
        missing
    else {
        panic!("an unpersisted collection must stay typed unavailable");
    };
    assert_eq!(
        reason,
        "multi-root collection scope-set.dashboard-missing names no persisted scope set for this project"
    );
}

async fn json_response_body(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body");
    (status, serde_json::from_slice(&body).expect("json body"))
}

#[tokio::test]
async fn standalone_dashboard_reports_native_integration_authority_unmounted() {
    let fixture = DashboardStateFixture::open("project.dashboard-native-status-standalone").await;

    let response = native_integration_api::status(
        State(fixture.state),
        RequestControl(DashboardHttpRequestControlV1::test_fixture(
            "dashboard-lcm-test",
        )),
        axum::extract::Query(native_integration_api::NativeIntegrationStatusQueryV1 {
            transaction_id: "transaction.dashboard.native".to_owned(),
        }),
    )
    .await;
    let (status, body) = json_response_body(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["outcome"], "unavailable");
    assert_eq!(body["reason"], "authority_unmounted");
}

#[tokio::test]
async fn doctor_routes_expose_diagnostics_without_mutation_endpoints() {
    let fixture = DashboardStateFixture::open("project.dashboard-doctor-read-only").await;
    let app = router_with_active_application(fixture.state, None, Router::new());

    for (method, path) in [
        (Method::POST, "/api/doctor/remediations/preview"),
        (Method::POST, "/api/doctor/remediations/apply"),
        (Method::GET, "/api/doctor/remediations/operation"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .expect("legacy Doctor mutation request"),
            )
            .await
            .expect("legacy Doctor mutation response");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{path} is not mounted"
        );
    }
}

/// A router served without [`with_dashboard_http_admission`] has no request
/// control. That is a wiring fault, and every canonical read must answer it
/// the same way: one 503 with the shared admission problem body, never a
/// 200 carrying fabricated empty data.
#[tokio::test]
async fn reads_without_request_control_fail_closed_with_one_problem_shape() {
    let fixture = DashboardStateFixture::open("project.dashboard-missing-control").await;
    let bare = router_with_active_application(fixture.state, None, Router::new());
    for path in [
        "/api/plugins/holographic/projection",
        "/api/plugins/holographic/similarity",
        "/api/plugins/holographic/oplog",
        "/api/plugins/holographic/status",
        "/api/capabilities",
    ] {
        let (status, body) = get_json(&bare, path).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path} must fail closed without request control"
        );
        assert_eq!(
            body,
            json!({
                "status": "unavailable",
                "error": DASHBOARD_REQUEST_CONTROL_MISSING_CODE,
                "detail": DASHBOARD_REQUEST_CONTROL_MISSING_DETAIL,
            }),
            "{path} must carry the shared admission problem body"
        );
    }
}

#[tokio::test]
async fn dashboard_user_job_run_without_automation_authority_fails_closed() {
    let fixture = DashboardStateFixture::open("project.dashboard-job-observation").await;
    let app = admitted_router(fixture.state);
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/automation/jobs")
                .header(header::HOST, TEST_DASHBOARD_AUTHORITY)
                .header(
                    header::AUTHORIZATION,
                    test_dashboard_access().basic_authorization(),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "id": "observation-required",
                        "name": "Observation required",
                        "prompt": "Review the current project"
                    })
                    .to_string(),
                ))
                .expect("create automation job request"),
        )
        .await
        .expect("create automation job response");
    assert_eq!(create.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/automation/jobs/observation-required/run")
                .header(header::HOST, TEST_DASHBOARD_AUTHORITY)
                .header(
                    header::AUTHORIZATION,
                    test_dashboard_access().basic_authorization(),
                )
                .body(Body::empty())
                .expect("run automation job request"),
        )
        .await
        .expect("run automation job response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("automation unavailable body");
    let payload: Value = serde_json::from_slice(&body).expect("automation unavailable json");
    assert_eq!(
        payload["detail"],
        json!("dashboard automation authority is not mounted")
    );
}

#[tokio::test]
async fn daemon_dashboard_without_retained_authority_fails_closed() {
    let fixture = DashboardStateFixture::open("project.dashboard-session-unavailable").await;
    let selected = resolve_lcm_store_for_layout(&fixture.layout, None);

    assert!(selected.lcm_db.is_none());
    assert_eq!(selected.scope, "unavailable");
    assert_eq!(
        selected.path,
        fixture.layout.sessions_db_path.display().to_string()
    );
}

#[test]
fn dashboard_bindings_are_loopback_only() {
    assert_eq!(
        validate_dashboard_host("127.0.0.1").expect("IPv4 loopback"),
        "127.0.0.1"
    );
    assert_eq!(
        validate_dashboard_host("localhost").expect("localhost"),
        "localhost"
    );
    assert_eq!(
        validate_dashboard_host("::1").expect("IPv6 loopback"),
        "::1"
    );
    assert!(validate_dashboard_host("0.0.0.0").is_err());
}

#[tokio::test]
async fn application_routes_are_active_project_only() {
    let fixture = DashboardStateFixture::open("project.dashboard-application-route").await;
    let state = fixture.state;
    let project_id = state.project_id.clone().expect("active project id");
    let application = ActiveProjectApplicationRoutes {
        http_router: Router::new()
            .route("/probe", get(|| async { StatusCode::NO_CONTENT }))
            .route("/work/views", post(|| async { StatusCode::NO_CONTENT })),
        dashboard_configuration_router: Router::new()
            .route("/probe", get(|| async { StatusCode::ACCEPTED })),
        dashboard_feedback_router: Router::new().route("/probe", get(|| async { StatusCode::OK })),
        dashboard_work_router: Router::new()
            .route("/views", post(|| async { StatusCode::NO_CONTENT })),
        executor: None,
    };
    let app = router_with_active_application(state, Some(application), Router::new());

    let active = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/application/probe")
                .body(Body::empty())
                .expect("active application request"),
        )
        .await
        .expect("active application response");
    assert_eq!(active.status(), StatusCode::NO_CONTENT);

    let feedback = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/feedback/probe")
                .body(Body::empty())
                .expect("active dashboard feedback request"),
        )
        .await
        .expect("active dashboard feedback response");
    assert_eq!(feedback.status(), StatusCode::OK);

    let dashboard_configuration = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/dashboard/application/probe")
                .body(Body::empty())
                .expect("dashboard application request"),
        )
        .await
        .expect("dashboard application response");
    assert_eq!(dashboard_configuration.status(), StatusCode::ACCEPTED);

    let work = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/work/views")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .expect("dashboard Work request"),
        )
        .await
        .expect("dashboard Work response");
    assert_eq!(work.status(), StatusCode::NO_CONTENT);

    let selected = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{project_id}/application/probe"))
                .body(Body::empty())
                .expect("selected-project application request"),
        )
        .await
        .expect("selected-project application response");
    assert_eq!(selected.status(), StatusCode::NOT_FOUND);
}

/// The V2 read-model routes must be reachable through both
/// router construction paths, the active-project gateway (`/api/…`) and the
/// project-scoped gateway (`/api/projects/{id}/…`), mirroring how the
/// existing families are exposed.
#[tokio::test]
async fn v2_read_models_are_reachable_through_both_gateways() {
    let fixture = DashboardStateFixture::open("project.dashboard-read-model-route").await;
    let state = fixture.state;
    let project_id = state.project_id.clone().expect("active project id");
    let app = router_with_active_application(state, None, Router::new());

    // Every V2 read-model route resolves both through the active gateway and
    // through the project-scoped gateway for the active project.
    for tail in [
        "doctor/findings",
        "storage/telemetry",
        "code-index/freshness",
        "feedback/status",
    ] {
        let active = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/{tail}"))
                    .body(Body::empty())
                    .expect("active v2 request"),
            )
            .await
            .expect("active v2 response");
        assert_eq!(
            active.status(),
            StatusCode::OK,
            "active gateway route /api/{tail} should resolve"
        );
        let body = axum::body::to_bytes(active.into_body(), 1 << 20)
            .await
            .expect("active body");
        let value: Value = serde_json::from_slice(&body).expect("envelope json");
        assert_eq!(value["schema_revision"], 1, "/api/{tail} envelope revision");
        assert!(
            value.get("domain_state").is_some(),
            "/api/{tail} carries a domain_state"
        );

        let scoped = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/{tail}"))
                    .body(Body::empty())
                    .expect("scoped v2 request"),
            )
            .await
            .expect("scoped v2 response");
        assert_eq!(
            scoped.status(),
            StatusCode::OK,
            "project-scoped gateway route for /api/{tail} should resolve"
        );
    }
}

/// Graph reads are served only from an admitted, verified projection, so a
/// dashboard that never mounted one cannot answer `ready`, it must answer
/// an enveloped `unknown` naming the missing registry, and must never
/// fabricate totals from the raw store connection the state still holds.
///
/// The `ready` side of this contract needs a real verified generation, so
/// it is proven end-to-end against a mounted graph authority by
/// `graph_api_returns_seeded_overview_search_detail_and_subgraph` in
/// `tests/dashboard_api_test/graph.rs`.
#[tokio::test]
async fn graph_overview_without_a_verified_projection_is_an_enveloped_unknown_read() {
    let fixture = DashboardStateFixture::open("project.dashboard-graph-envelope").await;
    assert!(
        fixture.state.code_graph_read_admission.is_none()
            && fixture.state.code_graph_projection_read_port.is_none(),
        "this fixture must not mount a graph authority",
    );
    let app = admitted_router(fixture.state);

    let response = app
        .oneshot(admitted_request("/api/plugins/graph/overview"))
        .await
        .expect("graph overview response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the daemon answered; source unavailability belongs in the envelope"
    );
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("graph overview body");
    let value: Value = serde_json::from_slice(&body).expect("graph overview json");

    assert_eq!(
        value["schema_revision"],
        crate::read_model::DASHBOARD_SCHEMA_REVISION_V1
    );
    assert_eq!(value["domain_state"], "unknown");
    assert_eq!(value["authorization"]["outcome"], "authorized");
    assert_eq!(value["coverage"]["completeness"], "unknown");
    assert_eq!(
        value["coverage"]["omission_reasons"],
        json!(["missing_registry"])
    );
    assert!(
        value["payload"].is_null(),
        "an unmounted graph authority must not fabricate a payload: {value}"
    );
    assert!(
        value["version"]["graph_version"].is_null(),
        "an unmounted graph authority has no verified generation: {value}"
    );
}

#[tokio::test]
async fn missing_project_registry_is_an_enveloped_unknown_read() {
    let fixture = DashboardStateFixture::open("project.dashboard-registry-envelope").await;
    let app = router_with_active_application(fixture.state, None, Router::new());

    let (status, value) = get_json(&app, "/api/projects").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the daemon answered; source unavailability belongs in the envelope"
    );

    assert_eq!(value["schema_revision"], 1);
    assert_eq!(value["domain_state"], "unknown");
    assert_eq!(value["payload"]["status"], "missing_registry");
    assert_eq!(value["coverage"]["completeness"], "unknown");
}

#[tokio::test]
async fn lcm_search_and_session_use_canonical_daemon_authority_and_opaque_cursors() {
    let mut fixture = DashboardStateFixture::open("project.dashboard-lcm-envelope").await;
    fixture.state.lcm_read_authority = Some(Arc::new(FakeDashboardLcmRead));
    let app = router_with_active_application(fixture.state, None, Router::new());

    for (uri, cursor) in [
        (
            "/api/plugins/hermes-lcm/search?q=needle",
            "opaque-search-cursor",
        ),
        (
            "/api/plugins/hermes-lcm/session/session.dashboard",
            "opaque-session-cursor",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .extension(DashboardHttpRequestControlV1::test_fixture(
                        "dashboard-lcm-test",
                    ))
                    .body(Body::empty())
                    .expect("LCM browse request"),
            )
            .await
            .expect("LCM browse response");
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("LCM browse body");
        let value: Value = serde_json::from_slice(&body).expect("LCM browse json");

        assert_eq!(value["schema_revision"], 1, "{uri}");
        assert_eq!(value["domain_state"], "ready", "{uri}");
        assert_eq!(
            value["payload"]["messages"]
                .as_array()
                .or_else(|| value["payload"]["matches"]["messages"].as_array())
                .and_then(|messages| messages.first())
                .and_then(|message| message["content"].as_str()),
            Some("canonically hydrated"),
            "{uri}",
        );
        assert_eq!(value["payload"]["next_cursor"], cursor, "{uri}");
    }
}

#[tokio::test]
async fn lcm_aggregate_reads_use_the_mounted_daemon_authority() {
    let mut fixture = DashboardStateFixture::open("project.dashboard-lcm-aggregate").await;
    fixture.state.lcm_read_authority = Some(Arc::new(FakeDashboardLcmRead));
    let app = router_with_active_application(fixture.state, None, Router::new());

    for uri in [
        "/api/plugins/hermes-lcm/overview",
        "/api/plugins/hermes-lcm/timeline",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .extension(DashboardHttpRequestControlV1::test_fixture(
                        "dashboard-lcm-test",
                    ))
                    .body(Body::empty())
                    .expect("LCM aggregate request"),
            )
            .await
            .expect("LCM aggregate response");
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("LCM aggregate body");
        let value: Value = serde_json::from_slice(&body).expect("LCM aggregate json");

        assert_eq!(value["domain_state"], "ready", "{uri}");
        assert_eq!(value["coverage"]["completeness"], "complete", "{uri}");
        assert_eq!(value["coverage"]["eligible"], 1, "{uri}");
        assert_eq!(value["coverage"]["examined"], 1, "{uri}");
        assert_eq!(value["payload"]["exists"], true, "{uri}");
    }
}

#[tokio::test]
async fn unavailable_analytics_detail_reads_are_enveloped_unknown_states() {
    let fixture = DashboardStateFixture::open("project.dashboard-analytics-detail").await;
    let app = router_with_active_application(fixture.state, None, Router::new());

    for uri in [
        "/api/plugins/analytics/agents",
        "/api/plugins/analytics/hints",
        "/api/plugins/analytics/usage",
        "/api/plugins/analytics/underused",
        "/api/plugins/analytics/diagnostics",
    ] {
        let (status, value) = get_json(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");

        assert_eq!(value["schema_revision"], 1, "{uri}");
        assert_eq!(value["domain_state"], "unknown", "{uri}");
        assert_eq!(value["payload"]["available"], false, "{uri}");
    }
}

#[tokio::test]
async fn unavailable_savings_is_an_enveloped_unknown_read() {
    let fixture = DashboardStateFixture::open("project.dashboard-savings-envelope").await;
    let app = router_with_active_application(fixture.state, None, Router::new());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/plugins/savings/overview")
                .body(Body::empty())
                .expect("savings overview request"),
        )
        .await
        .expect("savings overview response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("savings overview body");
    let value: Value = serde_json::from_slice(&body).expect("savings overview json");

    assert_eq!(value["schema_revision"], 1);
    assert_eq!(value["domain_state"], "unknown");
    assert_eq!(value["payload"]["savings"]["available"], false);
    assert_eq!(value["payload"]["sessions"]["available"], false);
    assert_eq!(value["payload"]["provider_usage"]["available"], false);
    let mut blocks: Vec<&str> = value["payload"]
        .as_object()
        .expect("savings overview payload object")
        .keys()
        .map(String::as_str)
        .collect();
    blocks.sort_unstable();
    assert_eq!(
        blocks,
        ["pricing", "provider_usage", "savings", "sessions"],
        "the canonical cost projection is served by /api/costs alone"
    );
}

#[test]
fn a_selected_project_answers_feedback_and_work_reads_and_nothing_else_by_post() {
    assert!(is_selected_project_event_delivery_ack(
        &Method::POST,
        "events/delivery-ack"
    ));
    assert!(!is_selected_project_event_delivery_ack(
        &Method::GET,
        "events/delivery-ack"
    ));
    for tail in [
        "feedback/get",
        "feedback/expand",
        "feedback/list",
        "feedback/proximity",
    ] {
        assert_eq!(
            selected_project_application_read(&Method::POST, tail),
            Some(SelectedProjectApplicationRead::Feedback)
        );
        assert_eq!(selected_project_application_read(&Method::GET, tail), None);
    }
    // `views` is the canonical Work projection read a selected project may
    // ask for; naming it keeps the intent legible even as the inventory
    // moves underneath the derived loop below.
    assert_eq!(
        selected_project_application_read(&Method::POST, "work/views"),
        Some(SelectedProjectApplicationRead::Work)
    );
    for operation in WorkOperation::ALL {
        if !operation.is_read_only() {
            continue;
        }
        let tail = operation
            .route_path()
            .strip_prefix("/")
            .expect("a rooted route path");
        assert_eq!(
            selected_project_application_read(&Method::POST, tail),
            Some(SelectedProjectApplicationRead::Work),
            "{tail} is a read-only Work operation a selected project may read"
        );
        assert_eq!(selected_project_application_read(&Method::GET, tail), None);
    }

    // Every Work command stays refused: a selected project is read-only
    // through this gateway.
    for operation in WorkOperation::ALL {
        if operation.is_read_only() {
            continue;
        }
        let tail = operation
            .route_path()
            .strip_prefix("/")
            .expect("a rooted route path");
        assert_eq!(
            selected_project_application_read(&Method::POST, tail),
            None,
            "{tail} must not be answerable for a selected project"
        );
    }
    assert_eq!(
        selected_project_application_read(&Method::POST, "feedback/status"),
        None
    );

    assert_eq!(
        selected_project_application_read(&Method::POST, "application/workflow/list-definitions"),
        Some(SelectedProjectApplicationRead::Workflow)
    );
    for operation in WorkflowOperation::ALL {
        let tail = operation
            .application_route_path()
            .strip_prefix("/")
            .expect("a rooted application route path");
        if operation.is_read_only() {
            assert_eq!(
                selected_project_application_read(&Method::POST, tail),
                Some(SelectedProjectApplicationRead::Workflow),
                "{tail} is a read-only Workflow operation a selected project may read"
            );
            assert_eq!(selected_project_application_read(&Method::GET, tail), None);
        } else {
            // Mutations stay refused: the gateway is read-only.
            assert_eq!(
                selected_project_application_read(&Method::POST, tail),
                None,
                "{tail} must not be answerable for a selected project"
            );
        }
    }
}

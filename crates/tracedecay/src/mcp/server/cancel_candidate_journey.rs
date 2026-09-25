//! Large-candidate cancel journey on the production `rmcp` adapter.
//!
//! A unit checkpoint inside one scan finishes cancellation, and the search
//! permit is released when the scan observes the signal. This journey starts
//! at `notifications/cancelled` and ends at that same checkpoint: the request
//! stops before the next candidate batch and the single search permit is free
//! for the next call. It runs on a corpus larger than one candidate batch and
//! checks the result the caller sees.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::ThreadId;
use std::time::Duration;

use rmcp::ServiceExt;
use serde_json::{Value, json};
use tracedecay_code_index_runtime::code_index_executor::code_index_search_executor;
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_code_index_runtime::mcp_admission::{
    CodeIndexMcpAdmissionUnavailableV1, CodeIndexMcpReadAdmissionV1, CodeIndexMcpReadGrantV1,
    CodeIndexScopeResolverV1, CodeIndexScopeUnavailableV1,
};
use tracedecay_code_index_runtime::resolved_scope_for_project;
use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::{
    AuthorizationRevision, CalibrationProfileId, ComponentRevision, DiversityPolicy, FusionProfile,
    PrincipalId, PrivacyDomainId, ProjectId, RetrievalBudget, RetrievalCursorKeyId, RetrieverKind,
    ScoreDomainCalibrationV1, ScoreDomainId,
};
use tracedecay_query::code_search::CodeIndexSearchAuthorityV1;
use tracedecay_query::retrieval::QueryAuthorityV1;
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
use tracedecay_runtime_core::config::PinnedUserDataDir;

use super::McpServer;
use super::construction::McpServerConstructionContext;
use tracedecay_project::project::TraceDecay;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;

/// One candidate batch is 128. The fixture is larger so the fourth control
/// observation, the same boundary the executor permit test uses, is the next
/// batch rather than end of input.
const CANDIDATE_FILES: usize = 160;
const NEXT_BATCH_CHECKPOINT: usize = 3;

#[derive(Clone)]
struct FixtureGrant(CodeIndexSearchAuthorityV1);

impl CodeIndexMcpReadGrantV1 for FixtureGrant {
    fn authorize(
        &self,
        _scope: &ResolvedScope,
        _authority: Option<&CodeIndexSearchAuthorityV1>,
    ) -> Result<CodeIndexSearchAuthorityV1, CodeIndexMcpAdmissionUnavailableV1> {
        Ok(self.0.clone())
    }

    fn search_authority(&self) -> CodeIndexSearchAuthorityV1 {
        self.0.clone()
    }
}

/// Pause the blocking candidate scan at one control observation so the
/// transport can deliver `notifications/cancelled` while the permit is held.
///
/// The runtime thread answers immediately. `spawn_blocking` is a different
/// thread, which is the scan. This test stays on the current-thread runtime
/// so that distinction is the thread id captured at construction.
#[derive(Clone)]
struct PausingAdmission {
    authority: CodeIndexSearchAuthorityV1,
    runtime_thread: ThreadId,
    scan_checkpoints: Arc<AtomicUsize>,
    pause_at: Arc<AtomicUsize>,
    scan_paused: Arc<tokio::sync::Notify>,
    resume: Arc<StdMutex<std::sync::mpsc::Receiver<()>>>,
    server: Arc<StdMutex<Option<Arc<McpServer>>>>,
    observed_signal: Arc<StdMutex<Option<tracedecay_contracts::CancellationSignal>>>,
}

impl CodeIndexMcpReadAdmissionV1 for PausingAdmission {
    type Grant = FixtureGrant;

    fn route_is_registered(&self) -> bool {
        if std::thread::current().id() == self.runtime_thread {
            return true;
        }
        let observation = self.scan_checkpoints.fetch_add(1, Ordering::SeqCst);
        if observation == self.pause_at.load(Ordering::SeqCst) {
            let signal = self
                .server
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(|server| {
                    server
                        .dispatch_authority
                        .cancellations()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .values()
                        .next()
                        .cloned()
                });
            if let Some(signal) = signal.clone() {
                *self
                    .observed_signal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(signal);
            }
            self.scan_paused.notify_one();
            let resume = self
                .resume
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if signal
                    .as_ref()
                    .is_some_and(tracedecay_contracts::CancellationSignal::is_cancelled)
                {
                    break;
                }
                match resume.try_recv() {
                    Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    Err(std::sync::mpsc::TryRecvError::Empty)
                        if std::time::Instant::now() < deadline =>
                    {
                        std::thread::yield_now();
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        true
    }

    fn admit_current(
        &self,
        _scope: &ResolvedScope,
    ) -> Result<Self::Grant, CodeIndexMcpAdmissionUnavailableV1> {
        Ok(FixtureGrant(self.authority.clone()))
    }
}

#[derive(Clone)]
struct FixedScopeResolver(ResolvedScope);

impl CodeIndexScopeResolverV1 for FixedScopeResolver {
    fn resolved_scope_for_project(
        &self,
        _project_root: &Path,
        _project_id: &ProjectId,
    ) -> Result<ResolvedScope, CodeIndexScopeUnavailableV1> {
        Ok(self.0.clone())
    }
}

struct MountedCorpus {
    registry: CodeIndexSchedulerRegistryV1,
    scope: ResolvedScope,
    _root: tempfile::TempDir,
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_large_candidate_search_stops_before_the_next_batch() {
    let _profile = PinnedUserDataDir::new();
    tracedecay_project::product_runtime::register_fixture_product_runtime();
    let corpus = mount_candidate_corpus().await;
    let authority = CodeIndexSearchAuthorityV1 {
        principal: PrincipalId::new("principal.cancel-journey.fixture").expect("principal"),
        authorization_revision: AuthorizationRevision::new("authorization.cancel-journey.fixture")
            .expect("authorization revision"),
    };
    let (resume_tx, admission) = pausing_admission(authority.clone());
    let executor = code_index_search_executor(
        corpus.registry.clone(),
        ProjectId::new("project.cancel-candidate-journey").expect("corpus project"),
        admission.clone(),
        FixedScopeResolver(corpus.scope.clone()),
    );
    let held = open_search_server(executor, authority).await;

    *admission
        .server
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&held.server));
    admission
        .pause_at
        .store(NEXT_BATCH_CHECKPOINT, Ordering::SeqCst);
    let pause_at = admission.pause_at.load(Ordering::SeqCst);
    drive_rmcp(&held.server, &admission, &resume_tx, pause_at).await;

    held.server.shutdown().await;
    corpus.registry.shutdown().await;
}

struct HeldServer {
    server: Arc<McpServer>,
    _project: tempfile::TempDir,
    _runtime: Arc<HostAdmissionTestRuntimeV1>,
}

async fn mount_candidate_corpus() -> MountedCorpus {
    let root = tempfile::TempDir::new().expect("corpus root");
    let corpus = root.path().join("corpus");
    let store = root.path().join("store");
    std::fs::create_dir_all(corpus.join("src")).expect("corpus source dir");
    for ordinal in 0..CANDIDATE_FILES {
        std::fs::write(
            corpus.join(format!("src/alpha_{ordinal:03}.rs")),
            format!(
                "pub fn alpha_{ordinal:03}() -> u32 {{ let value = {ordinal}; return value; }}\n"
            ),
        )
        .expect("corpus source");
    }
    git(&corpus, &["init", "-q", "-b", "main"]);
    git(
        &corpus,
        &["config", "user.email", "cancel-journey@test.invalid"],
    );
    git(&corpus, &["config", "user.name", "Cancel Journey"]);
    git(&corpus, &["config", "maintenance.auto", "false"]);
    git(&corpus, &["config", "gc.auto", "0"]);
    git(&corpus, &["config", "core.autocrlf", "false"]);
    git(&corpus, &["add", "."]);
    git(&corpus, &["commit", "-q", "-m", "candidate corpus"]);
    let corpus = corpus.canonicalize().expect("canonical corpus");
    let project_id =
        ProjectId::new("project.cancel-candidate-journey").expect("corpus project identity");
    let scope = resolved_scope_for_project(&corpus, &project_id).expect("corpus scope");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(project_id, &corpus, store)
        .await
        .expect("mount candidate corpus");
    let latest = tokio::time::timeout(Duration::from_mins(3), async {
        loop {
            if let Some(latest) = registry.latest_complete_serving_for_scope(&scope).await
                && latest.text_generation_handle().query_owners_are_ready()
            {
                return latest;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("candidate corpus did not become servable");
    registry
        .mount_query_authority(
            &corpus,
            &scope,
            journey_query_authority(latest.generation().manifest().privacy_domain.clone()),
        )
        .await
        .expect("mount query authority");
    MountedCorpus {
        registry,
        scope,
        _root: root,
    }
}

fn pausing_admission(
    authority: CodeIndexSearchAuthorityV1,
) -> (std::sync::mpsc::Sender<()>, PausingAdmission) {
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let admission = PausingAdmission {
        authority,
        runtime_thread: std::thread::current().id(),
        scan_checkpoints: Arc::new(AtomicUsize::new(0)),
        pause_at: Arc::new(AtomicUsize::new(usize::MAX)),
        scan_paused: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(StdMutex::new(resume_rx)),
        server: Arc::new(StdMutex::new(None)),
        observed_signal: Arc::new(StdMutex::new(None)),
    };
    (resume_tx, admission)
}

async fn open_search_server(
    executor: tracedecay_query::code_search::CodeIndexSearchExecutor,
    authority: CodeIndexSearchAuthorityV1,
) -> HeldServer {
    let project = tempfile::TempDir::new().expect("mcp project");
    std::fs::create_dir_all(project.path().join("src")).expect("mcp source dir");
    std::fs::write(
        project.path().join("src/lib.rs"),
        "pub fn cancel_journey_host() {}\n",
    )
    .expect("mcp source");
    git(project.path(), &["init", "-q", "-b", "main"]);
    git(
        project.path(),
        &["config", "user.email", "cancel-journey@test.invalid"],
    );
    git(project.path(), &["config", "user.name", "Cancel Journey"]);
    git(project.path(), &["add", "."]);
    git(project.path(), &["commit", "-q", "-m", "mcp host"]);
    let (cg, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        project.path(),
        "project.cancel-journey-host",
    )
    .await
    .expect("mcp host graph");
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(cg, None)
            .with_code_index_search_executor(executor)
            .with_code_index_search_authority(authority),
    )
    .await;
    HeldServer {
        server,
        _project: project,
        _runtime: runtime,
    }
}

async fn drive_rmcp(
    server: &Arc<McpServer>,
    admission: &PausingAdmission,
    _resume_tx: &std::sync::mpsc::Sender<()>,
    pause_at: usize,
) {
    let adapter = tracedecay_mcp::server::RmcpConnectionAdapter::new(
        super::connection::ProductionMcpConnectionContext::with_activity(Arc::clone(server), None),
        false,
        None,
        server.delivery_settlement_recorder.clone(),
    )
    .expect("RMCP adapter");
    let (server_io, client_io) = tokio::io::duplex(2 * 1024 * 1024);
    let client_messages = Arc::new(StdMutex::new(Vec::new()));
    let serving = tokio::spawn(async move {
        let running = adapter
            .serve(
                rmcp::transport::IntoTransport::<rmcp::RoleServer, _, _>::into_transport(server_io),
            )
            .await
            .expect("serve RMCP");
        running.waiting().await.expect("RMCP server task");
    });
    let mut client = ()
        .serve(RecordingTransport::new(
            rmcp::transport::IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_io),
            Arc::clone(&client_messages),
        ))
        .await
        .expect("initialize RMCP client");
    let peer = client.peer().clone();
    let arguments = search_call(11, 8)["params"]["arguments"]
        .as_object()
        .expect("search arguments")
        .clone();
    {
        let mut call = std::pin::pin!(client.call_tool(
            rmcp::model::CallToolRequestParams::new("tracedecay_search").with_arguments(arguments),
        ));
        tokio::select! {
            () = wait_for_batch_pause(admission, "rmcp") => {}
            result = &mut call => panic!("rmcp: search settled before the batch checkpoint: {result:?}"),
        }
        let request_id = tools_call_request_id(&client_messages);
        peer.notify_cancelled(rmcp::model::CancelledNotificationParam::new(
            Some(request_id),
            Some("stop before the next candidate batch".to_owned()),
        ))
        .await
        .expect("send RMCP cancellation");
        wait_until_observed_signal_cancels(admission, "rmcp").await;
        let cancelled = call
            .await
            .expect_err("rmcp cancelled search must be an error");
        assert!(
            matches!(
                cancelled,
                rmcp::service::ServiceError::Cancelled { reason: Some(ref reason) }
                    if reason == "stop before the next candidate batch"
            ),
            "rmcp client must observe the cancellation it sent: {cancelled}"
        );
        assert_scan_stopped(admission, pause_at, "rmcp");
        let follow_up = client
            .call_tool(
                rmcp::model::CallToolRequestParams::new("tracedecay_search").with_arguments(
                    search_call(12, 1)["params"]["arguments"]
                        .as_object()
                        .expect("follow-up arguments")
                        .clone(),
                ),
            )
            .await
            .expect("rmcp follow-up search");
        let text = follow_up.content[0].as_text().map_or_else(
            || panic!("rmcp follow-up text: {follow_up:?}"),
            |text| text.text.as_str(),
        );
        let payload: Value = serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("rmcp follow-up JSON ({error}): {text}"));
        assert_admitted_payload(&payload, "rmcp");
    }
    client.close().await.expect("close RMCP client");
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("RMCP connection did not close")
        .expect("join RMCP connection");
}

struct RecordingTransport<T> {
    inner: T,
    messages: Arc<StdMutex<Vec<Value>>>,
}

impl<T> RecordingTransport<T> {
    fn new(inner: T, messages: Arc<StdMutex<Vec<Value>>>) -> Self {
        Self { inner, messages }
    }
}

impl<R, T> rmcp::transport::Transport<R> for RecordingTransport<T>
where
    R: rmcp::service::ServiceRole,
    T: rmcp::transport::Transport<R> + 'static,
    rmcp::service::TxJsonRpcMessage<R>: serde::Serialize,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<R>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        if let Ok(encoded) = serde_json::to_value(&item) {
            self.messages
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(encoded);
        }
        self.inner.send(item)
    }

    fn receive(
        &mut self,
    ) -> impl std::future::Future<Output = Option<rmcp::service::RxJsonRpcMessage<R>>> + Send {
        self.inner.receive()
    }

    fn close(&mut self) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

fn tools_call_request_id(messages: &StdMutex<Vec<Value>>) -> rmcp::model::RequestId {
    let messages = messages
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let request = messages
        .iter()
        .rev()
        .find(|message| {
            message["method"] == "tools/call" || message["request"]["method"] == "tools/call"
        })
        .unwrap_or_else(|| panic!("RMCP client did not record tools/call: {messages:?}"));
    let id = request
        .get("id")
        .or_else(|| request.get("request").and_then(|request| request.get("id")))
        .cloned()
        .unwrap_or_else(|| panic!("tools/call has no id: {request}"));
    if let Some(text) = id.as_str() {
        rmcp::model::RequestId::String(std::sync::Arc::from(text))
    } else if let Some(number) = id.as_i64() {
        rmcp::model::RequestId::Number(number)
    } else if let Some(number) = id.as_u64() {
        rmcp::model::RequestId::Number(i64::try_from(number).expect("request id fits i64"))
    } else {
        panic!("unsupported RMCP request id: {id}");
    }
}

async fn wait_until_observed_signal_cancels(admission: &PausingAdmission, label: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let cancelled = admission
                .observed_signal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(tracedecay_contracts::CancellationSignal::is_cancelled);
            if cancelled {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label}: the transport cancel did not reach the in-flight scan"));
}

async fn wait_for_batch_pause(admission: &PausingAdmission, label: &str) {
    tokio::time::timeout(Duration::from_mins(2), admission.scan_paused.notified())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{label}: the candidate scan never reached its batch checkpoint while holding the permit"
            )
        });
}

fn assert_scan_stopped(admission: &PausingAdmission, pause_at: usize, label: &str) {
    assert_eq!(
        admission.scan_checkpoints.load(Ordering::SeqCst),
        pause_at.saturating_add(1),
        "{label}: the scan must stop at the checkpoint where cancellation was observed \
         and must not start the next candidate batch"
    );
}

fn assert_admitted_payload(payload: &Value, label: &str) {
    assert_ne!(
        payload["reason"],
        json!("cancelled"),
        "{label}: the follow-up search must not inherit the previous cancellation: {payload}"
    );
    assert_ne!(
        payload["reason"],
        json!("search_capacity_unavailable"),
        "{label}: the cancelled scan must release the search permit: {payload}"
    );
    assert!(
        payload["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "{label}: the next search must return a candidate from the corpus: {payload}"
    );
}

fn search_call(id: u64, limit: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_search",
            "arguments": {
                "query": "\"return value\"",
                "limit": limit,
                "format": "json"
            }
        }
    })
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn journey_query_authority(privacy_domain: PrivacyDomainId) -> Arc<QueryAuthorityV1> {
    let name = |value: &str| value.to_owned();
    let profile = FusionProfile {
        profile_id: name("profile.cancel-journey.fixture")
            .try_into()
            .expect("profile id"),
        evaluation_result_anchor: name("evaluation.cancel-journey.fixture")
            .try_into()
            .expect("evaluation anchor"),
        calibrations: RetrieverKind::QUERY_FALLBACK_LANES
            .into_iter()
            .map(|lane| {
                (
                    lane,
                    CalibrationProfileId::new(format!(
                        "calibration.{}.cancel-journey.fixture",
                        lane.as_str()
                    ))
                    .expect("calibration id"),
                )
            })
            .collect(),
        score_domain_calibrations: [
            (
                RetrieverKind::ExactLiteral,
                tracedecay_query::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1,
            ),
            (
                RetrieverKind::Lexical,
                tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1,
            ),
            (
                RetrieverKind::Graph,
                tracedecay_query::retrieval::QUERY_GRAPH_SCORE_DOMAIN_V1,
            ),
        ]
        .into_iter()
        .map(|(lane, domain)| {
            let score_domain = ScoreDomainId::new(domain).expect("score domain id");
            (
                score_domain.clone(),
                ScoreDomainCalibrationV1 {
                    calibration_profile_id: CalibrationProfileId::new(format!(
                        "calibration.{}.cancel-journey.fixture",
                        lane.as_str()
                    ))
                    .expect("calibration id"),
                    score_domain,
                    raw_min_micros: 0,
                    raw_max_micros: 1_000_000,
                },
            )
        })
        .collect(),
        minimum_calibrated_feature_micros: BTreeMap::new(),
        weights_micros: [
            (RetrieverKind::ExactLiteral, 1_000_000),
            (RetrieverKind::Lexical, 500_000),
            (RetrieverKind::Graph, 250_000),
        ]
        .into_iter()
        .collect(),
        diversity_policy_id: name("diversity.cancel-journey.fixture")
            .try_into()
            .expect("diversity id"),
        retrieval_budget: RetrievalBudget {
            max_candidates_per_lane: 32,
            max_fused_candidates: 32,
            max_hydrated_results: 32,
            max_hydration_bytes: 32 * 65_536,
            deadline_micros: None,
        },
    };
    let diversity = DiversityPolicy {
        policy_id: profile.diversity_policy_id.clone(),
        evaluation_result_anchor: Some(profile.evaluation_result_anchor.clone()),
        per_source_namespace: None,
        per_source_instance: None,
        per_repository: None,
        per_file: None,
        per_session_or_thread: None,
        per_copy_cluster: None,
        per_evidence_role: None,
    };
    let keyring = RetrievalCursorKeyringV1::new(
        privacy_domain,
        RetrievalCursorKeyId::new("retrieval-key.cancel-journey.fixture").expect("cursor key id"),
        1,
        vec![7_u8; 32],
        1_000_000,
    )
    .expect("cursor keyring");
    Arc::new(
        QueryAuthorityV1::new(
            profile,
            diversity,
            ComponentRevision::new("ranking.cancel-journey.fixture").expect("ranking revision"),
            keyring,
        )
        .expect("query authority"),
    )
}

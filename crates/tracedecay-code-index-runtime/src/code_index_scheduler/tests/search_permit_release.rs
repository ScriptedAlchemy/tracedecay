//! The single code-index search execution permit follows request settlement,
//! not the blocking worker's natural completion.
//!
//! `MAX_CONCURRENT_CODE_INDEX_SEARCHES` is one, so a request that its caller
//! has already abandoned must release the permit as soon as its lexical scan
//! observes the request control — otherwise every following search fails
//! `search_capacity_unavailable` until the abandoned scan hydrates the rest of
//! its candidate corpus.

use super::*;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::ThreadId;

use tracedecay_contracts::CancellationSignal;
use tracedecay_query::code_search::{
    CodeIndexSearchAuthorityV1, CodeIndexSearchExecutor, CodeIndexSearchModeV1,
    CodeIndexSearchOutcomeV1, CodeIndexSearchRequestV1, CodeIndexSearchUnavailableReasonV1,
};

use crate::code_index_executor::code_index_search_executor;
use crate::mcp_admission::{
    CodeIndexMcpAdmissionUnavailableV1, CodeIndexMcpReadAdmissionV1, CodeIndexMcpReadGrantV1,
    CodeIndexScopeResolverV1, CodeIndexScopeUnavailableV1,
};

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

/// Read admission whose route check is the test seam into the running scan.
///
/// The executor's request control consults `route_is_registered` on every
/// `is_cancelled` check. Calls from the test's runtime thread (the pre-permit
/// check and the executor's settlement poll) answer immediately; the first
/// call from any other thread is the scan itself, running under
/// `spawn_blocking` with the execution permit already held. That call reports
/// the pause to the test and blocks until the test resumes it, so the test
/// can observe the permit-held state and cancel the request at a point that
/// is deterministic rather than timing-dependent.
#[derive(Clone)]
struct PausingAdmission {
    authority: CodeIndexSearchAuthorityV1,
    runtime_thread: ThreadId,
    scan_checkpoints: Arc<AtomicUsize>,
    scan_paused: Arc<tokio::sync::Notify>,
    resume: Arc<StdMutex<mpsc::Receiver<()>>>,
}

impl CodeIndexMcpReadAdmissionV1 for PausingAdmission {
    type Grant = FixtureGrant;

    fn route_is_registered(&self) -> bool {
        if std::thread::current().id() == self.runtime_thread {
            return true;
        }
        if self.scan_checkpoints.fetch_add(1, Ordering::SeqCst) == 0 {
            self.scan_paused.notify_one();
            self.resume
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
                .expect("the test resumes the paused scan");
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

/// Read admission that never pauses: the request this fixture drives is
/// abandoned inside generation resolution, where no control checkpoint runs at
/// all, so a checkpoint seam would never be reached.
#[derive(Clone)]
struct OpenAdmission(CodeIndexSearchAuthorityV1);

impl CodeIndexMcpReadAdmissionV1 for OpenAdmission {
    type Grant = FixtureGrant;

    fn route_is_registered(&self) -> bool {
        true
    }

    fn admit_current(
        &self,
        _scope: &ResolvedScope,
    ) -> Result<Self::Grant, CodeIndexMcpAdmissionUnavailableV1> {
        Ok(FixtureGrant(self.0.clone()))
    }
}

/// Scope resolution is the last thing an admitted search does before it takes
/// the execution permit, and nothing between the two awaits. Reporting from
/// here therefore lets the test observe the request only once the permit is
/// held, without polling for it.
#[derive(Clone)]
struct AdmittingScopeResolver {
    scope: ResolvedScope,
    admitted: Arc<tokio::sync::Notify>,
}

impl CodeIndexScopeResolverV1 for AdmittingScopeResolver {
    fn resolved_scope_for_project(
        &self,
        _project_root: &Path,
        _project_id: &ProjectId,
    ) -> Result<ResolvedScope, CodeIndexScopeUnavailableV1> {
        self.admitted.notify_one();
        Ok(self.scope.clone())
    }
}

fn search_request(
    project_root: &Path,
    cancellation: Option<CancellationSignal>,
) -> CodeIndexSearchRequestV1 {
    CodeIndexSearchRequestV1 {
        project_root: project_root.to_path_buf(),
        query: "alpha".to_owned(),
        source_revision: None,
        source_tree: None,
        source_reference: None,
        limit: 8,
        cursor: None,
        mode: CodeIndexSearchModeV1::FallbackAllowed,
        lexical_routing: LexicalRoutingV1::query_only(),
        authority: None,
        deadline: None,
        cancellation,
    }
}

fn unavailable_reason(
    outcome: &CodeIndexSearchOutcomeV1,
) -> Option<CodeIndexSearchUnavailableReasonV1> {
    match outcome {
        CodeIndexSearchOutcomeV1::Unavailable(unavailable) => Some(unavailable.reason),
        CodeIndexSearchOutcomeV1::Complete(_) => None,
    }
}

/// Pause a lexical scan at its first control checkpoint after the permit is
/// acquired, prove the permit is held (a concurrent search is refused with
/// `search_capacity_unavailable`), cancel the paused request, and prove that
/// resuming it unwinds with the typed cancellation reason, performs no further
/// row checkpoints, and hands the permit to the next request, which is
/// admitted and completes normally.
#[tokio::test]
async fn cancelled_lexical_scan_releases_the_search_permit_to_the_next_request() {
    let sources = (0..16)
        .map(|ordinal| {
            (
                format!("src/alpha_{ordinal:02}.rs"),
                format!("pub fn alpha_{ordinal:02}() -> u32 {{ {ordinal} }}\n"),
            )
        })
        .collect::<Vec<_>>();
    let files = sources
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&files);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let admission = PausingAdmission {
        authority: CodeIndexSearchAuthorityV1 {
            principal: PrincipalId::new("principal.search-permit.fixture").expect("principal"),
            authorization_revision: AuthorizationRevision::new(
                "authorization.search-permit.fixture",
            )
            .expect("authorization revision"),
        },
        runtime_thread: std::thread::current().id(),
        scan_checkpoints: Arc::new(AtomicUsize::new(0)),
        scan_paused: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(StdMutex::new(resume_rx)),
    };
    let executor = code_index_search_executor(
        registry.clone(),
        test_project_id(),
        admission.clone(),
        FixedScopeResolver(scope),
    );
    let cancellation =
        CancellationSignal::active("cancellation.search-permit.fixture").expect("cancellation");

    let abandoned = tokio::spawn(executor(search_request(
        fixture.path(),
        Some(cancellation.clone()),
    )));
    // Deterministic rendezvous: the scan itself reports when it reaches its
    // first checkpoint holding the permit. The timeout only bounds a failure
    // in which the scan never consults the control while it holds the permit.
    tokio::time::timeout(Duration::from_mins(1), admission.scan_paused.notified())
        .await
        .expect("the lexical scan must consult the request control while holding the permit");

    let refused = executor(search_request(fixture.path(), None)).await;
    assert_eq!(
        unavailable_reason(&refused),
        Some(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable),
        "the paused scan owns the single execution permit: {refused:?}"
    );

    assert!(
        cancellation.cancel(tracedecay_contracts::clock::now_micros()),
        "the request cancels exactly once"
    );
    resume_tx
        .send(())
        .expect("the paused scan is waiting to resume");
    let outcome = abandoned.await.expect("executor task joins");
    assert_eq!(
        unavailable_reason(&outcome),
        Some(CodeIndexSearchUnavailableReasonV1::Cancelled),
        "a cancelled scan reports the typed cancellation state: {outcome:?}"
    );
    let checkpoints = admission.scan_checkpoints.load(Ordering::SeqCst);
    assert_eq!(
        checkpoints, 1,
        "the resumed scan observes cancellation at the checkpoint it paused in and \
         hydrates nothing further"
    );

    let admitted = executor(search_request(fixture.path(), None)).await;
    assert!(
        matches!(admitted, CodeIndexSearchOutcomeV1::Complete(_)),
        "the permit released by the cancelled scan admits the next request: {admitted:?}"
    );

    registry.shutdown().await;
}

/// Park one request on the scheduler's mounted map behind every waiter already
/// queued for it, and hand back the signals that observe and end that hold.
///
/// `tokio::sync::Mutex` serves its waiters in the order they enter the queue,
/// so "behind every current waiter" is a position, not a timing guess: the
/// returned `queued` notification and the enqueue itself happen in one poll,
/// so a task that observes `queued` cannot have run between them. From there
/// the map belongs to this hold the moment the waiter ahead of it releases,
/// and stays held until `release` is notified.
fn park_mounted_map_behind_current_waiters(
    registry: &CodeIndexSchedulerRegistryV1,
) -> (
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let queued = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mounted = Arc::clone(&registry.mounted);
    let task = tokio::spawn({
        let queued = Arc::clone(&queued);
        let release = Arc::clone(&release);
        async move {
            let held = mounted.lock_owned();
            queued.notify_one();
            let held = held.await;
            release.notified().await;
            drop(held);
        }
    });
    (queued, release, task)
}

/// One mounted worktree and a search executor over it whose scope resolution
/// reports.
///
/// Scope resolution is the last thing a request does before the ranking
/// authority read that takes the scheduler's mounted map, so the report is how
/// a deadline test observes that a request is queued for that map rather than
/// polling for it.
async fn map_reporting_search_executor(
    fixture: &GitFixture,
    store: &TempDir,
    authority: &str,
) -> (
    CodeIndexSchedulerRegistryV1,
    CodeIndexSearchExecutor,
    Arc<tokio::sync::Notify>,
) {
    let (registry, scope) = mounted_core_query_worktree(fixture, store).await;
    let admitted = Arc::new(tokio::sync::Notify::new());
    let executor = code_index_search_executor(
        registry.clone(),
        test_project_id(),
        OpenAdmission(CodeIndexSearchAuthorityV1 {
            principal: PrincipalId::new(format!("principal.search-permit.{authority}"))
                .expect("principal"),
            authorization_revision: AuthorizationRevision::new(format!(
                "authorization.search-permit.{authority}"
            ))
            .expect("authorization revision"),
        }),
        AdmittingScopeResolver {
            scope,
            admitted: Arc::clone(&admitted),
        },
    );
    (registry, executor, admitted)
}

/// A search whose dispatch deadline expires shortly after it is issued —
/// long enough to reach the park each test stages, short enough that the test
/// observes the settlement rather than the work.
fn expiring_request(project_root: &Path) -> CodeIndexSearchRequestV1 {
    let deadline = Deadline::new(UtcMicros(
        tracedecay_contracts::clock::now_micros().0 + 300_000,
    ))
    .expect("deadline");
    CodeIndexSearchRequestV1 {
        deadline: Some(deadline),
        ..search_request(project_root, None)
    }
}

/// Join a request the test abandoned mid-park and assert it reports the
/// deadline it was dispatched under.
///
/// The join is bounded an order of magnitude above that deadline, so the bound
/// is not the assertion: a request that settles on its own deadline never
/// reaches it, and one parked on `unbounded_await` names the await that
/// regressed instead of hanging the suite.
async fn abandoned_request_reports_its_deadline(
    abandoned: tokio::task::JoinHandle<CodeIndexSearchOutcomeV1>,
    unbounded_await: &str,
) {
    let outcome = tokio::time::timeout(Duration::from_secs(5), abandoned)
        .await
        .unwrap_or_else(|_| {
            panic!("a request past its deadline must stop waiting on {unbounded_await}")
        })
        .expect("executor task joins");
    assert_eq!(
        unavailable_reason(&outcome),
        Some(CodeIndexSearchUnavailableReasonV1::TimedOut),
        "the expired request reports the typed deadline state: {outcome:?}"
    );
}

/// The expired request left neither the execution permit nor the mounted map
/// held: an ordinary request issued after the staged hold ends completes.
async fn next_request_is_admitted(executor: &CodeIndexSearchExecutor, project_root: &Path) {
    let admitted = executor(search_request(project_root, None)).await;
    assert!(
        matches!(admitted, CodeIndexSearchOutcomeV1::Complete(_)),
        "the expired request released what the next request needs: {admitted:?}"
    );
}

/// A request whose dispatch deadline expires while its search is parked in
/// generation resolution must release the execution permit to the next
/// request.
///
/// Generation resolution runs with the single execution permit already held
/// and consults no control: it parks on the scheduler's mounted map and, when
/// nothing is servable, on the in-flight decode. Holding that map is exactly
/// the window a busy daemon spends there. Before the permit followed request
/// settlement, an expired request left the permit held by work nobody was
/// waiting for, and the retry its `retryable=true` refusal invited was refused
/// `search_capacity_unavailable` for as long as the abandoned scan sat there.
///
/// The map cannot simply be held for the whole test: an admitted search reads
/// its ranking authority off that same map *before* it takes the permit, so a
/// map held from the start parks the request short of the permit and deadlocks
/// any second request issued to observe it. The hold is therefore staged
/// through the map's own queue, so the request passes the pre-permit read,
/// takes the permit, and only then finds the map gone.
#[tokio::test]
async fn expired_request_parked_in_generation_resolution_releases_the_permit() {
    let fixture = GitFixture::new(&[("src/alpha.rs", "pub fn alpha() -> u32 { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, executor, admitted) =
        map_reporting_search_executor(&fixture, &store, "deadline").await;

    let admission_resolution = Arc::clone(&registry.mounted).lock_owned().await;

    // Observing the scope-resolution report proves the request is queued for
    // the map behind this hold.
    let abandoned = tokio::spawn(executor(expiring_request(fixture.path())));
    admitted.notified().await;

    // Queued behind the request, so the map is taken again the instant the
    // request's authority read releases it — the control-free window,
    // reproduced with the permit already held.
    let (map_queued, release_map, map_hold) = park_mounted_map_behind_current_waiters(&registry);
    map_queued.notified().await;
    drop(admission_resolution);

    abandoned_request_reports_its_deadline(
        abandoned,
        "generation resolution, which holds the execution permit and consults no control",
    )
    .await;

    release_map.notify_one();
    map_hold.await.expect("the mounted-map hold joins");
    next_request_is_admitted(&executor, fixture.path()).await;

    registry.shutdown().await;
}

/// A request whose dispatch deadline expires before it is ever admitted must
/// report that deadline, not wait out the daemon.
///
/// The ranking-authority read an admitted search takes before the execution
/// permit parks on the same mounted map, and it consults no control either. A
/// daemon holding that map across a mount, retire, or shutdown left a request
/// whose caller had already given up waiting there with nothing to end it: the
/// deadline it was dispatched under never became a bound on its own wait.
#[tokio::test]
async fn expired_request_parked_before_admission_reports_the_deadline() {
    let fixture = GitFixture::new(&[("src/alpha.rs", "pub fn alpha() -> u32 { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, executor, admitted) =
        map_reporting_search_executor(&fixture, &store, "admission").await;

    // Held for the whole request: the authority read this request parks in
    // runs before it is ever admitted, so nothing hands the map back.
    let held_resolution = registry.mounted.lock().await;

    let abandoned = tokio::spawn(executor(expiring_request(fixture.path())));
    admitted.notified().await;

    abandoned_request_reports_its_deadline(
        abandoned,
        "the mounted map, which pre-permit authority resolution takes with no control",
    )
    .await;

    drop(held_resolution);
    next_request_is_admitted(&executor, fixture.path()).await;

    registry.shutdown().await;
}

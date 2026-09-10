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
    CodeIndexSearchAuthorityV1, CodeIndexSearchModeV1, CodeIndexSearchOutcomeV1,
    CodeIndexSearchRequestV1, CodeIndexSearchUnavailableReasonV1,
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

/// A request whose dispatch deadline expires while its search is parked in
/// generation resolution must release the execution permit to the next
/// request.
///
/// Generation resolution is the first thing an admitted search does, it runs
/// with the single execution permit already held, and it consults no control:
/// it parks on the scheduler's mounted map and, when nothing is servable, on
/// the in-flight decode. Holding that map is exactly the window a busy daemon
/// spends there. Before the permit followed request settlement, an expired
/// request left the permit held by work nobody was waiting for, and the retry
/// its `retryable=true` refusal invited was refused
/// `search_capacity_unavailable` for as long as the abandoned scan sat there.
#[tokio::test]
async fn expired_request_parked_in_generation_resolution_releases_the_permit() {
    let fixture = GitFixture::new(&[("src/alpha.rs", "pub fn alpha() -> u32 { 0 }\n")]);
    let store = TempDir::new().expect("store root");
    let (registry, scope) = mounted_core_query_worktree(&fixture, &store).await;

    let admitted = Arc::new(tokio::sync::Notify::new());
    let executor = code_index_search_executor(
        registry.clone(),
        test_project_id(),
        OpenAdmission(CodeIndexSearchAuthorityV1 {
            principal: PrincipalId::new("principal.search-permit.deadline").expect("principal"),
            authorization_revision: AuthorizationRevision::new(
                "authorization.search-permit.deadline",
            )
            .expect("authorization revision"),
        }),
        AdmittingScopeResolver {
            scope,
            admitted: Arc::clone(&admitted),
        },
    );

    // The control-free window, reproduced: every generation resolution takes
    // this lock, and none of them checks the request control first.
    let parked_resolution = registry.mounted.lock().await;

    let deadline = Deadline::new(UtcMicros(
        tracedecay_contracts::clock::now_micros().0 + 300_000,
    ))
    .expect("deadline");
    let abandoned = tokio::spawn(executor(CodeIndexSearchRequestV1 {
        deadline: Some(deadline),
        ..search_request(fixture.path(), None)
    }));
    admitted.notified().await;

    let refused = executor(search_request(fixture.path(), None)).await;
    assert_eq!(
        unavailable_reason(&refused),
        Some(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable),
        "the parked request owns the single execution permit: {refused:?}"
    );

    let outcome = tokio::time::timeout(Duration::from_secs(5), abandoned)
        .await
        .expect(
            "a request past its deadline must stop holding the execution permit even where \
             generation resolution consults no control",
        )
        .expect("executor task joins");
    assert_eq!(
        unavailable_reason(&outcome),
        Some(CodeIndexSearchUnavailableReasonV1::TimedOut),
        "the expired request reports the typed deadline state: {outcome:?}"
    );

    drop(parked_resolution);
    let admitted_next = executor(search_request(fixture.path(), None)).await;
    assert!(
        matches!(admitted_next, CodeIndexSearchOutcomeV1::Complete(_)),
        "the permit released by the expired request admits the next request: {admitted_next:?}"
    );

    registry.shutdown().await;
}

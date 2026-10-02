//! Typed parking of deterministic contract violations observed by the
//! background worker, asserted through the real worker loop and the real
//! freshness projection.
//!
//! The pinned defect: a code-text-artifacts root that violates the
//! owner-privacy contract (for example a 0775 directory created by an older
//! binary) failed every text-projection pass with a background WARN and
//! nothing else, `status` reported "warming"/"indexing" forever while the
//! wake cadence silently retried a violation that can never fix itself. The
//! socket-directory variant of the same contract refuses fast and typed at
//! daemon bootstrap; background convergence must be just as truthful.
//!
//! Green means: every violation, including a permissive mode, surfaces as a
//! typed `parked` freshness state whose reason names the violation and is
//! never rewritten in place, while removing the violation lets the ordinary
//! wake cadence resume without a remount.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_code_index_retention::code_index_generations::{
    code_text_artifact_staging_root, scoped_code_index_store_root,
};

use tracedecay_contracts::code_index_freshness::{
    CodeGraphServingReadinessV1, CodeIndexBuildPhaseV1, CodeIndexReadinessTargetV1,
    CodeIndexReadinessWaitReadV1,
};
use tracedecay_contracts::{CallableCodeOperationKind, callable_code_operation};
use tracedecay_domain::UtcMicros;
use tracedecay_graph_query::{
    CodeGraphReadError, CodeGraphReadRequest, map_code_graph_read_runtime_error,
};

use super::super::graph_activation::{
    GRAPH_PUBLICATION_DEADLINE_REASON, injected_activation_attempt_count,
    install_injected_activation_gate, set_injected_activation_failures,
    set_injected_publication_deadline,
};
use super::super::tests::{OwnerSignals, application_context, query_authority};
use super::{CodeIndexSchedulerRegistryV1, CodeIndexSeatParkV1, CodeIndexSeatWaitV1};
use crate::project_reads::project_code_graph_projection_read_port;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

/// Ceiling on how long a test waits for the worker to reach the asserted
/// state. Nothing is asserted about elapsed time; this only stops a hung
/// worker from hanging CI.
const CONVERGENCE_DEADLINE: Duration = Duration::from_secs(30);

/// Poll spacing while waiting on the freshness projection.
const POLL_SPACING: Duration = Duration::from_millis(50);

/// More injected activation failures than any bounded test window can drain,
/// so a seat observed under this injection is never a pass that simply
/// outlasted the injection and activated for real.
const UNDRAINABLE_ACTIVATION_FAILURES: usize = 10_000;

struct Fixture {
    _root: TempDir,
    project: std::path::PathBuf,
    /// The exact durable text-artifacts root of the mounted worktree's scoped
    /// store, the directory the owner-privacy contract governs.
    artifacts_root: std::path::PathBuf,
    registry: CodeIndexSchedulerRegistryV1,
}

impl Fixture {
    /// Build the project and store on disk, poison the exact code-text
    /// artifacts root via `poison`, then mount. The mount itself drives the
    /// first reconcile pass, exactly as a daemon project open does.
    async fn mount_with_poisoned_artifacts_root(
        project_id: &str,
        poison: impl FnOnce(&Path),
    ) -> Self {
        let (fixture, admission) =
            Self::mount_with_poisoned_artifacts_root_held(project_id, poison).await;
        drop(admission);
        fixture
    }

    /// [`Self::mount_with_poisoned_artifacts_root`] with the background
    /// worker held at its dequeue point: the first pass starts only when the
    /// returned permit is dropped, so a test can arm observation hooks that
    /// need the mounted worktree's identity first.
    async fn mount_with_poisoned_artifacts_root_held(
        project_id: &str,
        poison: impl FnOnce(&Path),
    ) -> (Self, tokio::sync::OwnedSemaphorePermit) {
        let root = TempDir::new().expect("fixture root");
        let project = root.path().join("project");
        fs::create_dir_all(project.join("src")).expect("create source root");
        fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("write source");
        run_git_in(&project, &["init", "-q", "-b", "main"]);
        run_git_in(&project, &["add", "."]);
        run_git_in(&project, &["commit", "-qm", "fixture"]);

        // Pre-create the scoped store hierarchy owner-private, exactly as the
        // daemon would have on an earlier run, so the only violation in play
        // is the one `poison` plants on the artifacts root itself.
        let store = root.path().join("store");
        tracedecay_private_fs::create_private_directory(&store).expect("create store root");
        let canonical_project =
            canonical_existing_identity(&project).expect("canonical project root");
        let scoped = scoped_code_index_store_root(&store, &canonical_project);
        tracedecay_private_fs::create_private_directory(&scoped).expect("create scoped root");
        let artifacts_root = code_text_artifact_staging_root(&scoped);
        poison(&artifacts_root);

        let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
        let admission = registry
            .background_reconcile_admission()
            .acquire_owned()
            .await
            .expect("hold the background worker before its first pass");
        registry
            .mount_worktree(
                tracedecay_domain::ProjectId::new(project_id).expect("project identity"),
                &project,
                store.clone(),
            )
            .await
            .expect("mount scheduler");

        (
            Self {
                _root: root,
                project,
                artifacts_root,
                registry,
            },
            admission,
        )
    }

    /// One wake that carries no new input, exactly like the periodic cadence
    /// traffic a live daemon produces over an unchanged checkout.
    async fn wake_without_new_input(&self) {
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        if let Some(worktree) = mounted.get(&canonical) {
            worktree.wake.notify_one();
        }
    }

    /// Poll the real freshness projection until `accept` returns true, waking
    /// the worker between observations so a parked pass keeps re-checking on
    /// its ordinary cadence. Returns the last observed freshness.
    async fn wait_for_freshness(
        &self,
        accept: impl Fn(
            &tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1,
        ) -> bool,
    ) -> Option<tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1> {
        let deadline = tokio::time::Instant::now() + CONVERGENCE_DEADLINE;
        let mut last = None;
        while tokio::time::Instant::now() < deadline {
            if let Some(freshness) = self.registry.dashboard_freshness(&self.project).await {
                let accepted = accept(&freshness);
                last = Some(freshness);
                if accepted {
                    return last;
                }
            }
            self.wake_without_new_input().await;
            tokio::time::sleep(POLL_SPACING).await;
        }
        last
    }

    /// Poll the real serving slot until something is seated, waking the
    /// worker between observations exactly as the periodic cadence does.
    async fn wait_for_seated_generation(
        &self,
    ) -> Option<std::sync::Arc<super::super::CodeIndexPublishedGenerationV1>> {
        let deadline = tokio::time::Instant::now() + CONVERGENCE_DEADLINE;
        loop {
            let seated = self
                .registry
                .serving_code_scope(&self.project)
                .await
                .and_then(|scope| scope.serving_generation);
            if seated.is_some() || tokio::time::Instant::now() >= deadline {
                return seated;
            }
            self.wake_without_new_input().await;
            tokio::time::sleep(POLL_SPACING).await;
        }
    }
}

fn assert_seated_fixture_generation(seated: &super::super::CodeIndexPublishedGenerationV1) {
    assert_eq!(seated.snapshot().files.len(), 1);
    assert_eq!(seated.snapshot().files[0].logical_path, "src/main.rs");
    let symbols: Vec<(&str, &str)> = seated
        .symbols()
        .symbols
        .iter()
        .map(|symbol| (symbol.simple_name.as_str(), symbol.kind.as_str()))
        .collect();
    assert_eq!(symbols, [("main", "function")]);
}

/// A permissive artifacts root violates the owner-privacy contract; the
/// worker never re-permissions it. It parks typed, leaves the mode exactly as
/// found, and resumes on the ordinary wake cadence once the operator restores
/// owner-only access.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_permissive_text_artifacts_root_parks_typed_without_rewriting_its_mode() {
    let fixture = Fixture::mount_with_poisoned_artifacts_root(
        "project.text-artifacts-root-permissive",
        |artifacts_root| {
            fs::create_dir_all(artifacts_root).expect("create artifacts root");
            fs::set_permissions(artifacts_root, fs::Permissions::from_mode(0o775))
                .expect("loosen artifacts root");
        },
    )
    .await;

    let parked = fixture
        .wait_for_freshness(|freshness| freshness.parked.is_some())
        .await
        .expect("freshness projection for the mounted worktree");
    assert_eq!(
        parked.staleness_state,
        Some(tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Parked),
        "a permissive root must park instead of serving: {parked:?}"
    );
    let park = parked.parked.as_ref().expect("typed parked state");
    assert!(
        park.reason.contains("code text artifacts root") && park.reason.contains("mode 775"),
        "the parked reason must name the violated contract and observed mode: {}",
        park.reason
    );
    let mode = |root: &Path| {
        fs::metadata(root)
            .expect("artifacts root metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(
        mode(&fixture.artifacts_root),
        0o775,
        "the worker must not rewrite a root it did not create"
    );

    fs::set_permissions(&fixture.artifacts_root, fs::Permissions::from_mode(0o700))
        .expect("operator restores owner-only access");
    let recovered = fixture
        .wait_for_freshness(|freshness| {
            freshness.parked.is_none()
                && freshness.staleness_state
                    == Some(
                        tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Fresh,
                    )
        })
        .await
        .expect("freshness projection for the mounted worktree");
    assert!(
        recovered.parked.is_none(),
        "the park must clear once the operator fixes the mode: {recovered:?}"
    );

    fixture.registry.shutdown().await;
}

/// A foreign regular file squatting on the artifacts-root path must park
/// typed: the freshness
/// projection names the exact violation and remediation instead of reporting
/// "indexing" (surfaced as "warming") forever. Removing the violation lets
/// the ordinary wake cadence resume without a remount, proving parked is
/// visible-but-recoverable rather than permanently dead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_squatted_text_artifacts_root_parks_typed_and_recovers_when_fixed() {
    let fixture = Fixture::mount_with_poisoned_artifacts_root(
        "project.text-artifacts-root-typed-park",
        |artifacts_root| {
            fs::write(artifacts_root, b"squatter").expect("occupy artifacts root path");
        },
    )
    .await;

    let parked = fixture
        .wait_for_freshness(|freshness| freshness.parked.is_some())
        .await
        .expect("freshness projection for the mounted worktree");

    let park = parked.parked.as_ref().unwrap_or_else(|| {
        panic!(
            "a deterministic contract violation must surface a typed parked state \
             instead of indefinite warming; last observation: {parked:?}"
        )
    });
    assert_eq!(
        parked.staleness_state,
        Some(tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Parked),
        "status must report parked, not indexing/warming: {parked:?}"
    );
    assert!(
        park.reason.contains("code text artifacts root"),
        "the parked reason must name the violated contract: {}",
        park.reason
    );
    assert!(
        !park.remediation.is_empty(),
        "the parked state must carry operator remediation"
    );
    assert!(
        park.parked_at_micros > 0,
        "the parked state must stamp when the violation was first observed"
    );
    assert!(
        park.retries_on_wake,
        "a filesystem contract violation re-checks on every ordinary wake"
    );

    // The operator removes the violation. The next ordinary wake must pick it
    // up: parked is a visible state, not a terminal one.
    fs::remove_file(&fixture.artifacts_root).expect("remove squatter");

    let recovered = fixture
        .wait_for_freshness(|freshness| {
            freshness.parked.is_none() && freshness.staleness_state == Some(tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Fresh)
        })
        .await
        .expect("freshness projection for the mounted worktree");

    assert!(
        recovered.parked.is_none(),
        "the park must clear once the violation is removed: {recovered:?}"
    );
    assert_eq!(
        recovered.staleness_state,
        Some(tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Fresh),
        "convergence must resume on the ordinary wake cadence after the fix: {recovered:?}"
    );
    let mode = fs::metadata(&fixture.artifacts_root)
        .expect("artifacts root metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700, "the recovered root is created owner-private");

    fixture.registry.shutdown().await;
}

/// A fresh graph publication and its text projection are both corpus-sized
/// consumers of the sealed generation. Starting graph work while text is
/// parked can hold the source text needs, cross the process RSS watermark,
/// and then prevent text from reacquiring its reservation indefinitely. A
/// text owner parked on an invalid artifacts root makes the required
/// ordering observable: fresh graph activation must not start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_graph_activation_waits_while_the_published_text_owner_is_parked() {
    let (fixture, admission) = Fixture::mount_with_poisoned_artifacts_root_held(
        "project.text-artifacts-root-graph-ahead",
        |artifacts_root| {
            fs::write(artifacts_root, b"squatter").expect("occupy artifacts root path");
        },
    )
    .await;
    let scope = fixture
        .registry
        .serving_code_scope(&fixture.project)
        .await
        .expect("mounted scope");
    let gate = install_injected_activation_gate(&scope.worktree_id);
    drop(admission);

    let parked = fixture
        .wait_for_freshness(|freshness| freshness.parked.is_some())
        .await
        .expect("freshness projection for the mounted worktree");
    assert_eq!(
        parked.staleness_state,
        Some(tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Parked),
        "the text owner must park on the invalid root: {parked:?}"
    );

    assert!(
        tokio::time::timeout(Duration::from_millis(250), gate.wait_until_started())
            .await
            .is_err(),
        "fresh graph activation must not overlap a parked text projection"
    );
    let observed = fixture
        .registry
        .dashboard_freshness(&fixture.project)
        .await
        .expect("freshness projection for the mounted worktree");
    assert_eq!(
        observed.staleness_state,
        Some(tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Parked),
        "waiting graph activation must not unpark an owner that has not finished: {observed:?}"
    );
    assert!(
        fixture
            .registry
            .serving_code_scope(&fixture.project)
            .await
            .expect("mounted scope")
            .serving_generation
            .is_none(),
        "the seat still waits for a ready text owner"
    );
    fixture.registry.shutdown().await;
}

/// A publication's graph activation overlaps its own text projection once
/// that projection has opened its build: text readiness never waits on graph
/// activation, and the serving swap still waits for both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_graph_activation_never_delays_the_published_text_owner() {
    let (fixture, admission) =
        Fixture::mount_with_poisoned_artifacts_root_held("project.graph-beside-text", |_| {}).await;
    let scope = fixture
        .registry
        .serving_code_scope(&fixture.project)
        .await
        .expect("mounted scope");
    let gate = install_injected_activation_gate(&scope.worktree_id);
    drop(admission);

    tokio::time::timeout(CONVERGENCE_DEADLINE, gate.wait_until_started())
        .await
        .expect("fresh graph activation starts beside the opened text projection");
    let canonical = canonical_existing_identity(&fixture.project).expect("canonical project");
    let text = {
        let mounted = fixture.registry.mounted.lock().await;
        mounted
            .get(&canonical)
            .expect("mounted worktree")
            .text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    .expect("the publication installed its text owner before activation");
    let mut signals = OwnerSignals::subscribe(&fixture.registry, &fixture.project).await;
    tokio::time::timeout(CONVERGENCE_DEADLINE, async {
        while !text.query_owners_are_ready() {
            signals.changed().await;
        }
    })
    .await
    .expect("the text owner finishes while graph activation is held");
    assert!(
        fixture
            .registry
            .serving_code_scope(&fixture.project)
            .await
            .expect("mounted scope")
            .serving_generation
            .is_none(),
        "the seat waits for the held graph activation"
    );
    let mut signals = OwnerSignals::subscribe(&fixture.registry, &fixture.project).await;
    gate.release();
    tokio::time::timeout(CONVERGENCE_DEADLINE, async {
        while fixture
            .registry
            .serving_code_scope(&fixture.project)
            .await
            .expect("mounted scope")
            .serving_generation
            .is_none()
        {
            signals.changed().await;
        }
    })
    .await
    .expect("the publication seats once graph activation and text are done");
    fixture.registry.shutdown().await;
}

/// Exact and lexical serving does not depend on native graph. A retryable
/// activation failure used to replace the whole prepared triple with
/// `Ok((Err, None, None))`, which failed the serving swap's own guard, so the
/// sealed generation never reached the slot and search kept the predecessor
/// for the entire activation backoff. Under an activation that keeps failing
/// retryably, that is starvation: no pass ever seats.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn text_seats_while_graph_activation_keeps_failing_retryably() {
    let (fixture, admission) =
        Fixture::mount_with_poisoned_artifacts_root_held("project.seat-through-retry", |_| {})
            .await;
    let scope = fixture
        .registry
        .serving_code_scope(&fixture.project)
        .await
        .expect("mounted scope");
    // Injected unavailable-runtime failures are the retryable class, and they
    // carry no conflict verdict, so every attempt takes the retry arm rather
    // than falling through to the terminal one that already keeps the seat.
    set_injected_activation_failures(&scope.worktree_id, UNDRAINABLE_ACTIVATION_FAILURES);
    drop(admission);

    let seated = fixture
        .wait_for_seated_generation()
        .await
        .expect("the sealed generation must take the serving seat while graph activation retries");
    // Retries stay armed, so the attempt counter is already past 1 on some
    // observations. The seated file and symbol are the stable outcome.
    assert_seated_fixture_generation(&seated);
    fixture.registry.shutdown().await;
}

/// A graph publication that ran out its background budget is a typed refusal
/// for that sealed generation, never a retry. The build is a pure function of
/// the sealed generation, so every retry replayed the identical corpus-sized
/// work into the same budget, and a 200k-symbol repository stayed
/// `rebuild_in_flight` with a pending graph indefinitely (#2505).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spent_graph_publication_budget_is_refused_once_and_never_replayed() {
    let (fixture, admission) =
        Fixture::mount_with_poisoned_artifacts_root_held("project.code-index-tests", |_| {}).await;
    let scope = fixture
        .registry
        .serving_code_scope(&fixture.project)
        .await
        .expect("mounted scope");
    set_injected_publication_deadline(&scope.worktree_id, true);
    drop(admission);

    let refused = CodeGraphServingReadinessV1::Refused {
        reason: GRAPH_PUBLICATION_DEADLINE_REASON.to_owned(),
    };
    let settled = fixture
        .wait_for_freshness(|freshness| {
            freshness.code_graph_serving.as_ref() == Some(&refused) && !freshness.rebuild_in_flight
        })
        .await
        .expect("freshness is observable");
    assert_eq!(settled.code_graph_serving.as_ref(), Some(&refused));
    assert!(!settled.rebuild_in_flight, "{settled:?}");
    let seated = fixture
        .wait_for_seated_generation()
        .await
        .expect("exact and lexical keep serving the generation whose graph was refused");
    assert_seated_fixture_generation(&seated);

    // Past the whole retry backoff ladder, with the worker woken throughout.
    let observe_until = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < observe_until {
        fixture.wake_without_new_input().await;
        tokio::time::sleep(POLL_SPACING).await;
    }
    assert_eq!(
        injected_activation_attempt_count(&scope.worktree_id),
        1,
        "the spent publication must not be replayed for the same sealed generation"
    );
    let after = fixture
        .registry
        .dashboard_freshness(&fixture.project)
        .await
        .expect("freshness after the retry window");
    assert_eq!(after.code_graph_serving.as_ref(), Some(&refused));

    // A graph read of that generation answers the refusal typed and
    // terminal: retrying reads the same until another generation seals.
    let operation =
        callable_code_operation(CallableCodeOperationKind::Callers).expect("callers operation");
    let context = application_context(
        &operation,
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
    );
    let port = project_code_graph_projection_read_port(
        fixture.registry.clone(),
        fixture.project.clone(),
        context.scope().clone(),
    );
    let refusal = port
        .open(CodeGraphReadRequest::from_context(&context, UtcMicros(1)))
        .await
        .expect_err("a refused generation serves no graph read");
    assert_eq!(
        refusal,
        CodeGraphReadError::Refused {
            detail: GRAPH_PUBLICATION_DEADLINE_REASON.to_owned(),
        }
    );
    let routed = map_code_graph_read_runtime_error(refusal);
    assert_eq!(
        routed
            .project_route_context()
            .map(|(code, retryable, _)| (code, retryable)),
        Some(("code-graph-refused", false))
    );
    set_injected_publication_deadline(&scope.worktree_id, false);
    fixture.registry.shutdown().await;
}

/// Text readiness is not generation readiness. While the sealing pass still
/// publishes the native graph, progress names that publication with no
/// remaining-time estimate; it reads `ready` only once the graph seated.
/// Status used to report `ready, 0 s remaining` for the whole publication
/// (#2470).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn progress_names_graph_publication_until_the_graph_seats() {
    let (fixture, admission) =
        Fixture::mount_with_poisoned_artifacts_root_held("project.graph-publication-phase", |_| {})
            .await;
    let scope = fixture
        .registry
        .serving_code_scope(&fixture.project)
        .await
        .expect("mounted scope");
    let gate = install_injected_activation_gate(&scope.worktree_id);
    drop(admission);
    tokio::time::timeout(CONVERGENCE_DEADLINE, gate.wait_until_started())
        .await
        .expect("graph activation starts");

    let text_built = |phase: CodeIndexBuildPhaseV1| {
        matches!(
            phase,
            CodeIndexBuildPhaseV1::GraphPublication | CodeIndexBuildPhaseV1::Ready
        )
    };
    let publishing = fixture
        .wait_for_freshness(|freshness| {
            freshness
                .progress
                .as_ref()
                .is_some_and(|progress| text_built(progress.phase))
        })
        .await
        .expect("freshness while graph activation is held");
    let progress = publishing.progress.as_ref().expect("progress");
    assert_eq!(
        publishing.code_graph_serving,
        Some(CodeGraphServingReadinessV1::Pending)
    );
    assert_eq!(progress.phase, CodeIndexBuildPhaseV1::GraphPublication);
    assert_eq!(progress.estimated_remaining_seconds, None);

    gate.release();
    let seated = fixture
        .wait_for_freshness(|freshness| {
            freshness.code_graph_serving == Some(CodeGraphServingReadinessV1::Ready)
                && !freshness.rebuild_in_flight
        })
        .await
        .expect("freshness after the graph seats");
    assert_eq!(
        seated.progress.as_ref().map(|progress| progress.phase),
        Some(CodeIndexBuildPhaseV1::Ready)
    );
    fixture.registry.shutdown().await;
}

/// A search waiting for a retained generation's text serving answers from the
/// text owners while that generation's graph is still being published: the
/// publication is corpus-sized work the wait neither covers nor needs. It
/// answers warming only while the query authority it also needs is missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_text_serving_wait_answers_while_the_graph_publishes() {
    let (fixture, admission) =
        Fixture::mount_with_poisoned_artifacts_root_held("project.code-index-tests", |_| {}).await;
    let scope = fixture
        .registry
        .serving_code_scope(&fixture.project)
        .await
        .expect("mounted scope");
    let gate = install_injected_activation_gate(&scope.worktree_id);
    drop(admission);
    tokio::time::timeout(CONVERGENCE_DEADLINE, gate.wait_until_started())
        .await
        .expect("graph activation starts");

    let operation =
        callable_code_operation(CallableCodeOperationKind::Callers).expect("callers operation");
    let context = application_context(
        &operation,
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
    );
    let without_authority = tokio::time::timeout(
        CONVERGENCE_DEADLINE,
        fixture.registry.wait_for_retained_text_serving(
            &fixture.project,
            context.scope(),
            Duration::from_millis(200),
        ),
    )
    .await
    .expect("the wait answers at its own budget");
    assert_eq!(without_authority, CodeIndexSeatWaitV1::Deadline);

    let text = fixture
        .registry
        .retained_text_owner_for_root(&fixture.project)
        .await
        .expect("published text owner");
    fixture
        .registry
        .mount_query_authority(
            &fixture.project,
            context.scope(),
            query_authority(text.metadata().manifest().privacy_domain.clone()),
        )
        .await
        .expect("mount query authority");
    let serving = tokio::time::timeout(
        CONVERGENCE_DEADLINE,
        fixture.registry.wait_for_retained_text_serving(
            &fixture.project,
            context.scope(),
            Duration::from_mins(10),
        ),
    )
    .await
    .expect("the wait answers while the graph publication is held");
    assert_eq!(serving, CodeIndexSeatWaitV1::Seated(()));
    assert_eq!(
        fixture
            .registry
            .dashboard_freshness(&fixture.project)
            .await
            .expect("freshness")
            .code_graph_serving,
        Some(CodeGraphServingReadinessV1::Pending),
        "the graph is still unseated when text serving answers"
    );

    gate.release();
    fixture.registry.shutdown().await;
}

/// A seat wait answers a parked worker with its park instead of waiting out
/// its budget for a seat the worker will not install. The squatted artifacts
/// root parks the published generation's text owner on every pass, so the
/// search's wait for retained text serving can never be satisfied.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seat_wait_answers_a_parked_worker_with_its_park() {
    let fixture =
        Fixture::mount_with_poisoned_artifacts_root("project.seat-wait-parked", |artifacts_root| {
            fs::write(artifacts_root, b"squatter").expect("occupy artifacts root path");
        })
        .await;
    fixture
        .wait_for_freshness(|freshness| freshness.parked.is_some())
        .await
        .expect("the worker parks on the squatted artifacts root");
    let scope = fixture
        .registry
        .serving_code_scope(&fixture.project)
        .await
        .expect("mounted scope");
    let operation =
        callable_code_operation(CallableCodeOperationKind::Callers).expect("callers operation");
    let context = application_context(&operation, scope.repository_id, scope.worktree_id);

    let waited = tokio::time::timeout(
        CONVERGENCE_DEADLINE,
        fixture.registry.wait_for_retained_text_serving(
            &fixture.project,
            context.scope(),
            CONVERGENCE_DEADLINE * 2,
        ),
    )
    .await
    .expect("a parked worker ends the wait before its budget");
    let CodeIndexSeatWaitV1::Parked(CodeIndexSeatParkV1::Convergence(park)) = waited else {
        panic!("the wait must answer with the worker's park: {waited:?}");
    };
    assert!(
        park.reason.contains("code text artifacts root"),
        "{}",
        park.reason
    );
    assert!(park.retries_on_wake);
    fixture.registry.shutdown().await;
}

/// A readiness wait (`status` `wait_for`) ends as soon as the registry is
/// cancelled: a worktree shutting down installs nothing more, so the wait
/// reports the closed registry instead of spending its budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_readiness_wait_ends_when_the_registry_is_cancelled() {
    let (fixture, _held) =
        Fixture::mount_with_poisoned_artifacts_root_held("project.seat-wait-cancelled", |_| {})
            .await;
    let registry = fixture.registry.clone();
    let project = fixture.project.clone();
    let wait = tokio::spawn(async move {
        registry
            .wait_for_readiness(
                &project,
                CodeIndexReadinessTargetV1::Fresh,
                CONVERGENCE_DEADLINE * 2,
            )
            .await
    });
    fixture.registry.cancel();
    let waited = tokio::time::timeout(CONVERGENCE_DEADLINE, wait)
        .await
        .expect("cancellation ends the wait before its budget")
        .expect("wait task")
        .expect("readiness read");
    let CodeIndexReadinessWaitReadV1::Unreachable { reason } = waited else {
        panic!("a cancelled registry must end the wait: {waited:?}");
    };
    assert_eq!(reason, "code_index_scheduler_registry_closed");
    fixture.registry.shutdown().await;
}

fn run_git_in(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

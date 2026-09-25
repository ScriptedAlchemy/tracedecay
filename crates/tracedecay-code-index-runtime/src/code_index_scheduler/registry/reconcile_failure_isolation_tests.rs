//! The background reconcile worker's failure isolation, asserted through the
//! real worker loop rather than against the policy types in isolation.
//!
//! Both defects these tests pin were only observable *at the loop*: a policy
//! object can behave perfectly while nothing consults it. Every assertion here
//! counts reconcile passes the worker actually dispatched, never elapsed time,
//! which swings run to run on a shared machine.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_contracts::ResolvedScope;

use super::super::{
    CodeIndexBuildProgressSlotStateV1, CodeIndexCadenceTriggerV1, CodeIndexDemandAdmissionV1,
    CodeIndexReconcileAdmissionV1,
    reconcile_panic_guard::{
        MAX_CONSECUTIVE_CAPACITY_RETRIES_V1, MAX_CONSECUTIVE_RECONCILE_PANICS_V1,
        ReconcileFaultInjectionV1, ReconcileFaultKindV1,
    },
};
use super::CodeIndexSchedulerRegistryV1;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

/// Wake rounds driven from outside the worker. Each stands for the ordinary
/// wake traffic a live daemon produces (cadence ticks, queries, sibling
/// activity) over input that has not changed.
const EXTERNAL_WAKE_ROUNDS: usize = 12;

/// Spacing between external wakes. `Notify::notify_one` stores a single permit,
/// so back-to-back notifies would collapse into one pass and understate the
/// unbounded-retry behaviour these tests are meant to catch.
const WAKE_ROUND_SPACING: Duration = Duration::from_millis(120);

/// Ceiling on how long a test waits for the worker to settle. Nothing is
/// asserted about this number; it only stops a hung worker from hanging CI.
const SETTLE_DEADLINE: Duration = Duration::from_secs(20);

/// Idle window that means "no pass is pending" at mount, before any policy is
/// self-scheduling anything.
const MOUNT_QUIET_WINDOW: Duration = Duration::from_millis(500);

/// Idle window that means "the worker has stopped self-scheduling". An order of
/// magnitude above the policy's own retry ceiling so a retry still queued
/// behind a loaded machine is never mistaken for termination.
const TERMINATION_QUIET_WINDOW: Duration = Duration::from_secs(3);

struct Fixture {
    _root: TempDir,
    project: std::path::PathBuf,
    registry: CodeIndexSchedulerRegistryV1,
}

impl Fixture {
    async fn mount(project_id: &str) -> Self {
        let root = TempDir::new().expect("fixture root");
        let project = root.path().join("project");
        fs::create_dir_all(project.join("src")).expect("create source root");
        fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("write source");
        run_git_in(&project, &["init", "-q", "-b", "main"]);
        run_git_in(&project, &["add", "."]);
        run_git_in(&project, &["commit", "-qm", "fixture"]);

        let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
        registry
            .mount_worktree(
                tracedecay_domain::ProjectId::new(project_id).expect("project identity"),
                &project,
                root.path().join("store"),
            )
            .await
            .expect("mount scheduler");

        let fixture = Self {
            _root: root,
            project,
            registry,
        };
        // Mount itself can drive a pass. Let the worker go quiet before a fault
        // is installed, so every pass a test counts is one the test caused.
        // This is setup, not an assertion: nothing is claimed about how long it
        // takes, only that counting starts from rest.
        fixture.settle_for(MOUNT_QUIET_WINDOW).await;
        fixture
    }

    /// Mount the same checkout and store again under a fresh registry, the
    /// way a restarted (or upgraded) daemon does. The caller has already shut
    /// the previous registry down.
    async fn remount(previous: Self, project_id: &str) -> Self {
        let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
        registry
            .mount_worktree(
                tracedecay_domain::ProjectId::new(project_id).expect("project identity"),
                &previous.project,
                previous._root.path().join("store"),
            )
            .await
            .expect("remount scheduler");
        Self {
            _root: previous._root,
            project: previous.project,
            registry,
        }
    }

    /// The durable active pointer of this checkout's scope store.
    fn active_pointer_path(&self) -> std::path::PathBuf {
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        super::super::scoped_code_index_store_root(&self._root.path().join("store"), &canonical)
            .join("active-code-generation-v1.json")
    }

    /// Block until the worker has had no pass in flight for `window`.
    async fn settle_for(&self, window: Duration) {
        let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
        let mut quiet_since: Option<tokio::time::Instant> = None;
        while tokio::time::Instant::now() < deadline {
            if self
                .registry
                .reconcile_in_progress_for_test(&self.project)
                .await
            {
                quiet_since = None;
            } else {
                match quiet_since {
                    None => quiet_since = Some(tokio::time::Instant::now()),
                    Some(since) if since.elapsed() >= window => return,
                    Some(_) => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Install the fault and hand back the shared counter of dispatched passes.
    async fn install_fault(
        &self,
        kind: ReconcileFaultKindV1,
        faulting_passes: usize,
    ) -> Arc<ReconcileFaultInjectionV1> {
        let fault = Arc::new(ReconcileFaultInjectionV1::new(kind, faulting_passes));
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        worktree
            .scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .install_reconcile_fault_for_test(Arc::clone(&fault));
        fault
    }

    /// One wake that carries no new input: the control epoch does not advance,
    /// exactly as it does not when a cadence tick or query wakes the worker
    /// over bytes nobody touched.
    async fn wake_without_new_input(&self) {
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        worktree.wake.notify_one();
    }

    /// One attributable wake with no epoch advance.
    async fn wake_with_pending_arrival(&self) {
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        CodeIndexSchedulerRegistryV1::note_wake(
            &worktree.pending_wake,
            &worktree.wake,
            CodeIndexCadenceTriggerV1::QueryAdmission,
        );
    }

    async fn pending_wake_micros(&self) -> u64 {
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        let pending = worktree
            .pending_wake
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.micros
    }

    async fn clear_build_progress(&self) {
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        *worktree
            .build_progress
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            CodeIndexBuildProgressSlotStateV1::default();
    }

    async fn clear_convergence_park_for_test(&self) {
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        *worktree
            .convergence_park
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    async fn plant_terminal_publication_park(&self, reason: &str) {
        use tracedecay_contracts::code_index_freshness::{
            CodeIndexBuildBlockedReasonV1, CodeIndexConvergenceParkedV1,
        };
        let canonical = canonical_existing_identity(&self.project).expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        *worktree
            .convergence_park
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(CodeIndexConvergenceParkedV1 {
                reason: reason.to_owned(),
                blocked_reason: Some(CodeIndexBuildBlockedReasonV1::PublicationAuthorityCorrupt),
                remediation: "run `tracedecay daemon restart`".to_owned(),
                parked_at_micros: 1,
                observed_passes: 1,
                retries_on_wake: false,
            });
    }

    /// Drive `EXTERNAL_WAKE_ROUNDS` spaced wakes over unchanged input.
    async fn drive_external_wakes(&self) {
        for _ in 0..EXTERNAL_WAKE_ROUNDS {
            self.wake_without_new_input().await;
            tokio::time::sleep(WAKE_ROUND_SPACING).await;
        }
    }
}

/// Poll until the worker has taken the pending arrival, or the deadline expires.
///
/// Zero means the wake was observed (suppressed or consumed by a pass). A
/// non-zero result means the worker never ran, so an attempts-equals-zero
/// assertion would be vacuous.
async fn wait_until_pending_wake_drained(fixture: &Fixture) -> u64 {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let micros = fixture.pending_wake_micros().await;
        if micros == 0 || tokio::time::Instant::now() >= deadline {
            return micros;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Poll until the mount reports a sealed complete generation, waking the
/// worker as a query would. Panics at the deadline: a mount that never seals
/// is the failure these tests exist to catch, not a timing artifact.
async fn wait_for_latest_generation(fixture: &Fixture) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let freshness = fixture
            .registry
            .dashboard_freshness(&fixture.project)
            .await
            .expect("mounted freshness");
        if let Some(generation) = freshness.latest_generation_id {
            return generation;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the mount never sealed a generation: {freshness:?}"
        );
        fixture.wake_with_pending_arrival().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll until `attempts` reaches `target`, or the deadline expires. Returns the
/// last observed count so the caller asserts on the count, not on the wait.
async fn wait_for_attempts(fault: &ReconcileFaultInjectionV1, target: usize) -> usize {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let seen = fault.attempts();
        if seen >= target || tokio::time::Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// FINDING 1. A reconcile unit that panics on every pass must stop being
/// retried.
///
/// The reported symptom was one malformed source file panicking the indexing
/// pool and the scheduler re-dispatching the identical unit 114 times, leaving
/// the project index permanently stale. Before the guard was wired into this
/// loop, every wake produced another attempt: the count tracked the wakes.
/// Bounded means the count stops at the policy bound however many wakes
/// arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconcile_that_panics_every_pass_stops_being_retried() {
    let fixture = Fixture::mount("project.reconcile-panic-isolation").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::Panic, usize::MAX)
        .await;

    let bound = MAX_CONSECUTIVE_RECONCILE_PANICS_V1 as usize;

    // One wake starts it. The guard's own backoff drives the retries.
    fixture.wake_without_new_input().await;
    wait_for_attempts(&fault, bound).await;

    // Now behave like a live daemon: keep waking the worker over the same
    // bytes. A wired guard suppresses every one of these.
    fixture.drive_external_wakes().await;
    let attempts = fault.attempts();

    assert!(
        attempts <= bound,
        "a panicking unit must reach a terminal state, not one retry per wake: \
         {attempts} passes after 1 + {EXTERNAL_WAKE_ROUNDS} wakes (bound {bound})"
    );
    assert!(
        attempts >= 1,
        "the first wake must actually dispatch a pass; {attempts} means the harness never ran"
    );
    assert!(
        attempts < 1 + EXTERNAL_WAKE_ROUNDS,
        "unbounded retry: attempts ({attempts}) still scale with wakes ({})",
        1 + EXTERNAL_WAKE_ROUNDS
    );

    fixture.registry.shutdown().await;
}

/// FINDING 1, other half. Quarantine must not be permanent: input that
/// actually changed advances the code-index control epoch and earns another
/// attempt, or a fixed file would never be indexed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_input_lifts_a_quarantined_reconcile() {
    let fixture = Fixture::mount("project.reconcile-panic-epoch").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::Panic, usize::MAX)
        .await;
    let bound = MAX_CONSECUTIVE_RECONCILE_PANICS_V1 as usize;

    fixture.wake_without_new_input().await;
    let quarantined_at = wait_for_attempts(&fault, bound).await;
    fixture.drive_external_wakes().await;
    assert_eq!(
        fault.attempts(),
        quarantined_at,
        "unchanged input must stay quarantined across every external wake"
    );

    // A real hook hint advances the control epoch: these are not the bytes
    // that panicked.
    assert!(
        matches!(
            fixture
                .registry
                .notify_hook_paths(&fixture.project, &["src/main.rs".to_owned()])
                .await,
            CodeIndexDemandAdmissionV1::Queued
        ),
        "the hint must reach the mounted scheduler"
    );
    let after_hint = wait_for_attempts(&fault, quarantined_at + 1).await;

    assert!(
        after_hint > quarantined_at,
        "changed input must earn another attempt; stayed at {quarantined_at}"
    );

    fixture.registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn superseded_reconcile_retains_its_arrival_until_a_later_wake() {
    let fixture = Fixture::mount("project.reconcile-superseded-arrival").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::Cancelled, usize::MAX)
        .await;

    fixture.wake_with_pending_arrival().await;
    wait_for_attempts(&fault, 1).await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        1,
        "an interrupted pass relies on the source observation's wake instead of self-retrying"
    );
    assert_ne!(
        fixture.pending_wake_micros().await,
        0,
        "the interrupted pass must restore the arrival a later source wake will consume"
    );

    let attempts_before_shutdown = fault.attempts();
    fixture.registry.shutdown().await;
    assert_eq!(
        fault.attempts(),
        attempts_before_shutdown,
        "terminal shutdown must not retry the interrupted pass"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deadline_interruption_retains_arrival_without_retrying_expired_work() {
    let fixture = Fixture::mount("project.reconcile-deadline-arrival").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::DeadlineExceeded, usize::MAX)
        .await;

    fixture.wake_with_pending_arrival().await;
    wait_for_attempts(&fault, 1).await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        1,
        "an expired reconcile must not retry the same deadline"
    );
    assert_ne!(
        fixture.pending_wake_micros().await,
        0,
        "deadline attribution must not erase the accepted arrival"
    );

    fixture.registry.shutdown().await;
}

/// FINDING 2. A reconcile refused because shared process capacity was
/// momentarily held must be retried by this worker on its own.
///
/// Nothing wakes this worktree when the competing holder releases the budget,
/// so before the retry was wired the single failing pass was the last pass:
/// the worktree stayed stale until an unrelated query or edit happened to wake
/// it. The assertion is that a second pass happens with **no** further external
/// wake, and that it then succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transient_capacity_refusal_is_retried_without_an_external_wake() {
    let fixture = Fixture::mount("project.reconcile-capacity-retry").await;
    // Refuse exactly the first pass, as a sibling holder would while it holds
    // the shared budget; every later pass finds capacity.
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::TransientCapacity, 1)
        .await;

    // Exactly one wake, and never another from outside.
    fixture.wake_without_new_input().await;
    let attempts = wait_for_attempts(&fault, 2).await;

    assert!(
        attempts >= 2,
        "a transient capacity refusal must schedule its own retry; only {attempts} pass(es) \
         ran after a single wake, so the worktree stays stale until unrelated traffic arrives"
    );

    fixture.registry.shutdown().await;
}

/// FINDING 2, guard rail. The retry must not become the bug it fixes: a
/// capacity refusal that never clears is bounded, not retried forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capacity_refusal_that_never_clears_is_bounded() {
    let fixture = Fixture::mount("project.reconcile-capacity-bound").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::TransientCapacity, usize::MAX)
        .await;
    let bound = 1 + MAX_CONSECUTIVE_CAPACITY_RETRIES_V1 as usize;

    fixture.wake_without_new_input().await;
    // Sample the chain at its policy bound rather than after a quiet window:
    // `settle_for` gives its deadline up silently, so on a loaded machine a
    // retry still queued was sampled as the terminal count and the comparison
    // below read its arrival as a re-arm.
    wait_for_attempts(&fault, bound).await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;
    let settled = fault.attempts();
    // The decisive property: self-scheduling has *stopped*. An unbounded retry
    // keeps producing passes here however long the wait.
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        settled,
        "self-scheduled capacity retries must terminate, not keep re-arming"
    );
    assert!(
        settled <= bound,
        "self-scheduled capacity retries must respect the policy bound: \
         {settled} passes from one wake (bound {bound})"
    );
    assert!(
        settled >= 2,
        "the bound must still allow at least one retry; saw {settled}"
    );

    fixture.registry.shutdown().await;
}

/// FINDING 2, the distinction that matters most. A refusal that *is* a
/// resident-memory admission failure but can never be admitted, the request
/// alone exceeds the whole process limit, must not be self-retried. No other
/// holder releasing anything makes it fit, so retrying it is the unbounded
/// retry this PR exists to remove, wearing a capacity label.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capacity_refusal_that_can_never_fit_is_not_retried() {
    let fixture = Fixture::mount("project.reconcile-oversized-capacity").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::OversizedCapacity, usize::MAX)
        .await;

    fixture.wake_without_new_input().await;
    wait_for_attempts(&fault, 1).await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        1,
        "an admission failure whose request exceeds the process limit must not \
         be treated as transient capacity"
    );

    fixture.registry.shutdown().await;
}

/// FINDING 2, guard rail. A refusal the same input reproduces forever must
/// **not** be self-retried either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_permanent_refusal_is_never_self_retried() {
    let fixture = Fixture::mount("project.reconcile-permanent-refusal").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::Permanent, usize::MAX)
        .await;

    fixture.wake_without_new_input().await;
    wait_for_attempts(&fault, 1).await;
    // Any self-scheduled retry would land inside the quiet window.
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        1,
        "a permanent refusal must not schedule its own retry"
    );

    fixture.registry.shutdown().await;
}

/// The upgrade journey behind issue #1979, driven through the real worker.
/// The daemon mounts cold over a store whose pointer a pre-beta.38 release
/// sealed; its digest no longer matches the re-serialized entries. The worker
/// must delete the derived store and rebuild it from source with no operator
/// action, and status must never show a terminal park.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pre_segment_bytes_pointer_is_reset_and_rebuilt_by_the_worker() {
    let fixture = Fixture::mount("project.reconcile-upgrade-pointer-reset").await;
    let sealed = wait_for_latest_generation(&fixture).await;
    let pointer_path = fixture.active_pointer_path();
    assert!(
        pointer_path.exists(),
        "the initial build must publish a durable pointer for {sealed}"
    );
    fixture.registry.shutdown().await;
    super::super::tests::downgrade_pointer_to_pre_segment_bytes_shape(&pointer_path);

    let upgraded = Fixture::remount(fixture, "project.reconcile-upgrade-pointer-reset").await;
    let rebuilt = wait_for_latest_generation(&upgraded).await;
    let freshness = upgraded
        .registry
        .dashboard_freshness(&upgraded.project)
        .await
        .expect("mounted freshness");
    assert!(
        freshness.parked.is_none(),
        "a corrupt derived publication is rebuilt, never parked: {freshness:?}"
    );
    let wire = serde_json::to_value(&freshness).expect("freshness wire");
    assert!(
        wire["progress"]["blocked_reason"].is_null(),
        "status must not carry a terminal reason after the rebuild: {wire}"
    );
    assert!(
        matches!(
            upgraded
                .registry
                .notify_hook_overflow(&upgraded.project)
                .await,
            CodeIndexDemandAdmissionV1::Queued
        ),
        "hooks are admitted again on the rebuilt store"
    );
    let pointer: tracedecay_code_index_retention::code_index_generations::DurablePublicationPointerV1 =
        serde_json::from_slice(&fs::read(&pointer_path).expect("rebuilt pointer"))
            .expect("rebuilt pointer decodes");
    assert_eq!(pointer.generation_id, rebuilt);
    assert!(
        pointer.generation_index[0].segment_bytes > 0,
        "the rebuilt pointer is in the current shape"
    );
    upgraded.registry.shutdown().await;
}

/// A corrupt publication authority gets exactly one automatic reset and
/// rebuild per mount. A store that is corrupt again after that rebuild is
/// parked; the fault here reproduces on every pass, so the second attempt is
/// the rebuild the reset scheduled and the park follows it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupt_publication_authority_resets_once_then_parks_and_reports_terminal_state() {
    let fixture = Fixture::mount("project.reconcile-publication-corruption").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::PublicationCorruption, usize::MAX)
        .await;

    fixture.wake_with_pending_arrival().await;
    wait_for_attempts(&fault, 2).await;
    fixture.drive_external_wakes().await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        2,
        "one reset buys one rebuild; a store corrupt again after it must ignore later wakes"
    );
    assert_eq!(
        fixture.pending_wake_micros().await,
        0,
        "terminal failure must consume the arrival so status is not rebuilding"
    );
    let freshness = fixture
        .registry
        .dashboard_freshness(&fixture.project)
        .await
        .expect("mounted freshness");
    assert!(
        !freshness.rebuild_in_flight,
        "terminal publication corruption is blocked, not in flight: {freshness:?}"
    );
    let wire = serde_json::to_value(&freshness).expect("freshness wire");
    assert_eq!(
        wire["parked"]["blocked_reason"], "publication_authority_corrupt",
        "status must carry the typed terminal reason: {wire}"
    );
    let parked = freshness.parked.expect("terminal convergence state");
    assert!(
        parked
            .reason
            .contains("injected corrupt publication authority"),
        "terminal state must retain the exact cause: {parked:?}"
    );
    assert!(
        parked.remediation.contains("`tracedecay daemon restart`"),
        "the park must name the exact operator command: {parked:?}"
    );
    assert_eq!(
        parked.blocked_reason,
        Some(
            tracedecay_contracts::code_index_freshness::CodeIndexBuildBlockedReasonV1::PublicationAuthorityCorrupt
        )
    );
    assert!(
        !parked.retries_on_wake,
        "a store corrupt again after its rebuild cannot clear on another wake"
    );
    assert!(
        matches!(
            fixture
                .registry
                .notify_hook_overflow(&fixture.project)
                .await,
            CodeIndexDemandAdmissionV1::Terminal(_)
        ),
        "the mounted scheduler must return the terminal state until reset"
    );

    fixture.registry.shutdown().await;
}

/// A terminal park planted without a worker-observed failure must still stop
/// the loop. Admission already refuses that slot; a stack flag the observing
/// pass alone sets would keep dispatching reconcile against it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planted_terminal_publication_park_suppresses_worker_reconcile() {
    let fixture = Fixture::mount("project.reconcile-planted-terminal-park").await;
    fixture
        .plant_terminal_publication_park("planted before any reconcile")
        .await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::PublicationCorruption, usize::MAX)
        .await;

    fixture.wake_with_pending_arrival().await;
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    while fixture.pending_wake_micros().await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "planted terminal park did not drain the pending arrival"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    fixture.drive_external_wakes().await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        0,
        "the worker must read the shared PublicationAuthorityCorrupt park, not a local flag"
    );
    assert_eq!(
        fixture.pending_wake_micros().await,
        0,
        "terminal suppression must leave no rebuild arrival"
    );
    assert!(
        matches!(
            fixture
                .registry
                .notify_hook_overflow(&fixture.project)
                .await,
            CodeIndexDemandAdmissionV1::Terminal(_)
        ),
        "admission and the worker must refuse the same planted park"
    );

    fixture.registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupt_publication_without_build_progress_returns_terminal_admission() {
    let fixture = Fixture::mount("project.reconcile-cold-publication-corruption").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::PublicationCorruption, usize::MAX)
        .await;

    fixture.wake_with_pending_arrival().await;
    wait_for_attempts(&fault, 2).await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;
    // Observational progress can lag or be cleared while the park remains the
    // sole terminal authority.
    fixture.clear_build_progress().await;

    assert!(matches!(
        fixture
            .registry
            .notify_hook_overflow(&fixture.project)
            .await,
        CodeIndexDemandAdmissionV1::Terminal(_)
    ));
    assert!(matches!(
        fixture
            .registry
            .notify_hook_paths(&fixture.project, &["src/main.rs".to_owned()])
            .await,
        CodeIndexDemandAdmissionV1::Terminal(_)
    ));
    fixture.registry.shutdown().await;
}

/// The typed park is the mount's publication-authority, not a bool this worker
/// latches after it personally observes the error. A park already present,
/// planted by admission, a previous owner, or a test of that contract, must
/// stop the loop before it dispatches another reconcile.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_publication_park_stops_the_worker_without_a_local_latch() {
    let fixture = Fixture::mount("project.reconcile-park-is-worker-authority").await;
    fixture
        .plant_terminal_publication_park("store already corrupt before this worker observed it")
        .await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::PublicationCorruption, usize::MAX)
        .await;

    fixture.wake_with_pending_arrival().await;
    assert_eq!(
        wait_until_pending_wake_drained(&fixture).await,
        0,
        "the worker must observe the terminal wake before the assertion"
    );
    fixture.drive_external_wakes().await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        0,
        "a terminal publication park must stop the worker; a private bool misses a park this loop has not yet observed"
    );
    assert_eq!(
        fixture.pending_wake_micros().await,
        0,
        "terminal suppression must consume the arrival so status is not rebuilding"
    );
    let freshness = fixture
        .registry
        .dashboard_freshness(&fixture.project)
        .await
        .expect("mounted freshness");
    assert!(
        !freshness.rebuild_in_flight,
        "a pre-existing terminal park is blocked, not in flight: {freshness:?}"
    );
    let parked = freshness.parked.expect("terminal convergence state");
    assert_eq!(
        parked.blocked_reason,
        Some(
            tracedecay_contracts::code_index_freshness::CodeIndexBuildBlockedReasonV1::PublicationAuthorityCorrupt
        )
    );
    assert_eq!(
        parked.observed_passes, 1,
        "the worker must not re-observe a park it did not cause"
    );
    assert!(
        !parked.retries_on_wake,
        "stopping the worker must not rewrite the park into a retryable failure"
    );

    // The loop must re-read the slot. A worker-local bool would stay set
    // after this clear and keep the next wake suppressed.
    fixture.clear_convergence_park_for_test().await;
    fixture.wake_with_pending_arrival().await;
    let seen = wait_for_attempts(&fault, 1).await;
    assert!(
        seen >= 1,
        "clearing the typed park must admit the worker again; a sticky latch would not"
    );

    fixture.registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn park_visible_before_progress_reason_returns_terminal_admission() {
    let fixture = Fixture::mount("project.reconcile-park-before-progress").await;
    let scope = {
        let canonical = canonical_existing_identity(&fixture.project).expect("canonical project");
        let mounted = fixture.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        ResolvedScope::new(
            worktree.project_id.clone(),
            worktree.repository_id.clone(),
            worktree.worktree_id.clone(),
            None,
        )
        .expect("resolved scope")
    };
    fixture
        .plant_terminal_publication_park("parked before progress snapshot")
        .await;
    fixture.clear_build_progress().await;

    assert!(matches!(
        fixture
            .registry
            .notify_hook_overflow(&fixture.project)
            .await,
        CodeIndexDemandAdmissionV1::Terminal(_)
    ));
    assert!(matches!(
        fixture
            .registry
            .notify_hook_paths(&fixture.project, &["src/main.rs".to_owned()])
            .await,
        CodeIndexDemandAdmissionV1::Terminal(_)
    ));
    assert!(matches!(
        fixture
            .registry
            .request_query_background_reconcile(&scope)
            .await,
        CodeIndexReconcileAdmissionV1::PublicationAuthorityCorrupt(_)
    ));
    fixture.registry.shutdown().await;
}

/// An ordinary read's freshness probe is the quietest path to the scheduler,
/// and the one most likely to be treated as retryable. A cold terminal park
/// must reach it as `Terminal` so a read never reports a resettable index as
/// merely stale.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cold_terminal_park_makes_the_freshness_probe_terminal() {
    let fixture = Fixture::mount("project.reconcile-park-freshness-probe").await;
    fixture
        .plant_terminal_publication_park("parked before any freshness probe")
        .await;
    fixture.clear_build_progress().await;

    assert!(matches!(
        fixture
            .registry
            .probe_freshness_admission(&fixture.project)
            .await,
        CodeIndexDemandAdmissionV1::Terminal(_)
    ));
    let scope = {
        let mounted = fixture.registry.mounted.lock().await;
        let worktree = mounted
            .get(&canonical_existing_identity(&fixture.project).unwrap())
            .unwrap();
        ResolvedScope::new(
            worktree.project_id.clone(),
            worktree.repository_id.clone(),
            worktree.worktree_id.clone(),
            None,
        )
        .unwrap()
    };
    assert!(matches!(
        fixture
            .registry
            .request_query_background_reconcile(&scope)
            .await,
        CodeIndexReconcileAdmissionV1::PublicationAuthorityCorrupt(_)
    ));
    fixture.registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retire_and_remount_clears_terminal_publication_park_for_new_admission() {
    let fixture = Fixture::mount("project.reconcile-publication-remount").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::PublicationCorruption, usize::MAX)
        .await;
    fixture.wake_with_pending_arrival().await;
    wait_for_attempts(&fault, 2).await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;
    assert!(matches!(
        fixture
            .registry
            .notify_hook_overflow(&fixture.project)
            .await,
        CodeIndexDemandAdmissionV1::Terminal(_)
    ));

    let mut roots = std::collections::BTreeSet::new();
    roots.insert(canonical_existing_identity(&fixture.project).expect("canonical project"));
    assert!(
        fixture.registry.retire_project_roots(&roots).await,
        "retire must drain the terminal owner"
    );
    fixture
        .registry
        .mount_worktree(
            tracedecay_domain::ProjectId::new("project.reconcile-publication-remount")
                .expect("project identity"),
            &fixture.project,
            fixture._root.path().join("store"),
        )
        .await
        .expect("remount over a fresh owner without the injected fault");

    assert!(
        matches!(
            fixture
                .registry
                .notify_hook_overflow(&fixture.project)
                .await,
            CodeIndexDemandAdmissionV1::Queued
        ),
        "retire/remount must admit work again on a repaired mount"
    );
    assert!(
        matches!(
            fixture
                .registry
                .notify_hook_paths(&fixture.project, &["src/main.rs".to_owned()])
                .await,
            CodeIndexDemandAdmissionV1::Queued
        ),
        "exact-path hooks must recover with the remounted owner"
    );
    fixture.registry.shutdown().await;
}

/// A park written by an actor other than this worker must still suppress
/// the loop. The old worker-local bool stayed false here, so later wakes
/// reconciled against a terminal publication authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planted_terminal_publication_park_suppresses_later_wakes() {
    let fixture = Fixture::mount("project.reconcile-planted-publication-park").await;
    let fault = fixture
        .install_fault(ReconcileFaultKindV1::Permanent, usize::MAX)
        .await;
    fixture
        .plant_terminal_publication_park("planted by an actor other than the worker")
        .await;

    fixture.wake_with_pending_arrival().await;
    fixture.drive_external_wakes().await;
    fixture.settle_for(TERMINATION_QUIET_WINDOW).await;

    assert_eq!(
        fault.attempts(),
        0,
        "a shared terminal park must stop the worker without a task-local latch"
    );
    assert_eq!(
        fixture.pending_wake_micros().await,
        0,
        "the suppressed wake must drain the pending arrival"
    );
    let freshness = fixture
        .registry
        .dashboard_freshness(&fixture.project)
        .await
        .expect("mounted freshness");
    assert!(
        !freshness.rebuild_in_flight,
        "a terminal park is not a rebuild: {freshness:?}"
    );
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

//! The background reconcile worker's failure isolation, asserted through the
//! real worker loop rather than against the policy types in isolation.
//!
//! Both defects these tests pin were only observable *at the loop*: a policy
//! object can behave perfectly while nothing consults it. Every assertion here
//! counts reconcile passes the worker actually dispatched — never elapsed time,
//! which swings run to run on a shared machine.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use super::super::reconcile_panic_guard::{
    MAX_CONSECUTIVE_CAPACITY_RETRIES_V1, MAX_CONSECUTIVE_RECONCILE_PANICS_V1,
    ReconcileFaultInjectionV1, ReconcileFaultKindV1,
};
use super::CodeIndexSchedulerRegistryV1;

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
                None,
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
        let canonical = self.project.canonicalize().expect("canonical project");
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
        let canonical = self.project.canonicalize().expect("canonical project");
        let mounted = self.registry.mounted.lock().await;
        let worktree = mounted.get(&canonical).expect("mounted worktree");
        worktree.wake.notify_one();
    }

    /// Drive `EXTERNAL_WAKE_ROUNDS` spaced wakes over unchanged input.
    async fn drive_external_wakes(&self) {
        for _ in 0..EXTERNAL_WAKE_ROUNDS {
            self.wake_without_new_input().await;
            tokio::time::sleep(WAKE_ROUND_SPACING).await;
        }
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
        fixture
            .registry
            .notify_hook_paths(&fixture.project, &["src/main.rs".to_owned()])
            .await,
        "the hint must reach the mounted scheduler"
    );
    let after_hint = wait_for_attempts(&fault, quarantined_at + 1).await;

    assert!(
        after_hint > quarantined_at,
        "changed input must earn another attempt; stayed at {quarantined_at}"
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
/// resident-memory admission failure but can never be admitted — the request
/// alone exceeds the whole process limit — must not be self-retried. No other
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

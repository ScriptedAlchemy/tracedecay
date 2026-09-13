//! Typed parking of deterministic contract violations observed by the
//! background worker, asserted through the real worker loop and the real
//! freshness projection.
//!
//! The pinned defect: a code-text-artifacts root that violates the
//! owner-privacy contract (for example a 0775 directory created by an older
//! binary) failed every text-projection pass with a background WARN and
//! nothing else — `status` reported "warming"/"indexing" forever while the
//! wake cadence silently retried a violation that can never fix itself. The
//! socket-directory variant of the same contract refuses fast and typed at
//! daemon bootstrap; background convergence must be just as truthful.
//!
//! Green means: an owned legacy mode self-heals (with the store converging to
//! owner-private and serving), and an unhealable violation surfaces as a typed
//! `parked` freshness state whose reason names the violation — while removing
//! the violation lets the ordinary wake cadence resume without a remount.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_code_index_retention::code_index_generations::{
    code_text_artifacts_root, scoped_code_index_store_root,
};

use super::super::graph_activation::install_injected_activation_gate;
use super::CodeIndexSchedulerRegistryV1;

/// Ceiling on how long a test waits for the worker to reach the asserted
/// state. Nothing is asserted about elapsed time; this only stops a hung
/// worker from hanging CI.
const CONVERGENCE_DEADLINE: Duration = Duration::from_secs(30);

/// Poll spacing while waiting on the freshness projection.
const POLL_SPACING: Duration = Duration::from_millis(50);

struct Fixture {
    _root: TempDir,
    project: std::path::PathBuf,
    /// The exact durable text-artifacts root of the mounted worktree's scoped
    /// store — the directory the owner-privacy contract governs.
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
        let canonical_project = project.canonicalize().expect("canonical project root");
        let scoped = scoped_code_index_store_root(&store, &canonical_project);
        tracedecay_private_fs::create_private_directory(&scoped).expect("create scoped root");
        let artifacts_root = code_text_artifacts_root(&scoped);
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
                None,
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
        let canonical = self.project.canonicalize().expect("canonical project");
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
}

/// An owned legacy artifacts root with a permissive mode is exactly the state
/// older binaries left behind. Ownership is provable, so the worker heals it
/// to owner-private in place and serving converges — no operator chmod, no
/// parked state, no indefinite warming.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legacy_permissive_text_artifacts_root_self_heals_and_serves() {
    let fixture = Fixture::mount_with_poisoned_artifacts_root(
        "project.text-artifacts-root-self-heal",
        |artifacts_root| {
            fs::create_dir_all(artifacts_root).expect("create artifacts root");
            fs::set_permissions(artifacts_root, fs::Permissions::from_mode(0o775))
                .expect("loosen artifacts root");
        },
    )
    .await;

    let observed = fixture
        .wait_for_freshness(|freshness| freshness.staleness_state.as_deref() == Some("fresh"))
        .await
        .expect("freshness projection for the mounted worktree");

    assert_eq!(
        observed.staleness_state.as_deref(),
        Some("fresh"),
        "the healed store must converge to serving instead of warming forever: {observed:?}"
    );
    assert!(
        observed.parked.is_none(),
        "a healed root must not stay parked: {:?}",
        observed.parked
    );
    let mode = fs::metadata(&fixture.artifacts_root)
        .expect("artifacts root metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o700,
        "self-heal must tighten the legacy root to owner-private"
    );

    fixture.registry.shutdown().await;
}

/// A violation ownership cannot prove away — here a foreign regular file
/// squatting on the artifacts-root path — must park typed: the freshness
/// projection names the exact violation and remediation instead of reporting
/// "indexing" (surfaced as "warming") forever. Removing the violation lets
/// the ordinary wake cadence resume without a remount, proving parked is
/// visible-but-recoverable rather than permanently dead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unhealable_text_artifacts_root_parks_typed_and_recovers_when_fixed() {
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
        parked.staleness_state.as_deref(),
        Some("parked"),
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
            freshness.parked.is_none() && freshness.staleness_state.as_deref() == Some("fresh")
        })
        .await
        .expect("freshness projection for the mounted worktree");

    assert!(
        recovered.parked.is_none(),
        "the park must clear once the violation is removed: {recovered:?}"
    );
    assert_eq!(
        recovered.staleness_state.as_deref(),
        Some("fresh"),
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

/// A publication's graph activation consumes the sealed generation, not the
/// text artifact, so it must not queue behind the text owner's projection.
/// The pinned defect: the seat gate demanded a *finished* text owner before
/// any graph work began, so on a 772-file fixture the graph build (tens of
/// seconds) started only after the whole text build and their sum missed the
/// journey's restart bound. A text owner that can never finish — parked on an
/// unhealable artifacts root — makes the ordering observable: activation must
/// start while the owner is still parked. The seat itself still waits for a
/// ready owner, so the parked state stays truthful throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_activation_starts_while_the_published_text_owner_is_parked() {
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
        parked.staleness_state.as_deref(),
        Some("parked"),
        "the text owner must park on the unhealable root: {parked:?}"
    );

    tokio::time::timeout(CONVERGENCE_DEADLINE, gate.wait_until_started())
        .await
        .expect("graph activation must start without waiting for the parked text owner");
    let observed = fixture
        .registry
        .dashboard_freshness(&fixture.project)
        .await
        .expect("freshness projection for the mounted worktree");
    assert_eq!(
        observed.staleness_state.as_deref(),
        Some("parked"),
        "activation must not seat or unpark an owner that has not finished: {observed:?}"
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
    gate.release();

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

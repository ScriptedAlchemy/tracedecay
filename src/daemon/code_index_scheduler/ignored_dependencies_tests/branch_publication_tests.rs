use std::process::Command;
use std::sync::atomic::Ordering;

use tracedecay_domain::{CodeGenerationId, GitOidV1, RefId};

use super::super::branch_generations::{BranchGenerationPairV1, BranchGenerationReadControlV1};
use super::*;

struct BranchRevision {
    reference: RefId,
    revision: GitOidV1,
    tree: GitOidV1,
}

struct PauseAfterCandidatePublication {
    candidate_is_published: Arc<dyn Fn() -> bool + Send + Sync>,
    paused: std::sync::atomic::AtomicBool,
    paused_sender: std::sync::Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl CodeIndexExecutionControlV1 for PauseAfterCandidatePublication {
    fn is_cancelled(&self) -> bool {
        if !self.paused.load(Ordering::Acquire)
            && (self.candidate_is_published)()
            && self
                .paused
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            self.paused_sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("candidate publication pause sender")
                .send(())
                .expect("signal candidate publication pause");
            self.release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
                .expect("release candidate publication pause");
        }
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

fn git_output(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new(crate::git::git_program())
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "Git fixture command failed: {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_owned()
}

fn current_branch_revision(root: &Path, reference: &str) -> BranchRevision {
    BranchRevision {
        reference: RefId::new(reference).expect("valid branch reference"),
        revision: GitOidV1::new(git_output(root, &["rev-parse", "HEAD"]))
            .expect("valid commit oid"),
        tree: GitOidV1::new(git_output(root, &["rev-parse", "HEAD^{tree}"]))
            .expect("valid tree oid"),
    }
}

fn add_comparison_branch(fixture: &GitFixture) -> BranchRevision {
    git(fixture.path(), &["checkout", "-qb", "comparison"]);
    fixture.write(
        "src/app.ts",
        br#"import type { PublicWidget } from "pkg";
export function ComparisonAnchor(value: PublicWidget) { return value; }
"#,
    );
    git(fixture.path(), &["add", "src/app.ts"]);
    git(fixture.path(), &["commit", "-qm", "comparison branch"]);
    let comparison = current_branch_revision(fixture.path(), "refs/heads/comparison");
    git(fixture.path(), &["checkout", "-q", "main"]);
    comparison
}

async fn branch_pair(
    registry: &CodeIndexSchedulerRegistryV1,
    request: &CodeIndexIgnoredDependencyRequestV1,
    base: &BranchRevision,
    head: &BranchRevision,
) -> BranchGenerationPairV1 {
    registry
        .generations_for_revisions(
            &request.scope,
            &base.reference,
            &base.revision,
            &base.tree,
            &head.reference,
            &head.revision,
            &head.tree,
            BranchGenerationReadControlV1 {
                deadline: None,
                cancellation: None,
            },
        )
        .await
        .expect("materialize and compare exact branch generations")
}

async fn active_publication_generation(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> CodeGenerationId {
    let scheduler = registry
        .scheduler_handle(project_root)
        .await
        .expect("mounted scheduler");
    let scheduler = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pointer = scheduler
        .publication
        .read_publication_pointer()
        .expect("read canonical publication pointer")
        .expect("active canonical publication");
    CodeGenerationId::new(pointer.generation_id).expect("valid active generation id")
}

async fn wait_for_active_publication_change(
    registry: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    incumbent: &CodeGenerationId,
) -> CodeGenerationId {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let active = active_publication_generation(registry, project_root).await;
            if &active != incumbent {
                break active;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("candidate publication becomes durable")
}

fn dependency_file(latest: &LatestCompleteCodeIndexV1) -> tracedecay_domain::SanitizedCodeFileV1 {
    latest
        .generation()
        .snapshot()
        .files
        .iter()
        .find(|file| file.logical_path == "node_modules/pkg/index.d.ts")
        .expect("admitted dependency snapshot row")
        .clone()
}

fn assert_dependency_bytes(
    latest: &LatestCompleteCodeIndexV1,
    expected: &tracedecay_domain::SanitizedCodeFileV1,
) {
    let actual = dependency_file(latest);
    assert_eq!(actual.logical_path, expected.logical_path);
    assert_eq!(actual.content_digest, expected.content_digest);
    let chunks = latest
        .generation()
        .admitted_chunks()
        .expect("re-admit exact dependency chunks");
    assert!(
        chunks.iter().any(|admitted| {
            let chunk = admitted.chunk();
            chunk.anchor.file_occurrence_id == actual.file_occurrence_id
                && chunk.sanitized_text.as_str().contains("PublicWidget")
        }),
        "the retained generation contains the admitted dependency bytes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_materialization_then_reconcile_and_restart_retains_the_exact_roster() {
    let fixture = GitFixture::new(
        r#"import type { PublicWidget } from "pkg";
export function GenerationAnchor(value: PublicWidget) { return value; }
"#,
    );
    let comparison = add_comparison_branch(&fixture);
    let main = current_branch_revision(fixture.path(), "refs/heads/main");
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface PublicWidget { value: string }\n",
    );
    let expected_bytes = std::fs::read(fixture.path().join("node_modules/pkg/index.d.ts"))
        .expect("dependency bytes");
    let store = TempDir::new().expect("store root");
    let registry = mount(fixture.path(), &store, 1).await;
    let baseline = latest(&registry, fixture.path()).await;
    let request = request_for(&baseline, "pkg");
    index_dependency(
        &registry,
        fixture.path(),
        request.clone(),
        StaticControl::active(),
    )
    .await
    .expect("admit ignored dependency");
    let admitted = latest(&registry, fixture.path()).await;
    let admitted_roster = admitted.generation().ignored_source_admissions().to_vec();
    let admitted_file = dependency_file(&admitted);
    let admitted_head = admitted.generation().manifest().generation_id.clone();

    let pair = branch_pair(&registry, &request, &main, &comparison).await;
    assert!(
        pair.head
            .generation()
            .ignored_source_admissions()
            .is_empty()
    );
    assert_eq!(
        active_publication_generation(&registry, fixture.path()).await,
        admitted_head,
        "branch comparison retains exact history without replacing the roster-bearing active head"
    );

    reconcile_through_worker(&registry, fixture.path(), &request.scope).await;
    let reconciled = latest(&registry, fixture.path()).await;
    assert_eq!(
        reconciled.generation().ignored_source_admissions(),
        admitted_roster,
        "branch comparison cannot erase the serving roster during ordinary reconcile"
    );
    assert_dependency_bytes(&reconciled, &admitted_file);
    assert_eq!(
        std::fs::read(fixture.path().join("node_modules/pkg/index.d.ts"))
            .expect("dependency bytes after reconcile"),
        expected_bytes
    );
    let reconciled_head = reconciled.generation().manifest().generation_id.clone();

    registry.shutdown().await;
    let restarted = mount(fixture.path(), &store, 1).await;
    let restored = latest(&restarted, fixture.path()).await;
    assert_eq!(
        restored.generation().manifest().generation_id,
        reconciled_head,
        "restart activates the exact roster-bearing serving head"
    );
    assert_eq!(
        restored.generation().ignored_source_admissions(),
        admitted_roster
    );
    assert_dependency_bytes(&restored, &admitted_file);
    restarted.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn competing_reconcile_during_graph_activation_preserves_the_incumbent() {
    let fixture = GitFixture::new(
        r#"import type { PublicWidget } from "pkg";
export function GenerationAnchor(value: PublicWidget) { return value; }
"#,
    );
    write_types_package(
        fixture.path(),
        "pkg",
        "export interface PublicWidget { value: string }\n",
    );
    let store = TempDir::new().expect("store root");
    let profile = TempDir::new().expect("profile root");
    let profile_root = profile.path().join("profile");
    crate::storage::write_enrollment_marker(
        fixture.path(),
        &crate::storage::EnrollmentMarker {
            project_id: PROJECT_ID.to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("project enrollment");
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _database_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 92, "ignored-dependency-race")
            .expect("daemon database scope");
    let graph_runtime = Arc::new(
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await
        .expect("graph runtime registry"),
    );
    let project_database = graph_runtime
        .project_memory(project_id(), [fixture.path().to_path_buf()])
        .await
        .expect("project graph database");
    let registry = Arc::new(CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1));
    registry
        .mount_worktree_with_graph_runtime(
            project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            None,
            Arc::clone(&graph_runtime),
            Arc::clone(&project_database),
        )
        .await
        .expect("mount persistent graph-backed scheduler");
    wait_for_initial_generation(&registry, fixture.path()).await;
    let baseline = latest(&registry, fixture.path()).await;
    let incumbent = baseline.generation().manifest().generation_id.clone();
    let request = request_for(&baseline, "pkg");
    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let publication_scheduler = Arc::clone(&scheduler);
    let publication_incumbent = incumbent.clone();
    let (paused_tx, paused_rx) = std::sync::mpsc::sync_channel(1);
    let (release_activation_tx, release_activation_rx) = std::sync::mpsc::sync_channel(1);
    let control: Arc<dyn CodeIndexExecutionControlV1 + Send + Sync> =
        Arc::new(PauseAfterCandidatePublication {
            candidate_is_published: Arc::new(move || {
                let scheduler = publication_scheduler
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                scheduler
                    .publication
                    .read_publication_pointer()
                    .ok()
                    .flatten()
                    .is_some_and(|pointer| pointer.generation_id != publication_incumbent.as_str())
            }),
            paused: std::sync::atomic::AtomicBool::new(false),
            paused_sender: std::sync::Mutex::new(Some(paused_tx)),
            release: std::sync::Mutex::new(release_activation_rx),
        });

    let owner_registry = Arc::clone(&registry);
    let owner_root = fixture.path().to_path_buf();
    let owner_request = request.clone();
    let owner = tokio::spawn(async move {
        index_dependency(&owner_registry, &owner_root, owner_request, control).await
    });
    paused_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("admission pauses after publishing its candidate");
    let candidate = wait_for_active_publication_change(&registry, fixture.path(), &incumbent).await;

    fixture.write(
        "src/app.ts",
        br#"import type { PublicWidget } from "pkg";
export function CompetingTrackedEdit(value: PublicWidget) { return value.value; }
"#,
    );
    tokio::task::spawn_blocking(move || {
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reconcile_now()
            .expect("publish competing ordinary reconcile")
    })
    .await
    .expect("competing reconcile task joins");
    let competitor = active_publication_generation(&registry, fixture.path()).await;
    assert_ne!(competitor, candidate);

    let reconcile_admission = registry.background_reconcile_admission();
    let mut worker_hold = Box::pin(reconcile_admission.acquire_owned());
    tokio::select! {
        biased;
        permit = &mut worker_hold => {
            drop(permit.expect("background reconcile admission remains open"));
            panic!("ignored admission must still own the sole reconcile permit");
        }
        () = tokio::task::yield_now() => {}
    }
    release_activation_tx
        .send(())
        .expect("release candidate graph activation");

    let error = tokio::time::timeout(Duration::from_secs(5), owner)
        .await
        .expect("admission terminates after graph activation is released")
        .expect("admission task joins")
        .expect_err("superseded activation cannot publish serving state");
    assert_refusal(error, CodeIndexIgnoredDependencyRefusalV1::StaleGeneration);
    let worker_hold = worker_hold
        .await
        .expect("hold the authoritative follow-up reconcile");
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(incumbent.clone()),
        "a candidate superseded in the canonical store cannot replace the incumbent serving head"
    );
    assert_eq!(
        active_publication_generation(&registry, fixture.path()).await,
        competitor,
        "the stale admission cannot overwrite the competing manifest and snapshot"
    );

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let scheduler_hold = std::thread::spawn(move || {
        let _scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler hold");
        release_rx.recv().expect("release scheduler hold");
    });
    held_rx.recv().expect("follow-up scheduler is held");
    drop(worker_hold);
    wait_for_reconciling(&registry, 1).await;
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(incumbent),
        "the scheduled follow-up cannot expose the stale admission candidate while blocked"
    );
    release_tx.send(()).expect("release follow-up scheduler");
    scheduler_hold
        .join()
        .expect("follow-up scheduler holder joins");
    wait_for_reconciling(&registry, 0).await;
    let converged = latest(&registry, fixture.path()).await;
    assert_eq!(
        converged.generation().manifest().generation_id,
        competitor,
        "the real worker converges serving to the canonical competing generation"
    );
    assert_ne!(
        converged.generation().manifest().generation_id,
        candidate,
        "stale candidate N is never installed as serving state"
    );
    registry.shutdown().await;
}

use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{Sender, channel},
};
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::{Mutex as AsyncMutex, mpsc::UnboundedReceiver};
use tracedecay_application::lsp_runtime::LspCodeIndexProjectionIdentityPort;
use tracedecay_application::semantic_runtime::{
    SavedCodeGenerationScheduleHookV1, SavedGenerationScheduleOutcomeV1,
};
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::CodeGenerationId;

use super::super::CodeIndexGenerationPublishedV1;
use super::{
    CodeIndexSchedulerRegistryV1, GitFixture, ResolvedScope, test_project_id,
    wait_for_generation_change, wait_for_initial_generation, wait_for_live_complete_generation,
    wait_for_quiescent_owner_pass,
};
use crate::code_index_scheduler::CodeGraphActivationPolicyV1;
use crate::code_index_scheduler::registry::SemanticEvaluationGenerationRefusalV1;

async fn published_generation_for_root(
    publications: &mut tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1>,
    project_root: &Path,
) -> CodeGenerationId {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = publications.recv().await.expect("generation publication");
            if event.project_root.as_path() == project_root {
                break event.generation_id;
            }
        }
    })
    .await
    .expect("matching project generation publication")
}

/// Ordered log of the generation ids a semantic hook was offered.
///
/// Semantic delivery is at-least-once per serving generation: no-op
/// verification passes and remount wakes re-offer the already-serving
/// generation so a lost enqueue is retried, and the production hook dedupes
/// downstream. Tests therefore assert on which generations a hook observed,
/// never on exact call counts.
type SemanticDeliveryLogV1 = Arc<Mutex<Vec<CodeGenerationId>>>;

fn recording_semantic_hook(
    deliveries: &SemanticDeliveryLogV1,
) -> SavedCodeGenerationScheduleHookV1 {
    let deliveries = Arc::clone(deliveries);
    Arc::new(move |generation: Arc<CodeIndexPublishedGenerationV1>| {
        deliveries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(generation.manifest().generation_id.clone());
        SavedGenerationScheduleOutcomeV1::Scheduled
    })
}

fn delivered_generations(deliveries: &SemanticDeliveryLogV1) -> Vec<CodeGenerationId> {
    deliveries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

async fn wait_for_semantic_delivery(
    deliveries: &SemanticDeliveryLogV1,
    generation: &CodeGenerationId,
) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !delivered_generations(deliveries).contains(generation) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("semantic hook received the generation");
}

#[tokio::test]
async fn remount_replaces_semantic_hook_and_replays_latest_generation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let first_deliveries: SemanticDeliveryLogV1 = Arc::new(Mutex::new(Vec::new()));
    let first_hook = recording_semantic_hook(&first_deliveries);
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(first_hook),
            )
            .await
            .expect("mount scheduler")
    );
    let first_generation = wait_for_initial_generation(&registry, fixture.path()).await;
    wait_for_semantic_delivery(&first_deliveries, &first_generation).await;

    let second_deliveries: SemanticDeliveryLogV1 = Arc::new(Mutex::new(Vec::new()));
    let second_hook = recording_semantic_hook(&second_deliveries);
    assert!(
        !registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(second_hook),
            )
            .await
            .expect("remount scheduler")
    );
    // Remount replays the already-published generation to the replacement
    // hook without requiring a new edit.
    wait_for_semantic_delivery(&second_deliveries, &first_generation).await;
    // The hook swap happens under the scheduler mutex, so once remount
    // returns no offer to the retired hook can still be in flight.
    let retired_deliveries = delivered_generations(&first_deliveries);
    assert!(
        retired_deliveries
            .iter()
            .all(|generation| generation == &first_generation),
        "retired hook saw only the generation published during its tenure"
    );

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let second_generation =
        wait_for_generation_change(&registry, fixture.path(), &first_generation).await;
    wait_for_semantic_delivery(&second_deliveries, &second_generation).await;
    assert_eq!(
        delivered_generations(&first_deliveries),
        retired_deliveries,
        "retired hook must not receive later generations"
    );
    assert!(
        !registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("remount without semantics")
    );
    // Snapshot after the clearing remount returns: at-least-once re-offers of
    // `second_generation` may land up to the hook swap, but never after it.
    let disabled_deliveries = delivered_generations(&second_deliveries);
    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 3 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    let third_generation =
        wait_for_generation_change(&registry, fixture.path(), &second_generation).await;
    assert_eq!(
        delivered_generations(&second_deliveries),
        disabled_deliveries,
        "remount without a semantic runtime must clear the stale hook"
    );
    assert!(
        !disabled_deliveries.contains(&third_generation),
        "no hook may observe a generation published after semantics were cleared"
    );
    registry.shutdown().await;
}

#[tokio::test]
async fn semantic_schedule_reuses_the_serving_generation_handle() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let scheduled_generation = Arc::new(Mutex::new(None));
    let semantic_hook = {
        let scheduled_generation = Arc::clone(&scheduled_generation);
        Arc::new(move |generation: Arc<CodeIndexPublishedGenerationV1>| {
            *scheduled_generation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(generation);
            SavedGenerationScheduleOutcomeV1::Scheduled
        }) as SavedCodeGenerationScheduleHookV1
    };
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(semantic_hook),
            )
            .await
            .expect("mount scheduler")
    );
    wait_for_live_complete_generation(&registry, fixture.path()).await;

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let serving_generation = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .latest_complete()
        .expect("serving generation")
        .generation_handle();
    let scheduled_generation = scheduled_generation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .expect("scheduled generation");

    assert!(
        Arc::ptr_eq(&scheduled_generation, &serving_generation),
        "semantic scheduling must share the immutable serving generation instead of deep-cloning it"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_schedule_can_retry_the_serving_generation_after_lifecycle_selection() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let lifecycle_ready = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(AtomicUsize::new(0));
    let scheduled_generation = Arc::new(Mutex::new(None));
    let semantic_hook = {
        let lifecycle_ready = Arc::clone(&lifecycle_ready);
        let attempts = Arc::clone(&attempts);
        let scheduled_generation = Arc::clone(&scheduled_generation);
        Arc::new(move |generation: Arc<CodeIndexPublishedGenerationV1>| {
            attempts.fetch_add(1, Ordering::AcqRel);
            if !lifecycle_ready.load(Ordering::Acquire) {
                return SavedGenerationScheduleOutcomeV1::QueueRefused;
            }
            *scheduled_generation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(generation);
            SavedGenerationScheduleOutcomeV1::Scheduled
        }) as SavedCodeGenerationScheduleHookV1
    };
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(semantic_hook),
            )
            .await
            .expect("mount scheduler")
    );
    wait_for_live_complete_generation(&registry, fixture.path()).await;
    // The serving seat is published from inside the pass that also runs the
    // semantic schedule hook, so the seat alone does not prove the hook was
    // reached. Let that pass finish before reading its attempt count.
    wait_for_quiescent_owner_pass(&registry, fixture.path()).await;
    let attempts_before_selection = attempts.load(Ordering::Acquire);
    assert!(
        attempts_before_selection > 0,
        "the serving generation must have reached the not-yet-selected lifecycle"
    );

    lifecycle_ready.store(true, Ordering::Release);
    assert_eq!(
        registry
            .reschedule_semantic_generation(fixture.path())
            .await,
        SavedGenerationScheduleOutcomeV1::Scheduled,
        "selection completion must re-offer the already-serving generation"
    );

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let serving_generation = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .latest_complete()
        .expect("serving generation")
        .generation_handle();
    let scheduled_generation = scheduled_generation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .expect("lifecycle-ready schedule");
    assert_eq!(
        attempts.load(Ordering::Acquire),
        attempts_before_selection + 1,
        "selection completion must trigger exactly one bounded retry"
    );
    assert!(Arc::ptr_eq(&scheduled_generation, &serving_generation));
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_partitioned_generation_reaches_semantics_after_source_proof_expires() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn retained_semantic_alpha() -> u32 { 1 }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let seeded_registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        seeded_registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("seed scheduler")
    );
    let retained_generation = wait_for_live_complete_generation(&seeded_registry, fixture.path())
        .await
        .generation()
        .manifest()
        .generation_id
        .clone();
    seeded_registry.shutdown().await;
    drop(seeded_registry);

    let registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        registry
            .mount_worktree_with_graph_policy(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
                CodeGraphActivationPolicyV1::RefusedByConfiguration,
            )
            .await
            .expect("reopen retained scheduler")
    );

    let retained_text = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(text) = registry.latest_text_serving_for_root(fixture.path()).await
                && registry
                    .dashboard_freshness(fixture.path())
                    .await
                    .is_some_and(|freshness| freshness.staleness_state.as_deref() == Some("fresh"))
            {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("retained text owner becomes ready");
    let snapshot = retained_text.metadata().snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("retained scope");
    let projection_identity = LspCodeIndexProjectionIdentityPort::current_identity(
        &registry,
        fixture.path().to_path_buf(),
        None,
    )
    .await
    .expect("retained generation provides the current query identity");
    assert_eq!(
        projection_identity.code_generation_id, retained_generation,
        "query identity must use the retained generation"
    );
    assert!(
        registry
            .latest_complete_serving_for_test(fixture.path())
            .await
            .is_none(),
        "identity demand must not fabricate or install a decoded graph seat"
    );
    registry
        .expire_source_freshness_for_test(fixture.path())
        .await;
    assert_eq!(
        registry
            .dashboard_freshness(fixture.path())
            .await
            .and_then(|freshness| freshness.staleness_state),
        Some("fresh".to_owned()),
        "status retains the last verified owner while its bounded source proof ages out"
    );
    let unverified_identity = LspCodeIndexProjectionIdentityPort::current_identity(
        &registry,
        fixture.path().to_path_buf(),
        None,
    )
    .await
    .expect("an expired source proof must retain the read-only query identity");
    assert_eq!(
        unverified_identity.code_generation_id, retained_generation,
        "source verification must keep serving the retained generation"
    );
    let (candidate, code) = registry
        .semantic_evaluation_generation_for_scope(fixture.path(), &scope)
        .await
        .expect("retained generation is eligible for semantic evaluation");
    assert_eq!(candidate.source_generation, retained_generation);
    assert_eq!(code.manifest().generation_id, retained_generation);
    assert!(
        registry
            .latest_complete_serving_for_test(fixture.path())
            .await
            .is_none(),
        "semantic demand must not fabricate or install a decoded graph seat"
    );
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(retained_generation.clone()),
        "the partitioned text owner remains the serving identity"
    );
    let reconcile_admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold stale-source reconcile");
    fixture.edit(
        "src/lib.rs",
        "pub fn retained_semantic_beta() -> u32 { 2 }\n",
    );
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await,
        "source edit reaches the mounted freshness authority"
    );
    assert!(
        matches!(
            registry
                .semantic_evaluation_generation_for_scope(fixture.path(), &scope)
                .await,
            Err(SemanticEvaluationGenerationRefusalV1::SourceChanged)
        ),
        "source drift must refuse the retained semantic evaluation candidate"
    );
    let projection_identity = LspCodeIndexProjectionIdentityPort::current_identity(
        &registry,
        fixture.path().to_path_buf(),
        None,
    )
    .await
    .expect("source drift must retain the read-only query identity");
    assert_eq!(
        projection_identity.code_generation_id, retained_generation,
        "read-only query identity must keep serving the retained generation"
    );
    drop(reconcile_admission);
    registry.shutdown().await;

    let refreshed_deliveries: SemanticDeliveryLogV1 = Arc::new(Mutex::new(Vec::new()));
    let refreshed_registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        refreshed_registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(recording_semantic_hook(&refreshed_deliveries)),
            )
            .await
            .expect("reopen stale retained scheduler")
    );
    let refreshed_generation =
        wait_for_generation_change(&refreshed_registry, fixture.path(), &retained_generation).await;
    wait_for_semantic_delivery(&refreshed_deliveries, &refreshed_generation).await;
    assert!(
        !delivered_generations(&refreshed_deliveries).contains(&retained_generation),
        "the superseded retained generation must not cross semantic admission"
    );
    let wrong_generation_witness = super::super::ServingSourceWitnessV1 {
        generation_id: refreshed_generation,
    };
    assert!(
        !super::super::registry::semantic_handoff_has_exact_witness(
            true,
            Some(&wrong_generation_witness),
            &retained_generation,
        ),
        "an incumbent witness cannot authorize a discarded superseded generation"
    );
    refreshed_registry.shutdown().await;
}

struct BlockingSemanticScheduleProbeV1 {
    entered: AsyncMutex<UnboundedReceiver<Arc<CodeIndexPublishedGenerationV1>>>,
    release: Sender<()>,
    hook: SavedCodeGenerationScheduleHookV1,
}

impl BlockingSemanticScheduleProbeV1 {
    fn new() -> Self {
        let (entered_tx, entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release_tx, release_rx) = channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let hook = Arc::new(move |generation: Arc<CodeIndexPublishedGenerationV1>| {
            entered_tx
                .send(Arc::clone(&generation))
                .expect("report scheduled semantic generation");
            release_rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(3))
                .expect("release blocked semantic schedule");
            SavedGenerationScheduleOutcomeV1::Scheduled
        }) as SavedCodeGenerationScheduleHookV1;
        Self {
            entered: AsyncMutex::new(entered_rx),
            release: release_tx,
            hook,
        }
    }

    async fn entered_generation(&self) -> Arc<CodeIndexPublishedGenerationV1> {
        tokio::time::timeout(Duration::from_secs(5), self.entered.lock().await.recv())
            .await
            .expect("semantic schedule entry")
            .expect("semantic schedule authority stays open")
    }

    fn release(&self) {
        self.release
            .send(())
            .expect("release semantic schedule hook");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_schedule_runs_after_exact_generation_becomes_servable() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let mut serving_seats = registry.subscribe_serving_seats();
    let probe = BlockingSemanticScheduleProbeV1::new();
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(Arc::clone(&probe.hook)),
            )
            .await
            .expect("mount scheduler")
    );
    let mut serving_changes = registry
        .subscribe_serving_generation_changes(fixture.path())
        .await
        .expect("subscribe to advisory serving changes");
    assert!(
        registry.request_complete_generation(fixture.path()).await,
        "mounted worktree admits complete-generation demand"
    );

    tokio::time::timeout(Duration::from_secs(5), serving_seats.changed())
        .await
        .expect("serving-seat wake while semantic handoff is blocked")
        .expect("serving-seat authority stays open");
    let first_scheduled = probe.entered_generation().await;
    let first_scheduled_id = first_scheduled.manifest().generation_id.clone();
    let snapshot = first_scheduled.snapshot();
    let scope = ResolvedScope::new(
        test_project_id(),
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    tokio::time::timeout(Duration::from_secs(1), serving_changes.changed())
        .await
        .expect("advisory wake while semantic handoff is blocked")
        .expect("advisory serving authority stays open");
    let ready_while_hook_runs = registry
        .latest_complete_ready_decoded_for_root_scope(fixture.path(), &scope)
        .await
        .expect("exact ready reader advances while semantic handoff is blocked");
    let feedback_while_hook_runs = registry
        .latest_feedback_generation_for_scope(fixture.path(), &scope)
        .await
        .expect("advisory reader advances while semantic handoff is blocked");
    let first_serving_while_hook_runs = registry.latest_generation_id(fixture.path()).await;
    probe.release();
    let first_published = wait_for_initial_generation(&registry, fixture.path()).await;

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    tokio::time::timeout(Duration::from_secs(5), serving_seats.changed())
        .await
        .expect("edited serving-seat wake while semantic handoff is blocked")
        .expect("serving-seat authority stays open");
    let second_scheduled = probe.entered_generation().await;
    let second_scheduled_id = second_scheduled.manifest().generation_id.clone();
    tokio::time::timeout(Duration::from_secs(1), serving_changes.changed())
        .await
        .expect("edited advisory wake while semantic handoff is blocked")
        .expect("advisory serving authority stays open");
    let second_serving_while_hook_runs = registry.latest_generation_id(fixture.path()).await;
    probe.release();
    let second_published =
        wait_for_generation_change(&registry, fixture.path(), &first_published).await;
    registry.shutdown().await;

    assert_eq!(first_scheduled_id, first_published);
    assert_eq!(
        ready_while_hook_runs.generation().manifest().generation_id,
        first_scheduled_id,
    );
    assert_eq!(
        feedback_while_hook_runs.metadata().manifest().generation_id,
        first_scheduled_id,
    );
    assert_eq!(
        first_serving_while_hook_runs,
        Some(first_scheduled_id),
        "cold-mount semantics must not start before its exact code generation is serving"
    );
    assert_ne!(second_scheduled_id, first_published);
    assert_eq!(second_scheduled_id, second_published);
    assert_eq!(
        second_serving_while_hook_runs,
        Some(second_scheduled_id),
        "edited-generation semantics must not start while the prior code generation is serving"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panicking_semantic_hook_does_not_retire_later_reconciliation() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let project_root = fixture
        .path()
        .canonicalize()
        .expect("canonical project root");
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    let mut publications = registry.subscribe_generation_publications();
    let panic_calls = Arc::new(AtomicUsize::new(0));
    let panicking_hook = {
        let calls = Arc::clone(&panic_calls);
        Arc::new(
            move |_: Arc<CodeIndexPublishedGenerationV1>| -> SavedGenerationScheduleOutcomeV1 {
                calls.fetch_add(1, Ordering::SeqCst);
                panic!("semantic schedule panic fixture");
            },
        ) as SavedCodeGenerationScheduleHookV1
    };
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(panicking_hook),
            )
            .await
            .expect("mount scheduler")
    );
    let first_generation = wait_for_initial_generation(&registry, fixture.path()).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while panic_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("panicking hook was called");
    let first_publication = published_generation_for_root(&mut publications, &project_root).await;
    assert_eq!(first_publication, first_generation);

    let replacement_deliveries: SemanticDeliveryLogV1 = Arc::new(Mutex::new(Vec::new()));
    let replacement_hook = recording_semantic_hook(&replacement_deliveries);
    assert!(
        !registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                Some(replacement_hook),
            )
            .await
            .expect("replace panicking hook")
    );
    // Remount replays the already-serving generation to the replacement hook.
    wait_for_semantic_delivery(&replacement_deliveries, &first_generation).await;

    fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    assert!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await
    );
    // Early publish can leave the first generation on the broadcast bus
    // (and remount may rebroadcast it). Wait for a distinct id.
    let second_publication = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = publications.recv().await.expect("generation publication");
            if event.project_root.as_path() == project_root
                && event.generation_id != first_generation
            {
                break event.generation_id;
            }
        }
    })
    .await
    .expect("edited generation published");
    let second_generation =
        wait_for_generation_change(&registry, fixture.path(), &first_generation).await;
    wait_for_semantic_delivery(&replacement_deliveries, &second_generation).await;
    registry.shutdown().await;

    assert_ne!(first_generation, second_generation);
    assert_eq!(second_publication, second_generation);
}

/// A sealed generation offered while no semantic runtime is mounted must name
/// the reason it was not scheduled.
///
/// Issue #753: after an aborted projection the daemon kept sealing
/// generations and none of them re-triggered projection, while the runtime sat
/// at `installed`. The handoff returned a bare `false` that every caller
/// discarded, so an operator could not tell "no runtime is mounted" from
/// "nothing needed doing". Both the direct handoff and the reconciler's
/// re-offer must answer with the typed decline.
#[tokio::test]
async fn a_generation_offered_without_a_mounted_semantic_runtime_names_the_decline() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")]);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    assert!(
        registry
            .mount_worktree(
                test_project_id(),
                fixture.path(),
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount scheduler")
    );
    wait_for_live_complete_generation(&registry, fixture.path()).await;

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("scheduler handle");
    let generation = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .latest_complete()
        .expect("serving generation")
        .generation_handle();
    let handoff = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .schedule_semantic_generation(generation);
    assert_eq!(
        handoff,
        SavedGenerationScheduleOutcomeV1::RuntimeNotMounted,
        "the handoff boundary must name an absent semantic runtime"
    );

    assert_eq!(
        registry
            .reschedule_semantic_generation(fixture.path())
            .await,
        SavedGenerationScheduleOutcomeV1::RuntimeNotMounted,
        "the reconciler's re-offer must carry the same typed decline"
    );
}

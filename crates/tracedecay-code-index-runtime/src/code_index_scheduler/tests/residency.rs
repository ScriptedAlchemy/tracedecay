use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1;
use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessSharedMemoryReservationV1, ResidentMemoryComponentIdV1,
    ResidentMemoryPressureV1, ResidentOwnerBytesV1, ResidentOwnerKindV1,
    ResidentOwnerReleaseCauseV1, ResidentOwnerReleaseV1, ResidentOwnerSampleV1,
    ResidentOwnerScopeV1, ResidentOwnerV1, ResidentOwnersReportV1, ResidentOwnersV1,
};

use super::super::{
    CodeIndexCadenceTelemetryV1, CodeIndexCadenceTriggerV1, CodeIndexWorkerPhaseV1,
};
use super::{
    CodeIndexSchedulerRegistryV1, GitFixture, core_search_request, git,
    mounted_core_query_worktree_in, test_project_id, wait_for_generation_change,
    wait_for_live_complete_generation, wait_for_queryable_text_generation, wait_for_worker_phase,
};

const IDLE_WINDOW: Duration = Duration::from_mins(10);

fn rows(report: &ResidentOwnersReportV1) -> Vec<(ResidentOwnerKindV1, String, bool)> {
    report
        .owners
        .iter()
        .map(|row| {
            (
                row.kind,
                row.generation_id.as_str().to_owned(),
                row.protected,
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_worktree_gives_back_its_decode_and_search_still_answers_fresh() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, scope) = mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
    )
    .await;
    let fresh = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("the seated generation answers");
    assert!(!fresh.served_stale);
    let generation = fresh.generation.as_str().to_owned();

    let used = owners.report(Instant::now());
    assert_eq!(
        rows(&used),
        [(
            ResidentOwnerKindV1::DecodedGeneration,
            generation.clone(),
            true
        )],
        "a worktree that just served a complete read holds one protected decode"
    );
    assert_eq!(used.unmeasured_owners, 0);
    let decoded_bytes = used.measured_bytes;
    assert_ne!(decoded_bytes, 0, "the decode reports the bytes it holds");

    let mut receipts = registry.subscribe_cadence_receipts();
    receipts.borrow_and_update();
    let later = Instant::now() + IDLE_WINDOW;
    let released = owners.release_idle(later);
    assert_eq!(
        released
            .iter()
            .map(|release| (release.kind, release.cause, release.bytes.measured()))
            .collect::<Vec<_>>(),
        [(
            ResidentOwnerKindV1::DecodedGeneration,
            ResidentOwnerReleaseCauseV1::Idle,
            Some(decoded_bytes)
        )]
    );
    let idle = owners.report(later);
    assert_eq!(rows(&idle), []);
    assert_eq!(idle.measured_bytes, 0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !receipts.has_changed().unwrap(),
        "a worktree with no refused work is not woken, so nothing re-decodes"
    );
    assert_eq!(owners.report(later).measured_bytes, 0);

    let staleness = registry
        .dashboard_freshness_read(fixture.path())
        .await
        .expect("freshness reads")
        .expect("mounted worktree")
        .staleness_state;
    assert_eq!(
        staleness,
        Some(CodeIndexStalenessStateV1::Fresh),
        "status keeps reporting the released generation as the fresh seat"
    );
    let after = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("search keeps serving after the decode is released");
    assert!(
        !after.served_stale,
        "the released worktree still answers current"
    );
    assert_eq!(after.generation.as_str(), generation);
    assert_eq!(
        after.authorized.fallback.ordered_candidates,
        fresh.authorized.fallback.ordered_candidates
    );

    registry.shutdown().await;
}

/// The advisory cycle resolves its generation through this lookup. After the
/// idle window released the decode, the first lookup must answer with the
/// still-current sealed generation, not only the retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_feedback_lookup_after_an_idle_release_answers() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, scope) = mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
    )
    .await;
    let seated = wait_for_live_complete_generation(&registry, fixture.path())
        .await
        .generation()
        .manifest()
        .generation_id
        .clone();
    let before = registry
        .latest_feedback_generation_for_scope(fixture.path(), &scope)
        .await
        .expect("a seated generation answers the feedback lookup");
    assert_eq!(before.metadata().manifest().generation_id, seated);

    let released = owners.release_idle(Instant::now() + IDLE_WINDOW);
    assert_eq!(
        released
            .iter()
            .map(|release| (release.kind, release.cause))
            .collect::<Vec<_>>(),
        [(
            ResidentOwnerKindV1::DecodedGeneration,
            ResidentOwnerReleaseCauseV1::Idle
        )]
    );

    let first = registry
        .latest_feedback_generation_for_scope(fixture.path(), &scope)
        .await
        .map(|generation| generation.metadata().manifest().generation_id.clone());
    assert_eq!(first, Some(seated));

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_swaps_keep_retained_bytes_flat() {
    const FIRST: &str = "fn main() { first(); }\nfn first() {}\n";
    const SECOND: &str = "fn main() { second(); }\nfn second() {}\n";
    let fixture = GitFixture::new(&[("src/main.rs", FIRST)]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, _) = mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
    )
    .await;
    let mut generation = wait_for_live_complete_generation(&registry, fixture.path())
        .await
        .generation()
        .manifest()
        .generation_id
        .clone();
    let mut retained = vec![owners.report(Instant::now()).measured_bytes];
    let mut row_counts = vec![owners.report(Instant::now()).owners.len()];
    for swap in 1..=6 {
        let content = if swap % 2 == 0 { FIRST } else { SECOND };
        fixture.edit("src/main.rs", content);
        git(fixture.path(), &["commit", "-qam", &format!("swap {swap}")]);
        assert!(matches!(
            registry
                .notify_path(fixture.path(), fixture.path().join("src/main.rs"))
                .await,
            super::super::CodeIndexDemandAdmissionV1::Queued
        ));
        generation = wait_for_generation_change(&registry, fixture.path(), &generation).await;
        let seated = wait_for_live_complete_generation(&registry, fixture.path()).await;
        assert_eq!(seated.generation().manifest().generation_id, generation);
        let report = owners.report(Instant::now());
        retained.push(report.measured_bytes);
        row_counts.push(report.owners.len());
    }

    assert_eq!(
        row_counts,
        [1, 1, 1, 1, 1, 1, 1],
        "one decode per worktree, never a pile"
    );
    assert_eq!(
        [retained[4], retained[6]],
        [retained[2]; 2],
        "each return to the first tree retains exactly what the previous return did"
    );
    assert_eq!([retained[5]], [retained[3]]);

    registry.shutdown().await;
}

async fn next_receipt_trigger(
    receipts: &mut tokio::sync::watch::Receiver<CodeIndexCadenceTelemetryV1>,
) -> CodeIndexCadenceTriggerV1 {
    tokio::time::timeout(Duration::from_mins(1), async {
        loop {
            receipts
                .changed()
                .await
                .expect("the cadence channel stays open while the registry lives");
            if let Some(receipt) = receipts.borrow_and_update().latest().cloned() {
                return receipt.trigger;
            }
        }
    })
    .await
    .expect("a pass records its receipt")
}

/// Mount a worktree whose first text build the resident-memory authority
/// refuses: the ledger leaves 1 GiB below the watermark, under the builder's
/// 1.5 GiB floor, until the returned reservation drops.
async fn mount_with_refused_text_build(
    fixture: &GitFixture,
    store: &TempDir,
    owners: &Arc<ResidentOwnersV1>,
) -> (
    CodeIndexSchedulerRegistryV1,
    ProcessSharedMemoryReservationV1,
    tokio::sync::watch::Receiver<CodeIndexCadenceTelemetryV1>,
) {
    const GIB: u64 = 1024 * 1024 * 1024;
    let limit = NonZeroU64::new(16 * GIB).unwrap();
    let pressure = Arc::new(ResidentMemoryPressureV1::new(limit));
    let high_watermark = pressure.high_watermark_bytes();
    let resident_memory = Arc::new(ProcessResidentMemoryV1::with_pressure(limit, pressure));
    let blocker = resident_memory
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("test-held-memory").unwrap(),
            NonZeroU64::new(high_watermark - GIB).unwrap(),
        )
        .expect("the blocker fits the ledger");
    let registry = CodeIndexSchedulerRegistryV1::with_resident_memory(1, resident_memory)
        .with_resident_owners(Arc::clone(owners));
    let mut receipts = registry.subscribe_cadence_receipts();
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount daemon-owned scheduler");
    tokio::time::sleep(Duration::from_secs(1)).await;
    wait_for_worker_phase(&registry, fixture.path(), CodeIndexWorkerPhaseV1::Parked).await;
    receipts.borrow_and_update();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !receipts.has_changed().unwrap(),
        "a refused build parks the worker instead of spinning continuations"
    );
    assert!(
        registry
            .latest_text_serving_for_root(fixture.path())
            .await
            .is_none(),
        "the refused build left no queryable text owner"
    );
    (registry, blocker, receipts)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_text_build_refused_for_memory_retries_when_memory_is_given_back() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, blocker, mut receipts) =
        mount_with_refused_text_build(&fixture, &store, &owners).await;

    drop(blocker);
    owners.note_headroom();

    assert_eq!(
        next_receipt_trigger(&mut receipts).await,
        CodeIndexCadenceTriggerV1::MemoryHeadroom
    );
    let text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    assert!(text.query_owners_are_ready());

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_text_build_retries_after_its_delay_when_rss_falls_without_a_release() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, blocker, mut receipts) =
        mount_with_refused_text_build(&fixture, &store, &owners).await;

    drop(blocker);

    assert_eq!(
        next_receipt_trigger(&mut receipts).await,
        CodeIndexCadenceTriggerV1::MemoryRetry
    );
    let text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    assert!(
        text.query_owners_are_ready(),
        "the delayed retry finds the headroom no owner release announced"
    );

    registry.shutdown().await;
}

/// Retained state another worktree holds, modelled as a ledger reservation so
/// releasing it gives the build real headroom.
struct HeldMemoryOwner {
    held: std::sync::Mutex<Option<ProcessSharedMemoryReservationV1>>,
    bytes: u64,
    last_used: Instant,
}

impl ResidentOwnerV1 for HeldMemoryOwner {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        self.held
            .lock()
            .unwrap()
            .is_some()
            .then(|| ResidentOwnerSampleV1 {
                generation_id: CodeGenerationId::new("generation.v1.superseded").unwrap(),
                bytes: ResidentOwnerBytesV1::Measured(self.bytes),
                last_used: self.last_used,
                serving: false,
            })
    }

    fn release(&self) -> ResidentOwnerReleaseV1 {
        match self.held.lock().unwrap().take() {
            Some(_) => ResidentOwnerReleaseV1::Released {
                bytes: ResidentOwnerBytesV1::Measured(self.bytes),
            },
            None => ResidentOwnerReleaseV1::Empty,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_text_build_sheds_retained_state_before_it_refuses() {
    const GIB: u64 = 1024 * 1024 * 1024;
    let limit = NonZeroU64::new(16 * GIB).unwrap();
    let pressure = Arc::new(ResidentMemoryPressureV1::new(limit));
    let held_bytes = pressure.high_watermark_bytes() - GIB;
    let resident_memory = Arc::new(ProcessResidentMemoryV1::with_pressure(limit, pressure));
    let held = Arc::new(HeldMemoryOwner {
        held: std::sync::Mutex::new(Some(
            resident_memory
                .reserve_process_shared(
                    ResidentMemoryComponentIdV1::new("test-superseded-decode").unwrap(),
                    NonZeroU64::new(held_bytes).unwrap(),
                )
                .expect("the held state fits the ledger"),
        )),
        bytes: held_bytes,
        last_used: Instant::now(),
    });
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let held_owner: Arc<dyn ResidentOwnerV1> = Arc::clone(&held) as Arc<dyn ResidentOwnerV1>;
    let _registration = owners
        .register(
            ResidentOwnerScopeV1 {
                project_id: ProjectId::new("project.other").unwrap(),
                worktree_id: WorktreeId::new("worktree.other").unwrap(),
            },
            ResidentOwnerKindV1::SupersededGeneration,
            Arc::downgrade(&held_owner),
        )
        .unwrap();
    assert_eq!(owners.report(Instant::now()).measured_bytes, held_bytes);
    let registry = CodeIndexSchedulerRegistryV1::with_resident_memory(1, resident_memory)
        .with_resident_owners(Arc::clone(&owners));
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount daemon-owned scheduler");

    let text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    assert!(
        text.query_owners_are_ready(),
        "the build shed the superseded decode and finished instead of refusing"
    );
    assert!(held.held.lock().unwrap().is_none());
    assert_eq!(
        owners
            .report(Instant::now())
            .owners
            .iter()
            .filter(|row| row.kind == ResidentOwnerKindV1::SupersededGeneration)
            .count(),
        0
    );

    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readers_of_a_build_waiting_for_memory_do_not_spin_the_worker() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, blocker, mut receipts) =
        mount_with_refused_text_build(&fixture, &store, &owners).await;

    // The first complete-generation demand is new work and wakes one pass,
    // which finds the build still waiting for memory.
    assert!(
        registry
            .latest_complete_ready(fixture.path())
            .await
            .is_none()
    );
    assert_eq!(
        next_receipt_trigger(&mut receipts).await,
        CodeIndexCadenceTriggerV1::QueryAdmission
    );
    wait_for_worker_phase(&registry, fixture.path(), CodeIndexWorkerPhaseV1::Parked).await;
    receipts.borrow_and_update();
    for _ in 0..20 {
        assert!(
            registry
                .latest_complete_ready(fixture.path())
                .await
                .is_none(),
            "no complete generation serves while its text build waits for memory"
        );
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !receipts.has_changed().unwrap(),
        "a reader's wake cannot help a build parked on memory, so no pass runs"
    );

    drop(blocker);
    owners.note_headroom();
    assert_eq!(
        next_receipt_trigger(&mut receipts).await,
        CodeIndexCadenceTriggerV1::MemoryHeadroom
    );
    let text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    assert!(text.query_owners_are_ready());

    registry.shutdown().await;
}

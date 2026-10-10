use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_contracts::code_index_freshness::{
    CodeGraphServingReadinessV1, CodeIndexStalenessStateV1,
};
use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};
use tracedecay_runtime_core::path_safety::canonical_existing_identity;
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessResidentSampleV1, ProcessSharedMemoryReservationV1,
    ResidentHoldingV1, ResidentMemoryComponentIdV1, ResidentMemoryPressureV1, ResidentOwnerBytesV1,
    ResidentOwnerKindV1, ResidentOwnerReleaseCauseV1, ResidentOwnerReleaseV1,
    ResidentOwnerSampleV1, ResidentOwnerScopeV1, ResidentOwnerV1, ResidentOwnersReportV1,
    ResidentOwnersV1,
};

use super::super::{
    CodeIndexCadenceTelemetryV1, CodeIndexCadenceTriggerV1, CodeIndexWorkerPhaseV1,
};

use super::{
    ALPHA_LIB_V1, CodeIndexSchedulerRegistryV1, GitFixture, SERVING_SEAT_FAILURE_CEILING,
    core_search_request, git, install_verified_graph_store_on_text, mounted_core_query_worktree_at,
    mounted_core_query_worktree_in, mounted_text_query_worktree_at, test_project_id,
    wait_for_generation_change, wait_for_live_complete_generation,
    wait_for_queryable_text_generation, wait_for_settled_owner, wait_for_worker_phase,
    wait_until_serving_seat, with_untouched_fillers,
};

const IDLE_WINDOW: Duration = Duration::from_mins(10);

fn decoded_kinds(report: &ResidentOwnersReportV1) -> Vec<ResidentOwnerKindV1> {
    report
        .owners
        .iter()
        .filter(|row| row.kind == ResidentOwnerKindV1::DecodedGeneration)
        .map(|row| row.kind)
        .collect()
}

fn graph_copy_kinds(report: &ResidentOwnersReportV1) -> Vec<ResidentOwnerKindV1> {
    report
        .owners
        .iter()
        .filter(|row| {
            matches!(
                row.kind,
                ResidentOwnerKindV1::GraphCatalog | ResidentOwnerKindV1::GraphEngine
            )
        })
        .map(|row| row.kind)
        .collect()
}

fn search_anchors(search: &super::super::query_runtime::ExecutedQuerySearchV1) -> Vec<String> {
    search
        .authorized
        .fallback
        .ordered_candidates
        .iter()
        .map(|ranked| ranked.candidate.anchor_id.as_str().to_owned())
        .collect()
}

fn rows(report: &ResidentOwnersReportV1) -> Vec<(ResidentOwnerKindV1, String, bool)> {
    report
        .owners
        .iter()
        .flat_map(|row| {
            row.holders
                .iter()
                .map(|holder| (row.kind, holder.holding.as_str().to_owned(), row.protected))
        })
        .collect()
}

/// Issue #3328: after the worker parks, the seated decode is a third copy of
/// the generation the text artifact and catalog already serve. Drop it
/// without waiting out the idle window; search must keep the same answers
/// and must not re-pin the decode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parked_worktree_releases_its_decode_and_search_still_answers() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn park_release_target() -> u32 { 7 }\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, scope) = mounted_text_query_worktree_at(
        CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners)),
        fixture.path(),
        store.path().to_path_buf(),
    )
    .await;

    let parked = owners.report(Instant::now());
    assert_eq!(
        decoded_kinds(&parked),
        [],
        "a parked worktree must not keep a seated decode: {parked:?}"
    );

    let without_decode = registry
        .execute_query_search(&scope, core_search_request("park_release_target"))
        .await
        .expect("search serves from the text seat after the decode is released");
    assert!(!without_decode.served_stale);
    let anchors = search_anchors(&without_decode);
    assert!(
        !anchors.is_empty(),
        "search must still rank the parked generation: {anchors:?}"
    );
    assert_eq!(
        decoded_kinds(&owners.report(Instant::now())),
        [],
        "search must not re-pin the seated decode"
    );

    assert!(
        registry.request_complete_generation(fixture.path()).await,
        "complete-generation demand reaches the mounted worktree"
    );
    let _seated = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let with_decode = registry
        .execute_query_search(&scope, core_search_request("park_release_target"))
        .await
        .expect("search still answers after a demanded decode");
    assert_eq!(search_anchors(&with_decode), anchors);

    registry.shutdown().await;
}

/// Park drops catalog and engine after activation; status reports the typed
/// warming state until the next graph read reseats the store (#3328).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parked_worktree_releases_catalog_and_engine_and_reports_warming() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn park_graph_target() -> u32 { 7 }\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, scope) = mounted_text_query_worktree_at(
        CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners)),
        fixture.path(),
        store.path().to_path_buf(),
    )
    .await;
    let root = canonical_existing_identity(fixture.path()).expect("canonical root");
    assert!(registry.request_complete_generation(fixture.path()).await);
    let seated = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    install_verified_graph_store_on_text(&text, &seated);
    text.interactive_graph_store()
        .expect("installed graph store")
        .warm_serving_engine()
        .expect("pin the serving engine so park has something to release");
    drop(seated);

    // The worker may still be in the complete-generation pass and replace
    // this store with one whose catalog is still warming. Wait until both
    // copies are resident on the serving text, then park.
    let deadline = Instant::now() + SERVING_SEAT_FAILURE_CEILING;
    loop {
        let current = registry
            .latest_text_serving_for_root(&root)
            .await
            .expect("serving text");
        let catalog_ready = current
            .interactive_graph_store()
            .ok()
            .and_then(|store| store.interactive_catalog_bytes())
            .is_some();
        if !catalog_ready {
            if let Some(latest) = registry
                .latest_complete_serving_for_test(fixture.path())
                .await
            {
                install_verified_graph_store_on_text(&current, &latest);
                let _ = current
                    .interactive_graph_store()
                    .ok()
                    .and_then(|store| store.warm_serving_engine().ok());
            }
        }
        let warm = owners.report(Instant::now());
        if graph_copy_kinds(&warm)
            .iter()
            .any(|kind| *kind == ResidentOwnerKindV1::GraphCatalog)
            && graph_copy_kinds(&warm)
                .iter()
                .any(|kind| *kind == ResidentOwnerKindV1::GraphEngine)
            && current.code_graph_serving_readiness() == CodeGraphServingReadinessV1::Ready
        {
            break;
        }
        assert!(
            Instant::now() <= deadline,
            "catalog and engine must become resident before park: {warm:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    loop {
        registry
            .release_decode_when_parked_for_test(fixture.path())
            .await;
        let parked = owners.report(Instant::now());
        let current = registry
            .latest_text_serving_for_root(&root)
            .await
            .expect("serving text");
        let released = current
            .interactive_graph_store()
            .ok()
            .is_some_and(|store| store.interactive_catalog_bytes().is_none());
        let catalog_gone = !graph_copy_kinds(&parked)
            .iter()
            .any(|kind| *kind == ResidentOwnerKindV1::GraphCatalog);
        if catalog_gone && released {
            assert!(
                matches!(
                    current.code_graph_serving_readiness(),
                    CodeGraphServingReadinessV1::Warming { .. }
                ),
                "a memory release reports warming until a read reseats the store"
            );
            break;
        }
        assert!(
            Instant::now() <= deadline,
            "a parked worktree must not keep the catalog: {parked:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let search = registry
        .execute_query_search(&scope, core_search_request("park_graph_target"))
        .await
        .expect("search serves from text after catalog and engine are released");
    assert!(!search_anchors(&search).is_empty());
    assert!(
        !graph_copy_kinds(&owners.report(Instant::now()))
            .iter()
            .any(|kind| *kind == ResidentOwnerKindV1::GraphCatalog),
        "search must not re-pin the catalog"
    );

    registry.shutdown().await;
}

/// The parked release frees the seat only because the text owner serves in
/// its place. While the installed text owner cannot serve — its lexical
/// projection is still warming or it latched a failure — the seated decode
/// is the only servable generation: taking it turns serve-old into
/// `GenerationUnavailable`. The release must keep the seat, and it must
/// advance the registry-wide seat counter only when a seat was really
/// written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parked_worktree_keeps_its_decode_while_the_text_owner_warms() {
    let fixture = GitFixture::new(&[("src/lib.rs", "pub fn park_gate_target() -> u32 { 7 }\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, scope) = mounted_text_query_worktree_at(
        CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners)),
        fixture.path(),
        store.path().to_path_buf(),
    )
    .await;
    let root = canonical_existing_identity(fixture.path()).expect("canonical root");
    let ready_text = registry
        .latest_text_serving_for_root(&root)
        .await
        .expect("mounted text owner is query-ready");

    // Seat a real decode, then let the idle release clear the complete-read
    // latch so the parked release is the code under test again.
    assert!(registry.request_complete_generation(fixture.path()).await);
    let seated = wait_for_live_complete_generation(&registry, fixture.path()).await;
    let generation_id = seated.generation().manifest().generation_id.clone();
    wait_for_settled_owner(&registry, fixture.path()).await;
    owners.release_idle(Instant::now() + IDLE_WINDOW);

    let mut seats = registry.subscribe_serving_seats();
    async fn reseat(
        registry: &CodeIndexSchedulerRegistryV1,
        root: &Path,
        seated: &super::super::LatestCompleteCodeIndexV1,
    ) {
        let mounted = registry.mounted.lock().await;
        *mounted
            .get(root)
            .expect("mounted worktree")
            .serving_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(seated.clone());
    }
    async fn install_text(
        registry: &CodeIndexSchedulerRegistryV1,
        root: &Path,
        text: super::super::LatestCodeTextGenerationV1,
    ) {
        let mounted = registry.mounted.lock().await;
        *mounted
            .get(root)
            .expect("mounted worktree")
            .text_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(text);
    }
    async fn seat_is_occupied(registry: &CodeIndexSchedulerRegistryV1, root: &Path) -> bool {
        let mounted = registry.mounted.lock().await;
        mounted
            .get(root)
            .expect("mounted worktree")
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    // A ready text owner lets the take proceed and publishes the seat write
    // on the registry-wide counter every subscriber watches.
    reseat(&registry, &root, &seated).await;
    seats.borrow_and_update();
    registry
        .release_decode_when_parked_for_test(fixture.path())
        .await;
    assert!(
        !seat_is_occupied(&registry, &root).await,
        "a query-ready text owner leaves the decode spare on park"
    );
    assert!(
        seats.has_changed().expect("seats channel open"),
        "the seat counter must publish the take"
    );

    // A text owner whose projection is still warming cannot serve yet; the
    // seat stays and no counter advance claims a write happened.
    let warming_text = {
        let mounted = registry.mounted.lock().await;
        mounted
            .get(&root)
            .expect("mounted worktree")
            .historical_generation_owner
            .published_text_generation(&generation_id)
            .expect("read retained generation metadata")
            .expect("retained text owner")
    };
    assert!(!warming_text.query_owners_are_ready());
    reseat(&registry, &root, &seated).await;
    install_text(&registry, &root, warming_text).await;
    seats.borrow_and_update();
    registry
        .release_decode_when_parked_for_test(fixture.path())
        .await;
    assert!(
        seat_is_occupied(&registry, &root).await,
        "the seat is the only servable generation while text warms"
    );
    assert!(
        !seats.has_changed().expect("seats channel open"),
        "no seat write means no counter advance"
    );
    let stale = registry
        .execute_query_search(&scope, core_search_request("park_gate_target"))
        .await
        .expect("the kept seat still answers search");
    assert!(
        !search_anchors(&stale).is_empty(),
        "serve-old must keep ranking the seated generation"
    );

    // Once the text owner can serve again the same park releases the seat.
    // The search's complete-generation demand may still be latched; give it
    // back through the idle release and seat again so the parked take is
    // the code under test.
    install_text(&registry, &root, ready_text).await;
    owners.release_idle(Instant::now() + IDLE_WINDOW);
    reseat(&registry, &root, &seated).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        registry
            .release_decode_when_parked_for_test(fixture.path())
            .await;
        if !seat_is_occupied(&registry, &root).await {
            break;
        }
        assert!(
            Instant::now() <= deadline,
            "a ready text owner frees the seat"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    registry.shutdown().await;
}

/// Issue #3328: while the successor's own graph activation is still
/// pending, the held predecessor is the only servable graph
/// `graph_predecessor` answers stale reads from. A resident-owners release
/// takes the predecessor's catalog and engine bytes but must keep the
/// servable hold until the successor's activation settles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resident_release_keeps_the_predecessor_while_its_graph_is_pending() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let registry = CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners));
    let root = fixture.path();
    registry
        .mount_worktree(test_project_id(), root, store.path().to_path_buf())
        .await
        .expect("mount worktree");
    assert!(registry.request_complete_generation(root).await);
    let first_seat = wait_for_live_complete_generation(&registry, root).await;
    let first = first_seat
        .generation
        .manifest()
        .generation_id
        .as_str()
        .to_owned();
    let worktree_id = first_seat
        .generation
        .snapshot()
        .worktree
        .clone()
        .expect("worktree identity");
    let first_text = wait_for_queryable_text_generation(&registry, root).await;
    install_verified_graph_store_on_text(&first_text, &first_seat);
    drop(first_seat);

    // The second publication inherits the warm graph as its predecessor;
    // keep its own activation failing so its readiness stays Pending.
    super::super::graph_activation::set_injected_activation_failures(&worktree_id, usize::MAX);
    fixture.edit("src/lib.rs", "pub fn changed_after_the_warm_graph() {}\n");
    assert!(matches!(
        registry.notify_path(root, root.join("src/lib.rs")).await,
        super::super::CodeIndexDemandAdmissionV1::Queued
    ));
    let first_id = first.clone();
    let registry_ref = &registry;
    let second_text =
        wait_until_serving_seat(registry_ref, root, SERVING_SEAT_FAILURE_CEILING, || {
            let first_id = first_id.clone();
            async move {
                registry_ref
                    .latest_text_serving_for_root(root)
                    .await
                    .filter(|text| text.metadata().manifest().generation_id.as_str() != first_id)
            }
        })
        .await;
    let held = || {
        second_text
            .held_graph_predecessor()
            .map(|held| held.metadata().manifest().generation_id.as_str().to_owned())
    };
    assert_eq!(
        held(),
        Some(first.clone()),
        "the successor holds the warm graph as its predecessor"
    );
    assert_eq!(
        second_text.code_graph_serving_readiness(),
        CodeGraphServingReadinessV1::Pending,
        "the injected failure keeps the successor's graph pending"
    );

    wait_for_queryable_text_generation(&registry, root).await;
    owners.release_idle(Instant::now() + IDLE_WINDOW);
    assert_eq!(
        held(),
        Some(first.clone()),
        "the release must keep the only servable graph"
    );
    assert!(
        second_text
            .graph_predecessor(true)
            .is_some_and(|served| served.metadata().manifest().generation_id.as_str() == first),
        "the retained predecessor still serves stale graph reads"
    );

    // Once activation settles the same release drops the hold: the
    // successor's own graph serves now and the predecessor is spare.
    super::super::graph_activation::set_injected_activation_failures(&worktree_id, 0);
    let deadline = Instant::now() + SERVING_SEAT_FAILURE_CEILING;
    while second_text.code_graph_serving_readiness() != CodeGraphServingReadinessV1::Ready {
        assert!(
            Instant::now() <= deadline,
            "clearing the failure must let activation reach ready"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    owners.release_idle(Instant::now() + IDLE_WINDOW);
    assert_eq!(held(), None, "a ready successor frees the predecessor");

    registry.shutdown().await;
}

/// Eight enrolled worktrees is the daemon project-server cap. After they
/// park, none of them may keep a whole decoded generation just because
/// enrollment seated one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eight_parked_worktrees_keep_no_decoded_generations() {
    let fixtures = (0..8)
        .map(|ordinal| {
            let source = format!("pub fn symbol_{ordinal}() -> u32 {{ {ordinal} }}\n");
            GitFixture::new(&[("src/lib.rs", source.as_str())])
        })
        .collect::<Vec<_>>();
    let stores = (0..8)
        .map(|_| TempDir::new().expect("store root"))
        .collect::<Vec<_>>();
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let mut registry =
        CodeIndexSchedulerRegistryV1::new(8).with_resident_owners(Arc::clone(&owners));
    let mut scopes = Vec::new();
    for (fixture, store) in fixtures.iter().zip(&stores) {
        let (next, scope) =
            mounted_text_query_worktree_at(registry, fixture.path(), store.path().to_path_buf())
                .await;
        registry = next;
        scopes.push(scope);
    }

    assert_eq!(
        decoded_kinds(&owners.report(Instant::now())),
        [],
        "eight parked worktrees must not retain eight seated decodes"
    );

    for (ordinal, (fixture, scope)) in fixtures.iter().zip(&scopes).enumerate() {
        let found = registry
            .execute_query_search(scope, core_search_request(&format!("symbol_{ordinal}")))
            .await
            .expect("each parked worktree still answers search");
        assert!(!found.served_stale, "{}", fixture.path().display());
        assert!(
            !search_anchors(&found).is_empty(),
            "symbol_{ordinal} must remain searchable after park"
        );
    }
    assert_eq!(
        decoded_kinds(&owners.report(Instant::now())),
        [],
        "search across the eight worktrees must not re-pin their decodes"
    );

    registry.shutdown().await;
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
    let text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    assert!(text.query_owners_are_ready());
    wait_for_settled_owner(&registry, fixture.path()).await;
    wait_for_worker_phase(&registry, fixture.path(), CodeIndexWorkerPhaseV1::Parked).await;
    // Park drops the publish-time seat. A complete read is what renews it.
    assert!(registry.request_complete_generation(fixture.path()).await);
    let generation = {
        let seated = wait_for_live_complete_generation(&registry, fixture.path()).await;
        seated
            .generation()
            .manifest()
            .generation_id
            .as_str()
            .to_owned()
    };
    let fresh = registry
        .execute_query_search(&scope, core_search_request("main"))
        .await
        .expect("the seated generation answers");
    assert!(!fresh.served_stale);
    assert_eq!(fresh.generation.as_str(), generation);

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

/// The decode a complete read demands can seat between that read's miss and
/// its look at the slot. The read serves that seat instead of refusing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_complete_read_serves_the_seat_its_demand_lands_after_its_miss() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, scope) = mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
    )
    .await;
    wait_for_settled_owner(&registry, fixture.path()).await;
    assert_eq!(owners.release_idle(Instant::now() + IDLE_WINDOW).len(), 1);
    assert!(
        registry
            .latest_complete_serving_for_test(fixture.path())
            .await
            .is_none(),
        "only the text owner stays seated"
    );

    let root = canonical_existing_identity(fixture.path()).expect("canonical fixture");
    let (missed, release_probe) = registry.pause_next_complete_seat_probe_miss(root);
    let read = tokio::spawn({
        let registry = registry.clone();
        let scope = scope.clone();
        async move {
            registry
                .latest_complete_fresh_for_scope_awaiting_seat(
                    &scope,
                    tokio::time::Instant::now() + SERVING_SEAT_FAILURE_CEILING,
                )
                .await
        }
    });
    missed
        .await
        .expect("the read misses before the decode seats");
    let seated = wait_for_live_complete_generation(&registry, fixture.path()).await;
    release_probe.send(()).expect("the probe waits at its gate");

    let served = read.await.expect("read task").expect(
        "a seat that landed after the miss serves the read instead of ending it unavailable",
    );
    assert_eq!(
        served.generation.manifest().generation_id,
        seated.generation.manifest().generation_id
    );

    registry.shutdown().await;
}

/// A module with documented items, a struct with methods, cross-module calls
/// and a clone-sized body, so a decode holds every kind of page record.
fn linked_module(ordinal: usize) -> (String, String) {
    let next = (ordinal + 1) % 24;
    (
        format!("src/module_{ordinal:02}.rs"),
        format!(
            "//! Module {ordinal}.\nuse crate::module_{next:02}::transform_{next};\n\n\
             /// Transform the input for module {ordinal}.\n\
             pub fn transform_{ordinal}(input: &str, limit: usize) -> usize {{\n    \
             let mut total = 0;\n    for (index, part) in input.trim().split(',').enumerate() {{\n        \
             if index >= limit {{ break; }}\n        total += part.len() * {ordinal};\n    }}\n    total\n}}\n\n\
             pub fn describe_{ordinal}(value: u64) -> String {{\n    let label = format!(\"{{value}}-{ordinal}\");\n    \
             transform_{next}(&label, 3);\n    label.to_uppercase()\n}}\n\n\
             pub struct Holder{ordinal} {{\n    values: Vec<u32>,\n}}\n\n\
             impl Holder{ordinal} {{\n    pub fn total(&self) -> u32 {{\n        self.values.iter().sum()\n    }}\n}}\n"
        ),
    )
}

/// Linked worktrees sealing identical content serve one decode between them.
/// Each seals its own generation, yet the inventory reports a single
/// `decoded_generation` row naming both worktrees, holding the content once
/// plus each worktree's own evidence, and the idle window gives all of it
/// back, down to the disk-backed state a restart leaves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_worktrees_on_identical_content_hold_one_decoded_generation() {
    let modules = (0..24).map(linked_module).collect::<Vec<_>>();
    let files = modules
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = GitFixture::new(&files);
    let linked_parent = TempDir::new().expect("linked worktree parent");
    let linked = linked_parent.path().join("linked");
    git(
        fixture.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().expect("linked worktree path"),
            "main",
        ],
    );
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, primary) = mounted_core_query_worktree_at(
        CodeIndexSchedulerRegistryV1::new(2).with_resident_owners(Arc::clone(&owners)),
        fixture.path(),
        store.path().join("primary"),
    )
    .await;
    registry
        .execute_query_search(&primary, core_search_request("transform_3"))
        .await
        .expect("the primary worktree answers");
    assert!(registry.request_complete_generation(fixture.path()).await);
    wait_for_live_complete_generation(&registry, fixture.path()).await;
    let alone = owners.report(Instant::now());

    let (registry, secondary) =
        mounted_core_query_worktree_at(registry, &linked, store.path().join("linked")).await;
    registry
        .execute_query_search(&secondary, core_search_request("transform_3"))
        .await
        .expect("the linked worktree answers");
    assert!(registry.request_complete_generation(&linked).await);
    wait_for_live_complete_generation(&registry, &linked).await;
    let both = owners.report(Instant::now());

    let decoded = |report: &ResidentOwnersReportV1| {
        report
            .owners
            .iter()
            .filter(|row| row.kind == ResidentOwnerKindV1::DecodedGeneration)
            .map(|row| {
                (
                    row.holders
                        .iter()
                        .map(|holder| holder.worktree_id.clone())
                        .collect::<Vec<_>>(),
                    row.content_digest.is_some(),
                    row.bytes.measured(),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut worktrees = vec![primary.worktree_id.clone(), secondary.worktree_id.clone()];
    worktrees.sort();
    let alone_decoded = decoded(&alone);
    let [(alone_worktrees, true, Some(alone_bytes))] = alone_decoded.as_slice() else {
        panic!("one decoded owner must serve the primary worktree: {alone_decoded:?}");
    };
    assert_eq!(alone_worktrees, &vec![primary.worktree_id.clone()]);
    assert_eq!(alone.measured_bytes, *alone_bytes);

    let both_decoded = decoded(&both);
    let [(both_worktrees, true, Some(both_bytes))] = both_decoded.as_slice() else {
        panic!("one content-addressed decode must serve both worktrees: {both_decoded:?}");
    };
    assert_eq!(
        both_worktrees, &worktrees,
        "one decode row must name both worktrees"
    );
    assert_eq!(both.measured_bytes, *both_bytes);
    assert!(
        both_bytes > alone_bytes,
        "the second owner still contributes its own manifest evidence"
    );
    assert!(
        *both_bytes < alone_bytes.saturating_mul(2),
        "identical content must not retain two full decodes"
    );

    let later = Instant::now() + IDLE_WINDOW;
    let released = owners.release_idle(later);
    assert_eq!(
        released
            .iter()
            .map(|release| (release.kind, release.cause))
            .collect::<Vec<_>>(),
        [
            (
                ResidentOwnerKindV1::DecodedGeneration,
                ResidentOwnerReleaseCauseV1::Idle
            ),
            (
                ResidentOwnerKindV1::DecodedGeneration,
                ResidentOwnerReleaseCauseV1::Idle
            ),
        ]
    );
    assert_eq!(
        released
            .iter()
            .map(|release| release.bytes.measured().unwrap_or(0))
            .sum::<u64>(),
        *both_bytes,
        "the two releases give back exactly what the shared row held"
    );
    let idle = owners.report(later);
    assert_eq!(decoded(&idle), []);
    assert_eq!(idle.measured_bytes, 0);

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
    let decode_rows = |report: &ResidentOwnersReportV1| {
        report
            .owners
            .iter()
            .filter(|row| row.kind == ResidentOwnerKindV1::DecodedGeneration)
            .count()
    };
    let mut retained = vec![owners.report(Instant::now()).measured_bytes];
    let mut row_counts = vec![decode_rows(&owners.report(Instant::now()))];
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
        row_counts.push(decode_rows(&report));
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

/// An increment keeps the documents it re-extracted for the next edit's
/// incremental reparse. The inventory reports them as the worktree's own
/// holding and its idle window gives them back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_increment_reports_its_retained_parses_and_the_idle_window_releases_them() {
    let fixture = GitFixture::new(&with_untouched_fillers(&[
        ("src/main.rs", "fn main() { first(); }\nfn first() {}\n"),
        ("src/lib.rs", "pub fn untouched() {}\n"),
    ]));
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, _) = mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::new(1).with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
    )
    .await;
    let cold = wait_for_live_complete_generation(&registry, fixture.path())
        .await
        .generation()
        .manifest()
        .generation_id
        .clone();
    let retained_parses = |report: &ResidentOwnersReportV1| {
        report
            .owners
            .iter()
            .filter(|row| row.kind == ResidentOwnerKindV1::RetainedParses)
            .map(|row| {
                (
                    row.holders.len(),
                    row.holders[0].holding.clone(),
                    row.protected,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        retained_parses(&owners.report(Instant::now())),
        [],
        "a full build retains no parse"
    );

    fixture.edit("src/main.rs", "fn main() { second(); }\nfn second() {}\n");
    git(fixture.path(), &["commit", "-qam", "edit"]);
    assert!(matches!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/main.rs"))
            .await,
        super::super::CodeIndexDemandAdmissionV1::Queued
    ));
    wait_for_generation_change(&registry, fixture.path(), &cold).await;
    assert_eq!(
        retained_parses(&owners.report(Instant::now())),
        [(1, ResidentHoldingV1::Worktree, false)],
        "the re-extracted file is retained by the worktree and pressure may shed it"
    );

    let released = owners.release_idle(Instant::now() + IDLE_WINDOW);
    assert!(
        released.iter().any(
            |release| release.kind == ResidentOwnerKindV1::RetainedParses
                && release.cause == ResidentOwnerReleaseCauseV1::Idle
        ),
        "{released:?}"
    );
    assert_eq!(retained_parses(&owners.report(Instant::now())), []);

    registry.shutdown().await;
}

/// A pass lets go of the sources it captured once its build is sealed, and
/// the shared byte pool then frees them instead of keeping each one
/// allocated behind a dead weak entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settled_index_keeps_no_captured_source_allocated() {
    let fixture = GitFixture::new(&[
        ("src/main.rs", "fn main() { helper(); }\n"),
        ("src/helper.rs", "pub fn helper() {}\n"),
        ("src/other.rs", "pub fn other() {}\n"),
    ]);
    let store = TempDir::new().expect("store root");
    let (registry, _) =
        mounted_core_query_worktree_in(CodeIndexSchedulerRegistryV1::new(1), &fixture, &store)
            .await;
    wait_for_settled_owner(&registry, fixture.path()).await;

    let stats = registry.byte_pool_stats();
    assert_eq!(
        (stats.source_allocations, stats.live_sources),
        (0, 0),
        "{stats:?}"
    );

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
async fn a_refused_text_build_retries_when_a_reservation_it_waited_on_drops() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let (registry, blocker, mut receipts) =
        mount_with_refused_text_build(&fixture, &store, &owners).await;

    drop(blocker);

    assert_eq!(
        next_receipt_trigger(&mut receipts).await,
        CodeIndexCadenceTriggerV1::MemoryHeadroom
    );
    let text = wait_for_queryable_text_generation(&registry, fixture.path()).await;
    assert!(
        text.query_owners_are_ready(),
        "the dropped reservation, not an owner release, woke the refused build"
    );

    registry.shutdown().await;
}

/// A worktree serving its first generation whose refresh after a commit the
/// ledger refuses, still refused well after the reconcile's bounded capacity
/// retries (well under a second in tests) would have been spent. Returns the
/// first generation.
async fn refresh_refused_for_memory(
    registry: &CodeIndexSchedulerRegistryV1,
    fixture: &GitFixture,
) -> CodeGenerationId {
    let first = wait_for_live_complete_generation(registry, fixture.path())
        .await
        .generation()
        .manifest()
        .generation_id
        .clone();
    fixture.edit("src/main.rs", "fn main() { second(); }\nfn second() {}\n");
    git(fixture.path(), &["commit", "-qam", "refresh"]);
    assert!(matches!(
        registry
            .notify_path(fixture.path(), fixture.path().join("src/main.rs"))
            .await,
        super::super::CodeIndexDemandAdmissionV1::Queued
    ));
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(first.clone()),
        "the refresh stays refused while the ledger is held"
    );
    first
}

/// The ledger with all but 1 MiB of its room taken by `held`.
fn ledger_leaving_one_mib(
    resident_memory: &Arc<ProcessResidentMemoryV1>,
    held: &'static str,
) -> ProcessSharedMemoryReservationV1 {
    let limit = resident_memory.snapshot().limit_bytes;
    let occupied = resident_memory
        .snapshot()
        .used_bytes
        .max(resident_memory.pressure().measure_admission_bytes());
    resident_memory
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new(held).unwrap(),
            NonZeroU64::new(limit - occupied - 1024 * 1024).unwrap(),
        )
        .expect("the held memory fits the ledger")
}

fn sixteen_gib_authority() -> Arc<ProcessResidentMemoryV1> {
    let limit = NonZeroU64::new(16 * 1024 * 1024 * 1024).unwrap();
    Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::new(ResidentMemoryPressureV1::new(limit)),
    ))
}

/// Issue #2707: the refresh stopped retrying and the idle release that later
/// gave its memory back never woke it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_refused_for_memory_publishes_when_an_idle_owner_releases_it() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let resident_memory = sixteen_gib_authority();
    let (registry, _) = mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::with_resident_memory(1, Arc::clone(&resident_memory))
            .with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
    )
    .await;
    // The mount's last pass releases its own reservations as it finishes.
    wait_for_settled_owner(&registry, fixture.path()).await;
    // Another worktree's serving decode: pressure cannot shed it, only its
    // idle window ending gives it back.
    let held = ledger_leaving_one_mib(&resident_memory, "test-other-worktree-decode");
    let held = Arc::new(HeldMemoryOwner {
        bytes: held.reserved_bytes(),
        held: std::sync::Mutex::new(Some(held)),
        last_used: Instant::now(),
        serving: true,
    });
    let held_owner: Arc<dyn ResidentOwnerV1> = Arc::clone(&held) as Arc<dyn ResidentOwnerV1>;
    let other = WorktreeId::new("worktree.other").unwrap();
    let _registration = owners
        .register(
            ResidentOwnerScopeV1 {
                project_id: ProjectId::new("project.other").unwrap(),
                worktree_id: other.clone(),
            },
            ResidentOwnerKindV1::DecodedGeneration,
            Arc::downgrade(&held_owner),
        )
        .unwrap();
    let first = refresh_refused_for_memory(&registry, &fixture).await;

    let released = owners.release_idle(Instant::now() + IDLE_WINDOW);
    assert_eq!(
        released
            .iter()
            .filter(|release| release.scope.worktree_id == other)
            .map(|release| (release.kind, release.cause))
            .collect::<Vec<_>>(),
        [(
            ResidentOwnerKindV1::DecodedGeneration,
            ResidentOwnerReleaseCauseV1::Idle
        )],
        "the other worktree's decode outlived the refusal and went idle"
    );

    let second = tokio::time::timeout(
        Duration::from_secs(30),
        wait_for_generation_change(&registry, fixture.path(), &first),
    )
    .await
    .expect("the idle release is the refused refresh's retry");
    let seated = wait_for_live_complete_generation(&registry, fixture.path()).await;
    assert_eq!(seated.generation().manifest().generation_id, second);

    registry.shutdown().await;
}

/// A reservation dropping with no owner released, as when another
/// worktree's build finishes, is the retry of a refresh the ledger refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_refused_by_the_ledger_publishes_when_the_blocking_reservation_drops() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    let resident_memory = sixteen_gib_authority();
    let (registry, _) = mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::with_resident_memory(1, Arc::clone(&resident_memory))
            .with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
    )
    .await;
    wait_for_settled_owner(&registry, fixture.path()).await;
    let blocker = ledger_leaving_one_mib(&resident_memory, "test-other-worktree-build");
    let first = refresh_refused_for_memory(&registry, &fixture).await;

    drop(blocker);

    let second = tokio::time::timeout(
        Duration::from_secs(30),
        wait_for_generation_change(&registry, fixture.path(), &first),
    )
    .await
    .expect("the dropped reservation is the refused refresh's retry");
    let seated = wait_for_live_complete_generation(&registry, fixture.path()).await;
    assert_eq!(seated.generation().manifest().generation_id, second);

    registry.shutdown().await;
}

/// Issue #2907: a refresh refused for memory reads as one typed blocked
/// state, the deferred advisory mount's "is a generation there" check (run on
/// every worker transition) neither renews the decode's residency lease nor
/// starts a pass, and the release the refusal waits on publishes the refresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_refresh_waits_for_a_release_without_passive_reads_holding_its_decode() {
    let fixture = GitFixture::new(&[("src/main.rs", "fn main() {}\n")]);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(IDLE_WINDOW));
    // A fixed process reading, so the held ledger refuses the refresh however
    // much memory the tests beside this one hold.
    let limit = NonZeroU64::new(16 * 1024 * 1024 * 1024).unwrap();
    let process = ProcessResidentSampleV1 {
        resident_bytes: 256 * 1024 * 1024,
        unreclaimable_bytes: 256 * 1024 * 1024,
        swapped_bytes: 0,
        cgroup_committed_bytes: None,
    };
    let resident_memory = Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::new(ResidentMemoryPressureV1::with_sampler(
            limit,
            Arc::new(move || Some(process)),
        )),
    ));
    let (registry, scope) = mounted_core_query_worktree_in(
        CodeIndexSchedulerRegistryV1::with_resident_memory(1, Arc::clone(&resident_memory))
            .with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
    )
    .await;
    wait_for_settled_owner(&registry, fixture.path()).await;
    let blocker = ledger_leaving_one_mib(&resident_memory, "test-other-worktree-build");
    let mut receipts = registry.subscribe_cadence_receipts();
    let first = refresh_refused_for_memory(&registry, &fixture).await;
    let blocked_reason = || async {
        registry
            .dashboard_freshness(fixture.path())
            .await
            .expect("mounted worktree freshness")
            .parked
            .and_then(|parked| parked.blocked_reason)
    };
    let Ok(blocked) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(reason) = blocked_reason().await {
                return reason;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    else {
        panic!(
            "the refused refresh never read as a typed blocked state: {:?}",
            registry.dashboard_freshness(fixture.path()).await
        );
    };
    assert_eq!(
        blocked,
        tracedecay_contracts::code_index_freshness::CodeIndexBuildBlockedReasonV1::ResidentMemory
    );
    wait_for_worker_phase(&registry, fixture.path(), CodeIndexWorkerPhaseV1::Parked).await;

    receipts.borrow_and_update();
    let refused_at = Instant::now();
    for _ in 0..20 {
        assert!(
            registry
                .latest_feedback_generation_for_scope(fixture.path(), &scope)
                .await
                .is_none(),
            "the stale served generation is not current feedback evidence"
        );
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !receipts.has_changed().unwrap(),
        "a passive check cannot help a refresh waiting for memory, so no pass runs"
    );
    let idle_released = owners
        .release_idle(refused_at + IDLE_WINDOW)
        .into_iter()
        .filter(|release| release.scope.worktree_id == scope.worktree_id)
        .map(|release| release.kind)
        .collect::<Vec<_>>();
    assert!(
        idle_released.contains(&ResidentOwnerKindV1::DecodedGeneration),
        "the feedback checks renewed the decode's lease: released {idle_released:?}"
    );

    drop(blocker);
    let second = tokio::time::timeout(
        Duration::from_secs(30),
        wait_for_generation_change(&registry, fixture.path(), &first),
    )
    .await
    .expect("the dropped reservation is the refused refresh's retry");
    assert_eq!(
        wait_for_live_complete_generation(&registry, fixture.path())
            .await
            .generation()
            .manifest()
            .generation_id,
        second
    );
    assert_eq!(blocked_reason().await, None);

    registry.shutdown().await;
}

/// Retained state another worktree holds, modelled as a ledger reservation so
/// releasing it gives the build real headroom. A `serving` holding is
/// protected from pressure inside its idle window.
struct HeldMemoryOwner {
    held: std::sync::Mutex<Option<ProcessSharedMemoryReservationV1>>,
    bytes: u64,
    last_used: Instant,
    serving: bool,
}

impl ResidentOwnerV1 for HeldMemoryOwner {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        self.held
            .lock()
            .unwrap()
            .is_some()
            .then(|| ResidentOwnerSampleV1 {
                holding: ResidentHoldingV1::Generation(
                    CodeGenerationId::new("generation.v1.superseded").unwrap(),
                ),
                bytes: ResidentOwnerBytesV1::Measured(self.bytes),
                last_used: self.last_used,
                serving: self.serving,
                shared: None,
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
        serving: false,
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

#[tokio::test(start_paused = true)]
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

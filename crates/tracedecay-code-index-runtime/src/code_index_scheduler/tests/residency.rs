use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1;
use tracedecay_runtime_core::resident_memory::{
    ResidentOwnerKindV1, ResidentOwnerReleaseCauseV1, ResidentOwnersReportV1, ResidentOwnersV1,
};

use super::{
    CodeIndexSchedulerRegistryV1, GitFixture, core_search_request, git,
    mounted_core_query_worktree_in, wait_for_generation_change, wait_for_live_complete_generation,
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

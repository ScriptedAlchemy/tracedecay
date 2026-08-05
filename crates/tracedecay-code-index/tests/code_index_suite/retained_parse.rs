use std::{
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use tracedecay_code_extraction::incremental::{ParseDocumentIdentity, ParseLimits, ParseReuse};
use tracedecay_code_index::retained_parse::{RetainedParsePoolLimits, SharedRetainedParsePool};
use tracedecay_domain::{
    CommitId, ProjectId, RefId, RepositoryDirtyStateV1, RepositoryId, TreeId, WorktreeId,
};

use crate::support::id;

fn identity(worktree: &str, commit: &str, tree: &str) -> ParseDocumentIdentity {
    ParseDocumentIdentity::Repository {
        project_id: id::<ProjectId>("project.retained"),
        repository_id: id::<RepositoryId>("repository.retained"),
        worktree_id: Some(id::<WorktreeId>(worktree)),
        reference: Some(id::<RefId>("refs/heads/main")),
        commit: Some(id::<CommitId>(commit)),
        tree: Some(id::<TreeId>(tree)),
        dirty: RepositoryDirtyStateV1::Dirty,
        logical_path: "src/lib.rs".to_owned(),
    }
}

#[test]
fn saved_indexing_reuses_one_tree_only_within_the_exact_checkout() {
    let pool = SharedRetainedParsePool::default();
    let initial = pool
        .parse(
            identity("worktree.one", "commit-a", "tree-a"),
            "rust",
            "fn one() -> u32 { 1 }\nfn two() -> u32 { 2 }\n",
        )
        .expect("initial parse");
    let increment = pool
        .parse(
            identity("worktree.one", "commit-a", "tree-a"),
            "rust",
            "fn one() -> u32 { 1 }\nfn two() -> u32 { 20 }\n",
        )
        .expect("incremental parse");
    let other_worktree = pool
        .parse(
            identity("worktree.two", "commit-a", "tree-a"),
            "rust",
            "fn one() -> u32 { 1 }\nfn two() -> u32 { 20 }\n",
        )
        .expect("independent parse");

    assert_eq!(initial.reuse, ParseReuse::Initial);
    assert_eq!(increment.reuse, ParseReuse::Incremental);
    assert!(increment.metrics.changed_bytes < increment.metrics.source_bytes);
    assert_eq!(other_worktree.reuse, ParseReuse::Initial);
    let stats = pool.stats();
    assert_eq!(stats.initial_parses, 2);
    assert_eq!(stats.incremental_parses, 1);
    assert_eq!(stats.retained_documents, 2);
}

#[test]
fn retained_pool_eviction_and_failure_preserve_truthful_bounded_state() {
    let pool = SharedRetainedParsePool::new(RetainedParsePoolLimits {
        max_documents: 1,
        max_total_source_bytes: 64,
        document: ParseLimits {
            max_source_bytes: 32,
            max_changed_ranges: 8,
            max_parse_time: Duration::from_millis(250),
        },
    })
    .expect("valid pool limits");
    pool.parse(
        identity("worktree.one", "commit-a", "tree-a"),
        "rust",
        "fn one() {}\n",
    )
    .expect("first parse");
    pool.parse(
        identity("worktree.two", "commit-a", "tree-a"),
        "rust",
        "fn two() {}\n",
    )
    .expect("second parse evicts first");

    let error = pool.parse(
        identity("worktree.two", "commit-b", "tree-b"),
        "rust",
        "fn two() { let value = 12345678901234567890; }\n",
    );
    assert!(error.is_err());
    let no_op = pool
        .parse(
            identity("worktree.two", "commit-b", "tree-b"),
            "rust",
            "fn two() {}\n",
        )
        .expect("failed update retained prior tree");

    assert_eq!(no_op.reuse, ParseReuse::Noop);
    let stats = pool.stats();
    assert_eq!(stats.evicted_documents, 1);
    assert_eq!(stats.failed_parses, 1);
    assert_eq!(stats.retained_documents, 1);
    assert!(stats.retained_source_bytes <= 64);
}

#[test]
fn concurrent_first_admission_keeps_one_tree_and_one_initial_parse() {
    let pool = SharedRetainedParsePool::default();
    let start = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let pool = pool.clone();
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                pool.parse(
                    identity("worktree.concurrent", "commit-a", "tree-a"),
                    "rust",
                    "fn concurrent() -> u32 { 1 }\n",
                )
            })
        })
        .collect::<Vec<_>>();
    start.wait();
    let reports = handles
        .into_iter()
        .map(|handle| handle.join().expect("parse worker").expect("parse"))
        .collect::<Vec<_>>();

    assert_eq!(
        reports
            .iter()
            .filter(|report| report.reuse == ParseReuse::Initial)
            .count(),
        1
    );
    assert_eq!(
        reports
            .iter()
            .filter(|report| report.reuse == ParseReuse::Noop)
            .count(),
        1
    );
    let stats = pool.stats();
    assert_eq!(stats.initial_parses, 1);
    assert_eq!(stats.noop_parses, 1);
    assert_eq!(stats.retained_documents, 1);
}

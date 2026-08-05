use std::{
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use tracedecay_code_extraction::{
    AstroExtractor, LanguageExtractor, RustExtractor,
    incremental::{ParseDocumentIdentity, ParseLimits, ParseReuse},
    parsed_extraction::ParsedExtractionDisposition,
};
use tracedecay_code_index::retained_parse::{RetainedParsePoolLimits, SharedRetainedParsePool};
use tracedecay_domain::{
    CommitId, ExtractionResult, ProjectId, RefId, RepositoryDirtyStateV1, RepositoryId, TreeId,
    WorktreeId,
};

use crate::support::id;

fn identity(worktree: &str, commit: &str, tree: &str) -> ParseDocumentIdentity {
    identity_at(worktree, commit, tree, "src/lib.rs")
}

fn identity_at(
    worktree: &str,
    commit: &str,
    tree: &str,
    logical_path: &str,
) -> ParseDocumentIdentity {
    ParseDocumentIdentity::Repository {
        project_id: id::<ProjectId>("project.retained"),
        repository_id: id::<RepositoryId>("repository.retained"),
        worktree_id: Some(id::<WorktreeId>(worktree)),
        reference: Some(id::<RefId>("refs/heads/main")),
        commit: Some(id::<CommitId>(commit)),
        tree: Some(id::<TreeId>(tree)),
        dirty: RepositoryDirtyStateV1::Dirty,
        logical_path: logical_path.to_owned(),
    }
}

fn normalize_extraction(mut result: ExtractionResult) -> ExtractionResult {
    for node in &mut result.nodes {
        node.updated_at = 0;
    }
    result.duration_ms = 0;
    result.sanitize();
    result
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
fn incremental_extraction_matches_cold_canonical_rows_and_visits_only_changed_rust_root() {
    let pool = SharedRetainedParsePool::default();
    let extractor = RustExtractor;
    let before = "fn one() -> u32 { 1 }\nfn two() -> u32 { 2 }\n";
    let after = "fn one() -> u32 { 1 }\nfn two() -> u32 { 20 }\n";

    let (_, initial) = pool
        .parse_and_extract(
            identity("worktree.rows", "commit-a", "tree-a"),
            "rust",
            before,
            &extractor,
        )
        .expect("initial canonical extraction");
    let (report, incremental) = pool
        .parse_and_extract(
            identity("worktree.rows", "commit-b", "tree-b"),
            "rust",
            after,
            &extractor,
        )
        .expect("incremental canonical extraction");

    assert_eq!(report.reuse, ParseReuse::Incremental);
    assert_eq!(
        incremental.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );
    assert!(incremental.metrics.visited_top_level_nodes <= 2);
    assert!(incremental.metrics.visited_bytes < after.len());
    assert_eq!(
        normalize_extraction(incremental.result),
        normalize_extraction(extractor.extract("src/lib.rs", after))
    );
    assert_eq!(
        initial.disposition,
        ParsedExtractionDisposition::FullDocument
    );
    let stats = pool.stats();
    assert_eq!(stats.full_extractions, 1);
    assert_eq!(stats.incremental_extractions, 1);
    assert_eq!(stats.reset_extractions, 0);
}

#[test]
fn composite_source_masking_preserves_incremental_astro_canonical_rows() {
    let pool = SharedRetainedParsePool::default();
    let extractor = AstroExtractor;
    let before = "---\nconst title = 'old';\n---\n<h1>{title}</h1>\n";
    let after = "---\nconst title = 'new';\n---\n<h1>{title}</h1>\n";

    pool.parse_and_extract(
        identity_at("worktree.astro", "commit-a", "tree-a", "src/page.astro"),
        "astro",
        before,
        &extractor,
    )
    .expect("initial Astro extraction");
    let (report, incremental) = pool
        .parse_and_extract(
            identity_at("worktree.astro", "commit-b", "tree-b", "src/page.astro"),
            "astro",
            after,
            &extractor,
        )
        .expect("incremental Astro extraction");

    assert_eq!(report.reuse, ParseReuse::Incremental);
    assert_eq!(
        incremental.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );
    assert_eq!(
        normalize_extraction(incremental.result),
        normalize_extraction(extractor.extract("src/page.astro", after))
    );
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

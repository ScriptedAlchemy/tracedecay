//! Canonical extraction rows are byte-identical whether a document is
//! extracted cold (full-document traversal) or incrementally (changed-region
//! re-extraction merged with prior rows). This is the identity contract behind
//! generation reuse: the traversal path must never leak into row order.

use std::fmt::Write as _;
use std::time::Duration;

use tracedecay_code_extraction::{
    RustExtractor,
    incremental::{ParseDocumentIdentity, ParseLimits},
};
use tracedecay_code_index::retained_parse::{RetainedParsePoolLimits, SharedRetainedParsePool};
use tracedecay_domain::{
    CommitId, ExtractionResult, ProjectId, RefId, RepositoryDirtyStateV1, RepositoryId, TreeId,
    WorktreeId,
};

fn identity(commit: &str, tree: &str) -> ParseDocumentIdentity {
    ParseDocumentIdentity::Repository {
        project_id: ProjectId::new("project.canonical-identity").expect("project id"),
        repository_id: RepositoryId::new("repository.canonical-identity").expect("repository id"),
        worktree_id: Some(WorktreeId::new("worktree.canonical-identity").expect("worktree id")),
        reference: Some(RefId::new("refs/heads/evaluation").expect("ref id")),
        commit: Some(CommitId::new(commit).expect("commit id")),
        tree: Some(TreeId::new(tree).expect("tree id")),
        dirty: RepositoryDirtyStateV1::Dirty,
        logical_path: "src/generated.rs".to_owned(),
    }
}

/// Attributed functions exercise the ordering trap: the `AnnotationUsage` row
/// starts on an earlier line than its `Function` row, so emission order and
/// positional order disagree unless one canonical comparator owns both paths.
fn source_with_literal(function_count: usize, literal: &str) -> String {
    let mut source = String::with_capacity(function_count * 64);
    for index in 0..function_count {
        let value = if index == function_count / 2 {
            literal
        } else {
            "1"
        };
        writeln!(
            source,
            "#[inline]\npub fn generated_{index}() -> usize {{ {value} }}\n"
        )
        .expect("writing to a String cannot fail");
    }
    source
}

fn pool() -> SharedRetainedParsePool {
    SharedRetainedParsePool::new(RetainedParsePoolLimits {
        max_documents: 2,
        max_total_source_bytes: 8 * 1024 * 1024,
        document: ParseLimits {
            max_source_bytes: 4 * 1024 * 1024,
            max_changed_ranges: 256,
            max_parse_time: Duration::from_secs(10),
        },
    })
    .expect("pool")
}

fn canonical(mut result: ExtractionResult) -> String {
    for node in &mut result.nodes {
        node.updated_at = 0;
    }
    result.duration_ms = 0;
    result.sanitize();
    serde_json::to_string(&result).expect("serialize extraction result")
}

#[test]
fn incremental_rows_equal_cold_extraction_bytes() {
    let before = source_with_literal(100, "1");
    let after = source_with_literal(100, "123456");
    let extractor = RustExtractor;

    let cold_pool = pool();
    let (_, cold) = cold_pool
        .parse_and_extract(
            identity("commit-cold", "tree-cold"),
            "rust",
            &after,
            &extractor,
        )
        .expect("cold parse");

    let incremental_pool = pool();
    incremental_pool
        .parse_and_extract(
            identity("commit-before", "tree-before"),
            "rust",
            &before,
            &extractor,
        )
        .expect("before parse");
    let (_, incremental) = incremental_pool
        .parse_and_extract(
            identity("commit-after", "tree-after"),
            "rust",
            &after,
            &extractor,
        )
        .expect("incremental parse");

    assert_eq!(
        canonical(cold.result),
        canonical(incremental.result),
        "incremental canonical rows must match cold extraction byte-for-byte"
    );
}

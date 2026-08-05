use std::time::Duration;

use tracedecay_code_extraction::RustExtractor;
use tracedecay_code_extraction::incremental::{
    ParseCompleteness, ParseDocumentIdentity, ParseError, ParseInputEdit, ParseLimits,
    ParsePartialReason, ParsePoint, ParseResetReason, ParseReuse, RetainedParseDocument,
};
use tracedecay_code_extraction::parsed_extraction::ParsedExtractionDisposition;
use tracedecay_domain::{
    CommitId, ContentDigest, ManifestDigest, ProjectId, RefId, RepositoryDirtyStateV1,
    RepositoryId, TreeId, WorktreeId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned()).unwrap_or_else(|error| panic!("{value}: {error}"))
}

fn identity(commit: &str, tree: &str, dirty: RepositoryDirtyStateV1) -> ParseDocumentIdentity {
    identity_in_worktree(commit, tree, dirty, "worktree.incremental")
}

fn identity_in_worktree(
    commit: &str,
    tree: &str,
    dirty: RepositoryDirtyStateV1,
    worktree: &str,
) -> ParseDocumentIdentity {
    ParseDocumentIdentity::Repository {
        project_id: id::<ProjectId>("project.incremental"),
        repository_id: id::<RepositoryId>("repository.incremental"),
        worktree_id: Some(id::<WorktreeId>(worktree)),
        reference: Some(id::<RefId>("refs/heads/main")),
        commit: Some(id::<CommitId>(commit)),
        tree: Some(id::<TreeId>(tree)),
        dirty,
        logical_path: "src/lib.rs".to_owned(),
    }
}

fn point_for(source: &str, byte: usize) -> ParsePoint {
    let prefix = &source[..byte];
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix
        .rfind('\n')
        .map_or(prefix.len(), |line_start| prefix.len() - line_start - 1);
    ParsePoint { row, column }
}

#[test]
fn retained_tree_reparse_reports_only_the_edited_function_range() {
    let before = "fn unchanged() -> u32 { 1 }\n\nfn edited() -> u32 { 2 }\n";
    let after = "fn unchanged() -> u32 { 1 }\n\nfn edited() -> u32 { 20 }\n";
    let (mut document, opened) = RetainedParseDocument::open(
        identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "rust",
        before,
        ParseLimits::default(),
    )
    .expect("initial parse");
    assert_eq!(opened.reuse, ParseReuse::Initial);

    let start = before.find("2 }").expect("edited literal");
    let edit = ParseInputEdit {
        start_byte: start,
        old_end_byte: start + 1,
        new_end_byte: start + 2,
        start_position: point_for(before, start),
        old_end_position: point_for(before, start + 1),
        new_end_position: point_for(after, start + 2),
    };
    let report = document
        .apply_edits(
            identity("commit-a", "tree-a", RepositoryDirtyStateV1::Dirty),
            &[edit],
            after,
        )
        .expect("incremental parse");

    assert_eq!(report.reuse, ParseReuse::Incremental);
    assert_eq!(report.completeness, ParseCompleteness::Complete);
    assert!(report.metrics.reused_prior_tree);
    assert_eq!(report.metrics.input_edit_count, 1);
    assert!(report.metrics.changed_bytes < after.len());
    assert!(report.changed_ranges.iter().all(|range| {
        range.start_byte >= before.find("fn edited").expect("edited function")
            && range.end_byte <= after.len()
    }));
    assert_eq!(document.source(), after);
}

#[test]
fn invalid_ordered_edit_is_atomic() {
    let source = "fn main() {}\n";
    let (mut document, _) = RetainedParseDocument::open(
        identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "rust",
        source,
        ParseLimits::default(),
    )
    .expect("initial parse");
    let invalid = ParseInputEdit {
        start_byte: source.len() + 1,
        old_end_byte: source.len() + 1,
        new_end_byte: source.len() + 1,
        start_position: ParsePoint { row: 1, column: 1 },
        old_end_position: ParsePoint { row: 1, column: 1 },
        new_end_position: ParsePoint { row: 1, column: 1 },
    };

    assert!(matches!(
        document.apply_edits(
            identity("commit-a", "tree-a", RepositoryDirtyStateV1::Dirty),
            &[invalid],
            source,
        ),
        Err(ParseError::InvalidEdit { .. })
    ));
    assert_eq!(document.source(), source);
    assert!(matches!(
        document.identity(),
        ParseDocumentIdentity::Repository {
            dirty: RepositoryDirtyStateV1::Clean,
            ..
        }
    ));
}

#[test]
fn full_replacement_is_a_typed_reset() {
    let (mut document, _) = RetainedParseDocument::open(
        identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "rust",
        "fn before() {}\n",
        ParseLimits::default(),
    )
    .expect("initial parse");

    let report = document
        .replace(
            identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
            "fn after() {}\n",
        )
        .expect("replacement parse");

    assert_eq!(
        report.reuse,
        ParseReuse::Reset {
            reason: ParseResetReason::FullReplacement
        }
    );
    assert!(!report.metrics.reused_prior_tree);
}

#[test]
fn syntax_errors_and_changed_range_caps_are_truthful_partial_states() {
    let limits = ParseLimits {
        max_changed_ranges: 0,
        ..ParseLimits::default()
    };
    let before = "fn main() {}\n";
    let after = "fn main( {\n";
    let (mut document, _) = RetainedParseDocument::open(
        identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "rust",
        before,
        limits,
    )
    .expect("initial parse");

    let report = document
        .reparse(
            identity("commit-a", "tree-a", RepositoryDirtyStateV1::Dirty),
            after,
        )
        .expect("incremental error tree remains inspectable");

    let ParseCompleteness::Partial { reasons } = report.completeness else {
        panic!("syntax error and range truncation must be partial");
    };
    assert!(reasons.contains(&ParsePartialReason::SyntaxErrors));
    assert!(reasons.iter().any(|reason| matches!(
        reason,
        ParsePartialReason::ChangedRangesTruncated { returned: 0, total } if *total > 0
    )));
    assert!(report.changed_ranges.is_empty());
}

#[test]
fn unsupported_language_oversize_and_deadline_are_distinct() {
    let source = "fn main() {}\n";
    assert!(matches!(
        RetainedParseDocument::open(
            identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
            "not-a-language",
            source,
            ParseLimits::default(),
        ),
        Err(ParseError::UnsupportedLanguage { .. })
    ));

    let tiny = ParseLimits {
        max_source_bytes: source.len() - 1,
        ..ParseLimits::default()
    };
    assert!(matches!(
        RetainedParseDocument::open(
            identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
            "rust",
            source,
            tiny,
        ),
        Err(ParseError::SourceTooLarge { .. })
    ));

    let expired = ParseLimits {
        max_parse_time: Duration::ZERO,
        ..ParseLimits::default()
    };
    assert!(matches!(
        RetainedParseDocument::open(
            identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
            "rust",
            source,
            expired,
        ),
        Err(ParseError::TimedOut { .. })
    ));
}

#[test]
fn retained_tree_never_crosses_repository_worktree_or_path_identity() {
    let source = "fn main() {}\n";
    let (mut document, _) = RetainedParseDocument::open(
        identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "rust",
        source,
        ParseLimits::default(),
    )
    .expect("initial parse");
    let foreign = identity_in_worktree(
        "commit-a",
        "tree-a",
        RepositoryDirtyStateV1::Dirty,
        "worktree.foreign",
    );

    assert!(matches!(
        document.reparse(foreign, "fn main() { let _x = 1; }\n"),
        Err(ParseError::IdentityMismatch)
    ));
    assert_eq!(document.source(), source);
}

#[test]
fn session_overlay_reuses_only_within_exact_scope_and_document_identity() {
    let before = "fn main() { let value = 1; }\n";
    let after = "fn main() { let value = 2; }\n";
    let overlay = |scope: u8, version: i64, content: u8| ParseDocumentIdentity::SessionOverlay {
        scope_identity: id::<ManifestDigest>(&format!("sha256:{scope:064x}")),
        document_identity: id::<ManifestDigest>(&format!("sha256:{:064x}", 10)),
        version,
        content_digest: id::<ContentDigest>(&format!("sha256:{content:064x}")),
        logical_path: "src/main.rs".to_owned(),
    };
    let (mut document, _) =
        RetainedParseDocument::open(overlay(1, 1, 11), "rust", before, ParseLimits::default())
            .expect("initial overlay parse");

    let report = document
        .reparse(overlay(1, 2, 12), after)
        .expect("same session document may advance");
    assert_eq!(report.reuse, ParseReuse::Incremental);
    assert!(report.metrics.reused_prior_tree);

    assert!(matches!(
        document.reparse(overlay(2, 3, 13), "fn main() { let value = 3; }\n"),
        Err(ParseError::IdentityMismatch)
    ));
    assert_eq!(document.source(), after);
}

#[test]
fn canonical_reextraction_visits_only_changed_top_level_syntax() {
    let before = "fn unchanged() -> u32 { 1 }\n\nfn edited() -> u32 { 2 }\n";
    let after = "fn unchanged() -> u32 { 1 }\n\nasync fn edited() -> u32 { 2 }\n";
    let (mut document, opened) = RetainedParseDocument::open(
        identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "rust",
        before,
        ParseLimits::default(),
    )
    .expect("initial parse");
    let initial = document
        .extract_canonical(&RustExtractor, &opened, None)
        .expect("initial canonical extraction");
    assert_eq!(
        initial.disposition,
        ParsedExtractionDisposition::FullDocument
    );

    let report = document
        .reparse(
            identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
            after,
        )
        .expect("incremental parse");
    let increment = document
        .extract_canonical(&RustExtractor, &report, Some(&initial.result))
        .expect("incremental canonical extraction");

    assert_eq!(
        increment.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );
    assert_eq!(increment.metrics.visited_top_level_nodes, 1);
    assert!(increment.metrics.visited_bytes < after.len());
    let edited = increment
        .result
        .nodes
        .iter()
        .find(|node| node.name == "edited")
        .expect("edited function");
    assert!(edited.is_async);
    assert!(matches!(
        document.extract_canonical(&RustExtractor, &opened, Some(&initial.result)),
        Err(ParseError::StaleReport)
    ));
}

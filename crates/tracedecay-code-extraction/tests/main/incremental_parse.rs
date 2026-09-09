use std::time::Duration;

use tracedecay_code_extraction::incremental::{
    ParseCompleteness, ParseDocumentIdentity, ParseError, ParseInputEdit, ParseLimits,
    ParsePartialReason, ParsePoint, ParseResetReason, ParseReuse, RetainedParseDocument,
};
use tracedecay_code_extraction::parsed_extraction::{
    ParsedExtractionDisposition, ParsedExtractionResetReason,
};
use tracedecay_code_extraction::{
    ExtractionArtifactV1, ImportModuleKindV1, ImportNamespaceV1, LanguageExtractor, RustExtractor,
    TypeScriptExtractor,
};
use tracedecay_domain::{
    CommitId, ContentDigest, ExtractionResult, ManifestDigest, NodeKind, ProjectId, RefId,
    RepositoryDirtyStateV1, RepositoryId, SourceSpan, TreeId, WorktreeId,
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

fn typescript_identity(
    commit: &str,
    tree: &str,
    dirty: RepositoryDirtyStateV1,
) -> ParseDocumentIdentity {
    ParseDocumentIdentity::Repository {
        project_id: id::<ProjectId>("project.incremental"),
        repository_id: id::<RepositoryId>("repository.incremental"),
        worktree_id: Some(id::<WorktreeId>("worktree.incremental")),
        reference: Some(id::<RefId>("refs/heads/main")),
        commit: Some(id::<CommitId>(commit)),
        tree: Some(id::<TreeId>(tree)),
        dirty,
        logical_path: "src/imports.ts".to_owned(),
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

fn assert_artifact_rows_match_fresh_parse(
    incremental: &ExtractionArtifactV1,
    fresh: &ExtractionArtifactV1,
) {
    let mut incremental_result = incremental.result.clone();
    let mut fresh_result = fresh.result.clone();
    incremental_result.duration_ms = 0;
    fresh_result.duration_ms = 0;
    for node in &mut incremental_result.nodes {
        node.updated_at = 0;
    }
    for node in &mut fresh_result.nodes {
        node.updated_at = 0;
    }

    assert_eq!(incremental_result.nodes, fresh_result.nodes);
    assert_eq!(incremental_result.edges, fresh_result.edges);
    assert_eq!(
        incremental_result.unresolved_refs,
        fresh_result.unresolved_refs
    );
    assert_eq!(incremental_result.errors, fresh_result.errors);
    assert_eq!(incremental.imports, fresh.imports);
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

#[test]
fn same_line_column_shifts_reextract_following_top_level_syntax() {
    let before = "fn a() -> u32 { 1 } fn b() -> u32 { 2 }\n";
    let after = "fn longer() -> u32 { 1 } fn b() -> u32 { 2 }\n";
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

    let report = document
        .reparse(
            identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
            after,
        )
        .expect("same-line incremental parse");
    let incremental = document
        .extract_canonical(&RustExtractor, &report, Some(&initial.result))
        .expect("same-line canonical extraction");
    let following = incremental
        .result
        .nodes
        .iter()
        .find(|node| node.name == "b")
        .expect("following function");

    assert_eq!(incremental.metrics.visited_top_level_nodes, 2);
    assert_eq!(
        following.start_column as usize,
        after.find("fn b").expect("b")
    );
}

/// Two same-line methods share kind, name, and line. Re-extracting only the
/// second one's `impl` must not hand it the first one's identity: the merged
/// rows must equal a cold extraction of the edited source.
#[test]
fn same_line_method_edit_keeps_both_methods_distinct_after_merge() {
    let before = "fn alpha() {} fn beta() {} fn gamma() {} impl A { fn run() { alpha(); } } impl B { fn run() { beta(); } }\n";
    let after = "fn alpha() {} fn beta() {} fn gamma() {} impl A { fn run() { alpha(); } } impl B { fn run() { gamma(); } }\n";
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

    let report = document
        .reparse(
            identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
            after,
        )
        .expect("same-line incremental parse");
    assert_eq!(report.reuse, ParseReuse::Incremental);
    let incremental = document
        .extract_canonical(&RustExtractor, &report, Some(&initial.result))
        .expect("same-line canonical extraction");
    assert_eq!(
        incremental.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );
    assert_eq!(incremental.metrics.visited_top_level_nodes, 1);

    let mut cold = RustExtractor.extract("src/lib.rs", after);
    let mut merged = incremental.result;
    for result in [&mut cold, &mut merged] {
        result.duration_ms = 0;
        for node in &mut result.nodes {
            node.updated_at = 0;
        }
    }
    assert_eq!(
        serde_json::to_string(&merged).expect("merged rows"),
        serde_json::to_string(&cold).expect("cold rows"),
        "the merged same-line rows must equal a cold extraction of the edited source"
    );
    let callee_by_owner = merged
        .unresolved_refs
        .iter()
        .map(|reference| {
            let owner = merged
                .nodes
                .iter()
                .find(|node| node.id == reference.from_node_id)
                .expect("reference owner");
            (
                owner.qualified_name.clone(),
                reference.reference_name.clone(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(callee_by_owner.contains(&("src/lib.rs::A::run".to_owned(), "alpha".to_owned())));
    assert!(callee_by_owner.contains(&("src/lib.rs::B::run".to_owned(), "gamma".to_owned())));
}

/// Canonical rows with the runtime-only timing fields zeroed, as JSON so the
/// whole row set (file root span included) is compared at once.
fn timeless_rows(result: &ExtractionResult) -> String {
    let mut result = result.clone();
    result.duration_ms = 0;
    for node in &mut result.nodes {
        node.updated_at = 0;
    }
    serde_json::to_string(&result).expect("canonical rows")
}

fn file_root_end_line(result: &ExtractionResult) -> u32 {
    result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File)
        .expect("file root row")
        .end_line
}

/// The file root's line span is read off the retained tree, so every
/// line-ending shape must produce the rows a cold extraction of the same
/// source produces: on the initial parse, after a same-line edit that merges
/// through the changed-region path, and after a line-changing edit that takes
/// the multiline reset path. The last shape moves the terminating newline
/// without changing the row delta, so the file root's `end_line` changes on a
/// same-line edit — reusing the prior span would be wrong there.
#[test]
fn file_root_span_matches_cold_extraction_for_every_line_ending_shape() {
    let shapes = [
        ("fn a() -> u32 { 1 }", "fn a() -> u32 { 12 }"),
        ("fn a() -> u32 { 1 }\n", "fn a() -> u32 { 12 }\n"),
        ("fn a() -> u32 { 1 }\n\n\n", "fn a() -> u32 { 12 }\n\n\n"),
        (
            "fn a() -> u32 { 1 }\r\nfn b() {}\r\n",
            "fn a() -> u32 { 12 }\r\nfn b() {}\r\n",
        ),
        (
            "fn a() -> u32 { 1 } // é界\nfn b() {}",
            "fn a() -> u32 { 12 } // é界\nfn b() {}",
        ),
        ("", "fn a() {}"),
        ("fn a() {}\nfn b() {}", "fn a() {}fn b() {}\n"),
    ];
    for (before, after) in shapes {
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
        let cold_before = RustExtractor.extract("src/lib.rs", before);
        assert_eq!(
            timeless_rows(&initial.result),
            timeless_rows(&cold_before),
            "initial rows for {before:?}"
        );
        assert_eq!(
            file_root_end_line(&initial.result),
            before.lines().count().saturating_sub(1) as u32,
            "initial file root for {before:?}"
        );

        let report = document
            .reparse(
                identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
                after,
            )
            .expect("same-line incremental parse");
        assert_eq!(
            report.reuse,
            ParseReuse::Incremental,
            "{before:?} -> {after:?}"
        );
        let incremental = document
            .extract_canonical(&RustExtractor, &report, Some(&initial.result))
            .expect("same-line canonical extraction");
        assert_eq!(
            incremental.disposition,
            ParsedExtractionDisposition::ChangedRegions,
            "{before:?} -> {after:?}"
        );
        let cold_after = RustExtractor.extract("src/lib.rs", after);
        assert_eq!(
            timeless_rows(&incremental.result),
            timeless_rows(&cold_after),
            "same-line merged rows for {before:?} -> {after:?}"
        );
        assert_eq!(
            file_root_end_line(&incremental.result),
            after.lines().count().saturating_sub(1) as u32,
            "same-line file root for {after:?}"
        );

        let multiline = format!("{after}\nfn c() {{}}\n");
        let report = document
            .reparse(
                identity("commit-c", "tree-c", RepositoryDirtyStateV1::Dirty),
                multiline.as_str(),
            )
            .expect("multiline incremental parse");
        let reset = document
            .extract_canonical(&RustExtractor, &report, Some(&incremental.result))
            .expect("multiline canonical extraction");
        assert_eq!(
            reset.disposition,
            ParsedExtractionDisposition::Reset {
                reason: ParsedExtractionResetReason::MultilineEdit
            },
            "{after:?} -> {multiline:?}"
        );
        let cold_multiline = RustExtractor.extract("src/lib.rs", &multiline);
        assert_eq!(
            timeless_rows(&reset.result),
            timeless_rows(&cold_multiline),
            "reset rows for {multiline:?}"
        );
        assert_eq!(
            file_root_end_line(&reset.result),
            multiline.lines().count().saturating_sub(1) as u32,
            "reset file root for {multiline:?}"
        );
    }
}

#[test]
fn incremental_edit_after_same_line_import_matches_fresh_parser_artifact() {
    let before = "import type { Foo } from \"./foo\"; export const tail = 1;\n";
    let after = "import type { Foo } from \"./foo\"; export const tail = 2;\n";
    let (mut document, opened) = RetainedParseDocument::open(
        typescript_identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "typescript",
        before,
        ParseLimits::default(),
    )
    .expect("initial TypeScript parse");
    let initial = document
        .extract_canonical_artifact(&TypeScriptExtractor, &opened, None)
        .expect("initial extraction artifact");

    let report = document
        .reparse(
            typescript_identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
            after,
        )
        .expect("later same-line edit");
    assert_eq!(report.reuse, ParseReuse::Incremental);
    let incremental = document
        .extract_canonical_artifact(&TypeScriptExtractor, &report, Some(&initial.artifact))
        .expect("incremental extraction artifact");
    assert_eq!(
        incremental.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );

    assert_eq!(
        incremental
            .artifact
            .result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Use)
            .count(),
        1,
        "editing later syntax must retain the preceding raw import statement"
    );
    assert_eq!(incremental.artifact.imports.len(), 1);
    assert_eq!(incremental.artifact.imports[0].module_specifier, "./foo");

    let fresh = TypeScriptExtractor.extract_artifact("src/imports.ts", after);
    assert_artifact_rows_match_fresh_parse(&incremental.artifact, &fresh);
}

#[test]
fn incremental_edit_of_duplicate_module_import_matches_fresh_parser_artifact() {
    let before = "import type { A } from \"x\"; import { b } from \"x\";\n";
    let after = "import type { A } from \"x\"; import { c } from \"x\";\n";
    let (mut document, opened) = RetainedParseDocument::open(
        typescript_identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "typescript",
        before,
        ParseLimits::default(),
    )
    .expect("initial duplicate-module TypeScript parse");
    let initial = document
        .extract_canonical_artifact(&TypeScriptExtractor, &opened, None)
        .expect("initial duplicate-module extraction artifact");

    let report = document
        .reparse(
            typescript_identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
            after,
        )
        .expect("later duplicate-module import edit");
    assert_eq!(report.reuse, ParseReuse::Incremental);
    let incremental = document
        .extract_canonical_artifact(&TypeScriptExtractor, &report, Some(&initial.artifact))
        .expect("incremental duplicate-module extraction artifact");
    assert_eq!(
        incremental.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );

    assert_eq!(
        incremental
            .artifact
            .result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Use)
            .count(),
        2,
        "editing the later import must retain both same-module Use statements"
    );
    assert_eq!(
        incremental
            .artifact
            .imports
            .iter()
            .map(|row| (row.imported_name.as_deref(), row.namespace))
            .collect::<Vec<_>>(),
        vec![
            (Some("A"), ImportNamespaceV1::Type),
            (Some("c"), ImportNamespaceV1::Value),
        ],
        "the unchanged preceding type row and edited value row must both survive"
    );

    let fresh = TypeScriptExtractor.extract_artifact("src/imports.ts", after);
    assert_artifact_rows_match_fresh_parse(&incremental.artifact, &fresh);
}

#[test]
fn incremental_edit_after_multiline_import_closing_line_matches_fresh_parser_artifact() {
    let before = concat!(
        "import type {\n",
        "  Foo,\n",
        "} from \"./foo\"; export const tail = 1;\n",
    );
    let after = concat!(
        "import type {\n",
        "  Foo,\n",
        "} from \"./foo\"; export const tail = 2;\n",
    );
    let (mut document, opened) = RetainedParseDocument::open(
        typescript_identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "typescript",
        before,
        ParseLimits::default(),
    )
    .expect("initial multiline TypeScript parse");
    let initial = document
        .extract_canonical_artifact(&TypeScriptExtractor, &opened, None)
        .expect("initial multiline extraction artifact");

    let report = document
        .reparse(
            typescript_identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
            after,
        )
        .expect("later closing-line edit");
    assert_eq!(report.reuse, ParseReuse::Incremental);
    let incremental = document
        .extract_canonical_artifact(&TypeScriptExtractor, &report, Some(&initial.artifact))
        .expect("incremental multiline extraction artifact");
    assert_eq!(
        incremental.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );

    assert_eq!(
        incremental
            .artifact
            .result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Use)
            .count(),
        1,
        "editing later closing-line syntax must retain the multiline import statement"
    );
    assert_eq!(incremental.artifact.imports.len(), 1);
    assert_eq!(
        incremental.artifact.imports[0].imported_name.as_deref(),
        Some("Foo")
    );

    let fresh = TypeScriptExtractor.extract_artifact("src/imports.ts", after);
    assert_artifact_rows_match_fresh_parse(&incremental.artifact, &fresh);
}

#[test]
fn incremental_import_artifact_does_not_keep_stale_rows_after_add_change_and_delete() {
    let without_import = concat!(
        "export const untouched = 1;\n",
        "\n",
        "export const tail = 2;\n",
    );
    let with_type_import = concat!(
        "export const untouched = 1;\n",
        "import type { Foo } from \"./foo\";\n",
        "export const tail = 2;\n",
    );
    let with_value_import = concat!(
        "export const untouched = 1;\n",
        "import { Bar as Baz } from \"pkg\";\n",
        "export const tail = 2;\n",
    );
    let (mut document, opened) = RetainedParseDocument::open(
        typescript_identity("commit-a", "tree-a", RepositoryDirtyStateV1::Clean),
        "typescript",
        without_import,
        ParseLimits::default(),
    )
    .expect("initial TypeScript parse");
    let initial = document
        .extract_canonical_artifact(&TypeScriptExtractor, &opened, None)
        .expect("initial extraction artifact");
    assert!(
        initial.artifact.result.errors.is_empty(),
        "initial extraction errors: {:?}",
        initial.artifact.result.errors
    );
    assert!(initial.artifact.imports.is_empty());

    let added_report = document
        .reparse(
            typescript_identity("commit-b", "tree-b", RepositoryDirtyStateV1::Dirty),
            with_type_import,
        )
        .expect("incremental import addition");
    assert_eq!(added_report.reuse, ParseReuse::Incremental);
    let added = document
        .extract_canonical_artifact(&TypeScriptExtractor, &added_report, Some(&initial.artifact))
        .expect("added import artifact");
    assert!(
        added.artifact.result.errors.is_empty(),
        "added extraction errors: {:?}",
        added.artifact.result.errors
    );
    assert_eq!(
        added.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );
    assert_eq!(
        added
            .artifact
            .imports
            .iter()
            .map(|row| (
                row.logical_path.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            ))
            .collect::<Vec<_>>(),
        vec![(
            "src/imports.ts",
            "./foo",
            Some("Foo"),
            Some("Foo"),
            ImportNamespaceV1::Type,
            ImportModuleKindV1::ProjectRelative,
            SourceSpan {
                start_byte: 42,
                end_byte: 45,
            },
            1,
            14,
        )]
    );

    let changed_report = document
        .reparse(
            typescript_identity("commit-c", "tree-c", RepositoryDirtyStateV1::Dirty),
            with_value_import,
        )
        .expect("incremental import change");
    assert_eq!(changed_report.reuse, ParseReuse::Incremental);
    let changed = document
        .extract_canonical_artifact(&TypeScriptExtractor, &changed_report, Some(&added.artifact))
        .expect("changed import artifact");
    assert!(
        changed.artifact.result.errors.is_empty(),
        "changed extraction errors: {:?}",
        changed.artifact.result.errors
    );
    assert_eq!(
        changed.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );
    assert_eq!(
        changed
            .artifact
            .imports
            .iter()
            .map(|row| (
                row.logical_path.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            ))
            .collect::<Vec<_>>(),
        vec![(
            "src/imports.ts",
            "pkg",
            Some("Bar"),
            Some("Baz"),
            ImportNamespaceV1::Value,
            ImportModuleKindV1::BareModule,
            SourceSpan {
                start_byte: 37,
                end_byte: 47,
            },
            1,
            9,
        )]
    );

    let deleted_report = document
        .reparse(
            typescript_identity("commit-d", "tree-d", RepositoryDirtyStateV1::Dirty),
            without_import,
        )
        .expect("incremental import deletion");
    assert_eq!(deleted_report.reuse, ParseReuse::Incremental);
    let deleted = document
        .extract_canonical_artifact(
            &TypeScriptExtractor,
            &deleted_report,
            Some(&changed.artifact),
        )
        .expect("deleted import artifact");
    assert!(
        deleted.artifact.result.errors.is_empty(),
        "deleted extraction errors: {:?}",
        deleted.artifact.result.errors
    );
    assert_eq!(
        deleted.disposition,
        ParsedExtractionDisposition::ChangedRegions
    );
    assert!(
        deleted.artifact.imports.is_empty(),
        "deleted import rows must not survive incremental merge: {:#?}",
        deleted.artifact.imports
    );
}

#[test]
fn admitted_parse_continuation_preserves_incremental_rows_and_prior_state_on_abort() {
    let source = (0..2_000)
        .map(|n| format!("fn item_{n}() -> u64 {{ {n} }}\n"))
        .collect::<String>();
    let identity = identity("commit.parse", "tree.parse", RepositoryDirtyStateV1::Clean);
    let limits = ParseLimits {
        max_parse_time: Duration::from_nanos(1),
        ..ParseLimits::default()
    };
    let checks = std::cell::Cell::new(0);
    let admitted = || {
        checks.set(checks.get() + 1);
        true
    };
    let (mut document, _) = RetainedParseDocument::open_prepared_with_control(
        identity.clone(),
        "rust",
        "rust",
        &source,
        &source,
        limits,
        Some(&admitted),
    )
    .unwrap();
    assert!(checks.get() > 10, "many quanta must have resumed");
    let changed = source.replace("{ 100 }", "{ 101 }");
    let report = document
        .reparse_prepared_with_control(identity.clone(), &changed, &changed, Some(&admitted))
        .unwrap();
    assert_eq!(report.reuse, ParseReuse::Incremental);
    let artifact = document
        .extract_canonical_artifact(&RustExtractor, &report, None)
        .unwrap()
        .artifact;
    let fresh = RustExtractor.extract_artifact("src/lib.rs", &changed);
    assert_artifact_rows_match_fresh_parse(&artifact, &fresh);
    let abort = || false;
    assert!(matches!(
        document.reparse_prepared_with_control(identity.clone(), &source, &source, Some(&abort)),
        Err(ParseError::TimedOut { .. })
    ));
    assert_eq!(document.source(), changed);
    document
        .reparse_prepared_with_control(identity, &source, &source, Some(&admitted))
        .unwrap();
    assert_eq!(document.source(), source);
}

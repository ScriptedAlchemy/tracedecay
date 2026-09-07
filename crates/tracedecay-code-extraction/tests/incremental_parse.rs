use std::time::Duration;

use tracedecay_code_extraction::incremental::{
    ParseCompleteness, ParseDocumentIdentity, ParseError, ParseInputEdit, ParseLimits,
    ParsePartialReason, ParsePoint, ParseResetReason, ParseReuse, RetainedParseDocument,
};
use tracedecay_code_extraction::parsed_extraction::ParsedExtractionDisposition;
use tracedecay_code_extraction::{
    ExtractionArtifactV1, ImportModuleKindV1, ImportNamespaceV1, LanguageExtractor, RustExtractor,
    TypeScriptExtractor,
};
use tracedecay_domain::{
    CommitId, ContentDigest, ManifestDigest, NodeKind, ProjectId, RefId, RepositoryDirtyStateV1,
    RepositoryId, SourceSpan, TreeId, WorktreeId,
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

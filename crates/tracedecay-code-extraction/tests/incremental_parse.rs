use std::time::Duration;

use tracedecay_code_extraction::incremental::{
    MAX_RETAINED_PARSE_SOURCE_BYTES, ParseCompleteness, ParseDocumentIdentity, ParseError,
    ParseInputEdit, ParseLimits, ParsePartialReason, ParsePoint, ParseReuse, RetainedParseDocument,
};
use tracedecay_domain::{ContentDigest, ManifestDigest};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned()).unwrap_or_else(|error| panic!("{value}: {error}"))
}

fn identity(
    scope: u8,
    document: u8,
    generation: u64,
    version: i64,
    content: &str,
) -> ParseDocumentIdentity {
    ParseDocumentIdentity::new(
        id::<ManifestDigest>(&format!("sha256:{scope:064x}")),
        id::<ManifestDigest>(&format!("sha256:{document:064x}")),
        generation,
        version,
        ContentDigest::of_bytes(content.as_bytes()),
        "src/lib.rs".to_owned(),
    )
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
fn retained_tree_reports_only_the_edited_function_range() {
    let before = "fn unchanged() -> u32 { 1 }\n\nfn edited() -> u32 { 2 }\n";
    let after = "fn unchanged() -> u32 { 1 }\n\nfn edited() -> u32 { 20 }\n";
    let (mut document, opened) = RetainedParseDocument::open(
        identity(1, 2, 1, 1, before),
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
            identity(1, 2, 1, 2, after),
            &[edit],
            after,
            ParseLimits::default(),
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
fn invalid_ordered_edit_preserves_the_previous_source_identity_and_tree() {
    let source = "fn main() {}\n";
    let after = "fn main() { let value = 1; }\n";
    let original = identity(1, 2, 1, 1, source);
    let (mut document, _) =
        RetainedParseDocument::open(original.clone(), "rust", source, ParseLimits::default())
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
            identity(1, 2, 1, 2, after),
            &[invalid],
            after,
            ParseLimits::default(),
        ),
        Err(ParseError::InvalidEdit { .. })
    ));
    assert_eq!(document.source(), source);
    assert_eq!(document.identity(), &original);

    let report = document
        .reparse(identity(1, 2, 1, 2, after), after, ParseLimits::default())
        .expect("the retained pre-failure tree remains reusable");
    assert_eq!(report.reuse, ParseReuse::Incremental);
    assert!(report.metrics.reused_prior_tree);
}

#[test]
fn timeout_during_update_never_commits_partial_retained_state() {
    let before = "fn before() {}\n";
    let after = "fn after() {}\n";
    let original = identity(1, 2, 1, 1, before);
    let (mut document, _) =
        RetainedParseDocument::open(original.clone(), "rust", before, ParseLimits::default())
            .expect("initial parse");

    assert_eq!(
        document.reparse(
            identity(1, 2, 1, 2, after),
            after,
            ParseLimits {
                max_parse_time: Duration::ZERO,
                ..ParseLimits::default()
            },
        ),
        Err(ParseError::TimedOut {
            limit: Duration::ZERO
        })
    );
    assert_eq!(document.source(), before);
    assert_eq!(document.identity(), &original);

    let report = document
        .reparse(identity(1, 2, 1, 2, after), after, ParseLimits::default())
        .expect("the tree retained before cancellation remains reusable");
    assert_eq!(report.reuse, ParseReuse::Incremental);
    assert!(report.metrics.reused_prior_tree);
}

#[test]
fn whole_document_replacement_requires_and_advances_document_generation() {
    let before = "fn before() {}\n";
    let after = "fn after() {}\n";
    let (mut document, _) = RetainedParseDocument::open(
        identity(1, 2, 1, 1, before),
        "rust",
        before,
        ParseLimits::default(),
    )
    .expect("initial parse");

    assert_eq!(
        document.replace(identity(1, 2, 1, 2, after), after, ParseLimits::default(),),
        Err(ParseError::DocumentGenerationNotAdvanced {
            current: 1,
            received: 1,
        })
    );
    let report = document
        .replace(identity(1, 2, 2, 2, after), after, ParseLimits::default())
        .expect("replacement parse");

    assert_eq!(report.reuse, ParseReuse::Reset);
    assert!(!report.metrics.reused_prior_tree);
    assert_eq!(document.identity().document_generation(), 2);
}

#[test]
fn syntax_errors_and_changed_range_caps_are_truthful_partial_states() {
    let limits = ParseLimits {
        max_changed_ranges: 0,
        ..ParseLimits::default()
    };
    let before = "fn main() {}\n";
    let after = "fn main( {\n";
    let (mut document, _) =
        RetainedParseDocument::open(identity(1, 2, 1, 1, before), "rust", before, limits)
            .expect("initial parse");

    let report = document
        .reparse(identity(1, 2, 1, 2, after), after, limits)
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
fn production_policy_enforces_source_and_parse_time_boundaries() {
    let at_limit = " ".repeat(MAX_RETAINED_PARSE_SOURCE_BYTES);
    assert!(matches!(
        RetainedParseDocument::open(
            identity(1, 2, 1, 1, &at_limit),
            "unsupported-policy-probe",
            &at_limit,
            ParseLimits::default(),
        ),
        Err(ParseError::UnsupportedLanguage { .. })
    ));
    let over_limit = format!("{at_limit} ");
    assert!(matches!(
        RetainedParseDocument::open(
            identity(1, 2, 1, 1, &over_limit),
            "unsupported-policy-probe",
            &over_limit,
            ParseLimits::default(),
        ),
        Err(ParseError::SourceTooLarge {
            size,
            limit: MAX_RETAINED_PARSE_SOURCE_BYTES,
        }) if size == MAX_RETAINED_PARSE_SOURCE_BYTES + 1
    ));

    let policy = ParseLimits::default();
    assert!(!policy.parse_deadline_expired(Duration::from_millis(249)));
    assert!(policy.parse_deadline_expired(Duration::from_millis(250)));
}

#[test]
fn unsupported_language_and_zero_budget_are_distinct_unavailable_states() {
    let source = "fn main() {}\n";
    assert!(matches!(
        RetainedParseDocument::open(
            identity(1, 2, 1, 1, source),
            "not-a-language",
            source,
            ParseLimits::default(),
        ),
        Err(ParseError::UnsupportedLanguage { .. })
    ));
    assert!(matches!(
        RetainedParseDocument::open(
            identity(1, 2, 1, 1, source),
            "rust",
            source,
            ParseLimits {
                max_parse_time: Duration::ZERO,
                ..ParseLimits::default()
            },
        ),
        Err(ParseError::TimedOut { .. })
    ));
}

#[test]
fn retained_tree_never_crosses_scope_document_generation_or_path_identity() {
    let source = "fn main() {}\n";
    let after = "fn main() { let value = 1; }\n";
    let (mut document, _) = RetainedParseDocument::open(
        identity(1, 2, 1, 1, source),
        "rust",
        source,
        ParseLimits::default(),
    )
    .expect("initial parse");

    for foreign in [
        identity(9, 2, 1, 2, after),
        identity(1, 9, 1, 2, after),
        identity(1, 2, 2, 2, after),
    ] {
        assert_eq!(
            document.reparse(foreign, after, ParseLimits::default()),
            Err(ParseError::IdentityMismatch)
        );
        assert_eq!(document.source(), source);
    }
}

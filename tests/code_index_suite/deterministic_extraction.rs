use tracedecay::code_index::extract::{
    ExtractionCancellation, LanguageExtractor, MAX_EXTRACTION_SOURCE_BYTES, NeverCancelled,
    TreeSitterExtractor,
};
use tracedecay_domain::{ExtractionFailureV1, ParseOutcomeV1};

use crate::support::{RUST_SOURCE, rust_descriptor, validated_rust_file};

struct AlwaysCancelled;

impl ExtractionCancellation for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn extraction_is_deterministic_and_revision_bound() {
    let extractor = TreeSitterExtractor::new();
    let descriptor = rust_descriptor();
    let file = validated_rust_file(RUST_SOURCE.as_bytes());

    let first = extractor
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("first extraction");
    let second = extractor
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("second extraction");

    assert_eq!(first.batch(), second.batch());
    assert_eq!(first.batch().parse_outcome, ParseOutcomeV1::Complete);
    assert_eq!(
        first.batch().descriptor_revision,
        descriptor.descriptor_revision
    );
    assert_eq!(first.batch().grammar_revision, descriptor.grammar_revision);
    assert_eq!(
        first.batch().extractor_revision,
        descriptor.extractor_revision
    );
    assert!(first.batch().coverage.symbols_extracted > 0);
    assert!(first.batch().coverage.relations_extracted > 0);
}

#[test]
fn extraction_reports_cancellation_after_sanitized_intake() {
    let extractor = TreeSitterExtractor::new();
    let descriptor = rust_descriptor();
    let source = validated_rust_file(RUST_SOURCE.as_bytes());

    assert!(matches!(
        extractor.extract(&source, &descriptor, &AlwaysCancelled),
        Err(ExtractionFailureV1::Cancelled)
    ));
}

#[test]
fn extraction_caps_large_sources_as_partial_evidence() {
    let extractor = TreeSitterExtractor::new();
    let descriptor = rust_descriptor();
    let source = RUST_SOURCE.repeat((MAX_EXTRACTION_SOURCE_BYTES / RUST_SOURCE.len()) + 2);
    let file = validated_rust_file(source.as_bytes());

    let extraction = extractor
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("bounded extraction succeeds");
    let batch = extraction.batch();

    assert!(matches!(
        &batch.parse_outcome,
        ParseOutcomeV1::Partial { reason } if reason.contains("source byte cap")
    ));
    assert_eq!(batch.parsed_ranges.len(), 1);
    assert_eq!(batch.unsupported_ranges.len(), 1);
    assert_eq!(
        batch.parsed_ranges[0].end_byte,
        batch.unsupported_ranges[0].start_byte
    );
    assert_eq!(batch.unsupported_ranges[0].end_byte, source.len() as u64);
    assert_eq!(
        batch.coverage.parsed_bytes + batch.coverage.unsupported_bytes,
        source.len() as u64
    );
}

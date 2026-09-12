use std::collections::BTreeMap;

use tracedecay_code_index::chunks::DeterministicCodeChunker;
use tracedecay_code_index::extract::{LanguageExtractor, NeverCancelled, TreeSitterExtractor};
use tracedecay_domain::{
    ChunkerRevision, ContentDigest, RepositoryId, SanitizerRevision, SourceSpan, SymbolOccurrenceId,
};
use tracedecay_privacy::{CodeSourceShapeV1, sanitize_code_source_bytes};

use crate::support::{id, rust_descriptor, validated_rust_file};

const SOURCE: &str = r#"fn source_edit_execution_problem(error: String) -> Result<String, String> {
    Ok(error)
}

/// Convert a typed source-edit execution failure into its daemon response.
#[inline]
fn map_source_edit_execution_error(
    request_id: String,
    error: String,
) -> Result<String, String> {
    match source_edit_execution_problem(error) {
        Ok(problem) => Ok(format!("{request_id}:{problem}")),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
fn following_function() {}
"#;

#[test]
fn published_symbol_digests_cover_their_recorded_source_spans() {
    let admitted = sanitize_code_source_bytes(SOURCE.as_bytes(), CodeSourceShapeV1::CodeOrProse)
        .expect("source is admitted by the production sanitizer");
    let (sanitized_bytes, _) = admitted.into_parts();
    let sanitized_source =
        String::from_utf8(sanitized_bytes).expect("sanitized Rust source remains UTF-8");
    let file = validated_rust_file(sanitized_source.as_bytes());
    let descriptor = rust_descriptor();
    let extraction = TreeSitterExtractor::new()
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("extract Rust source");
    let artifacts = DeterministicCodeChunker::new(
        file.generation_id.clone(),
        id::<RepositoryId>("repo.fixture"),
        id::<SanitizerRevision>("sanitizer.v1"),
        id("policy.v1"),
        id::<ChunkerRevision>("chunker.v1"),
        tracedecay_code_extraction::LanguageRegistry::new(),
    )
    .index_file(&file, extraction.batch(), &descriptor, &NeverCancelled)
    .expect("index Rust source");

    let mut published_spans = BTreeMap::<SymbolOccurrenceId, SourceSpan>::new();
    for chunk in &artifacts.chunks.chunks {
        let Some(symbol) = &chunk.anchor.symbol_occurrence_id else {
            continue;
        };
        published_spans
            .entry(symbol.clone())
            .and_modify(|span| {
                span.start_byte = span.start_byte.min(chunk.anchor.source_span.start_byte);
                span.end_byte = span.end_byte.max(chunk.anchor.source_span.end_byte);
            })
            .or_insert(chunk.anchor.source_span);
    }

    assert!(!artifacts.symbols.is_empty(), "fixture publishes symbols");
    for symbol in &artifacts.symbols {
        let span = published_spans
            .get(&symbol.occurrence)
            .unwrap_or_else(|| panic!("{} has a published source span", symbol.qualified_name));
        let observed = ContentDigest::of_bytes(
            &sanitized_source.as_bytes()[span.start_byte as usize..span.end_byte as usize],
        );
        assert_eq!(
            observed, symbol.content_digest,
            "{} digest must cover its recorded source span {span:?}",
            symbol.qualified_name
        );
    }
}

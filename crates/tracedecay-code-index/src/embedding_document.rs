//! Embedding-document composition for the semantic projection.
//!
//! The semantic lane embeds one document per canonical chunk. Under
//! [`EmbeddingDocumentCompositionV1::SanitizedText`] the document is the
//! chunk's sanitized text, unchanged. Under
//! [`EmbeddingDocumentCompositionV1::SymbolContextHeader`] a deterministic
//! header naming the chunk's symbol and its enclosing scope precedes the text,
//! and the whole document is bounded by the projection's per-document byte
//! budget so composing never moves the canonical inference group boundaries.
//!
//! Header content comes only from the generation's symbol index, whose records
//! the chunker derived from the same sanitized bytes as the chunk text, and
//! only from fields pinned by the chunk's own identity: the symbol kind and
//! qualified name are inputs to the chunk id. A vector the projector reuses
//! by chunk id and content digest therefore can never carry a stale header.
//! Signature, visibility, and documentation are deliberately absent: a
//! multi-line signature can change without changing a signature-grain chunk's
//! first-line text, so including it would let a reused vector describe a
//! symbol that no longer exists.

use std::sync::Arc;

use thiserror::Error;
use tracedecay_domain::canonical_text::is_canonical_text;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, CodeSearchChunkV1, EmbeddingDocumentCompositionV1,
    EmbeddingProjectionKeyV1, SensitivityLevelV1, SymbolOccurrenceId,
};

use crate::chunks::snap_down;
use crate::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};

/// The header may take at most this share of the document byte budget; the
/// body keeps the remainder.
const HEADER_BUDGET_DIVISOR: usize = 4;
/// Replaces the tail of a header that does not fit its budget. It carries the
/// header's closing newline so a truncated header still ends a line.
const HEADER_TRUNCATION_MARK: &str = "...\n";
const SYMBOL_LINE_PREFIX: &str = "symbol: ";
const SCOPE_LINE_PREFIX: &str = "scope: ";
/// Separators a qualified name may end in once its simple-name suffix is
/// removed, across the extractor languages.
const SCOPE_SEPARATORS: &[char] = &[':', '.', '#', '/', '$'];

/// Why a symbol-backed chunk was embedded without its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbeddingHeaderWithholdReasonV1 {
    /// The chunk's sensitivity decision is not `Public` or `Internal`; only
    /// the body, whose bytes the sanitizer already ruled on, is embedded.
    Sensitivity,
    /// A symbol field is empty or carries control characters after one-line
    /// normalization, so it cannot be rendered.
    NonCanonicalSymbolText,
    /// The header budget cannot hold even a truncated header.
    BudgetExhausted,
}

/// What the composed document carries ahead of the chunk text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbeddingDocumentHeaderV1 {
    /// The composition has no header.
    NotComposed,
    /// A symbol-context header was rendered.
    Rendered,
    /// A file-grain chunk has no symbol to describe.
    NoSymbol,
    /// A symbol-backed chunk was embedded body-only.
    Withheld(EmbeddingHeaderWithholdReasonV1),
}

/// One composed tensor input plus the typed header outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingDocumentV1 {
    text: String,
    header: EmbeddingDocumentHeaderV1,
}

impl EmbeddingDocumentV1 {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn header(&self) -> EmbeddingDocumentHeaderV1 {
        self.header
    }

    pub fn into_text(self) -> String {
        self.text
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EmbeddingDocumentErrorV1 {
    #[error("chunk {chunk_id} is not from the symbol index's generation {generation_id}")]
    ForeignGeneration {
        chunk_id: CodeSearchChunkId,
        generation_id: CodeGenerationId,
    },
    #[error("chunk {chunk_id} names symbol occurrence {occurrence} that its generation does not")]
    MissingSymbolRecord {
        chunk_id: CodeSearchChunkId,
        occurrence: SymbolOccurrenceId,
    },
    #[error("embedding document budget is not admissible: {0}")]
    Budget(String),
    #[error("chunk {chunk_id} keeps no sanitized text within its document budget")]
    EmptyDocument { chunk_id: CodeSearchChunkId },
}

/// The symbol records of one generation, keyed by occurrence for header
/// composition. Records are `Arc`-shared with the generation they came from.
#[derive(Clone, Debug)]
pub struct EmbeddingSymbolContextIndexV1 {
    generation_id: CodeGenerationId,
    symbols: Vec<Arc<LineageSymbolRecordV1>>,
}

impl EmbeddingSymbolContextIndexV1 {
    pub fn from_generation_symbols(index: &GenerationSymbolIndexV1) -> Self {
        let mut symbols = index.symbols.clone();
        symbols.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
        Self {
            generation_id: index.generation_id.clone(),
            symbols,
        }
    }

    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    fn record(&self, occurrence: &SymbolOccurrenceId) -> Option<&LineageSymbolRecordV1> {
        self.symbols
            .binary_search_by(|record| record.occurrence.cmp(occurrence))
            .ok()
            .map(|index| self.symbols[index].as_ref())
    }
}

/// Composes the tensor input for every chunk of one generation under the
/// composition named by the admitted projection key.
#[derive(Clone, Debug)]
pub struct EmbeddingDocumentComposerV1 {
    symbols: EmbeddingSymbolContextIndexV1,
}

impl EmbeddingDocumentComposerV1 {
    pub fn new(symbols: EmbeddingSymbolContextIndexV1) -> Self {
        Self { symbols }
    }

    pub fn symbols(&self) -> &EmbeddingSymbolContextIndexV1 {
        &self.symbols
    }

    /// Compose one chunk's document. `SanitizedText` consults nothing beyond
    /// the chunk, so it keeps the shipped composition's exact behavior and
    /// failure surface.
    pub fn compose(
        &self,
        key: &EmbeddingProjectionKeyV1,
        chunk: &CodeSearchChunkV1,
    ) -> Result<EmbeddingDocumentV1, EmbeddingDocumentErrorV1> {
        match key.document_composition {
            EmbeddingDocumentCompositionV1::SanitizedText => Ok(EmbeddingDocumentV1 {
                text: chunk.sanitized_text.as_str().to_owned(),
                header: EmbeddingDocumentHeaderV1::NotComposed,
            }),
            EmbeddingDocumentCompositionV1::SymbolContextHeader => {
                self.compose_with_symbol_context_header(key, chunk)
            }
        }
    }

    fn compose_with_symbol_context_header(
        &self,
        key: &EmbeddingProjectionKeyV1,
        chunk: &CodeSearchChunkV1,
    ) -> Result<EmbeddingDocumentV1, EmbeddingDocumentErrorV1> {
        if chunk.anchor.generation_id != self.symbols.generation_id {
            return Err(EmbeddingDocumentErrorV1::ForeignGeneration {
                chunk_id: chunk.id.clone(),
                generation_id: self.symbols.generation_id.clone(),
            });
        }
        let budget = key
            .document_byte_budget()
            .map_err(|error| EmbeddingDocumentErrorV1::Budget(error.to_string()))?;
        let (header, outcome) = match &chunk.anchor.symbol_occurrence_id {
            None => (String::new(), EmbeddingDocumentHeaderV1::NoSymbol),
            Some(occurrence) => {
                let record = self.symbols.record(occurrence).ok_or_else(|| {
                    EmbeddingDocumentErrorV1::MissingSymbolRecord {
                        chunk_id: chunk.id.clone(),
                        occurrence: occurrence.clone(),
                    }
                })?;
                if !header_admits_sensitivity(chunk.sensitivity.level) {
                    (
                        String::new(),
                        EmbeddingDocumentHeaderV1::Withheld(
                            EmbeddingHeaderWithholdReasonV1::Sensitivity,
                        ),
                    )
                } else {
                    match render_symbol_context_header(record, budget) {
                        Ok(header) => (header, EmbeddingDocumentHeaderV1::Rendered),
                        Err(reason) => (String::new(), EmbeddingDocumentHeaderV1::Withheld(reason)),
                    }
                }
            }
        };
        // The header never exceeds a quarter of the budget, so the body always
        // keeps at least three quarters.
        let body_budget = budget.saturating_sub(header.len());
        let text = chunk.sanitized_text.as_str();
        let body = if text.len() <= body_budget {
            text
        } else {
            &text[..snap_down(text, body_budget)]
        };
        if body.is_empty() {
            return Err(EmbeddingDocumentErrorV1::EmptyDocument {
                chunk_id: chunk.id.clone(),
            });
        }
        let mut document = header;
        document.push_str(body);
        Ok(EmbeddingDocumentV1 {
            text: document,
            header: outcome,
        })
    }
}

/// Only chunks the sanitizer accepted outright, or that policy classed as
/// internal, carry symbol metadata into the model input. Redacted files had
/// credential-shaped bytes removed; their identifiers embed only through the
/// sanitized body.
fn header_admits_sensitivity(level: SensitivityLevelV1) -> bool {
    matches!(
        level,
        SensitivityLevelV1::Public | SensitivityLevelV1::Internal
    )
}

/// Render one symbol-context header within a quarter of `document_budget`.
///
/// Line order is fixed (`symbol:`, then `scope:` when the symbol has one),
/// every value is collapsed to one line, and a header longer than its budget
/// keeps its longest char-boundary prefix followed by [`HEADER_TRUNCATION_MARK`].
pub fn render_symbol_context_header(
    record: &LineageSymbolRecordV1,
    document_budget: usize,
) -> Result<String, EmbeddingHeaderWithholdReasonV1> {
    let kind = one_line(&record.kind)?;
    let name = one_line(&record.simple_name)?;
    let qualified = one_line(&record.qualified_name)?;
    let mut rendered = format!("{SYMBOL_LINE_PREFIX}{kind} {name}\n");
    if let Some(scope) = scope_breadcrumb(&qualified, &name) {
        rendered.push_str(SCOPE_LINE_PREFIX);
        rendered.push_str(scope);
        rendered.push('\n');
    }
    let header_budget = document_budget / HEADER_BUDGET_DIVISOR;
    if rendered.len() <= header_budget {
        return Ok(rendered);
    }
    let Some(keep_budget) = header_budget.checked_sub(HEADER_TRUNCATION_MARK.len()) else {
        return Err(EmbeddingHeaderWithholdReasonV1::BudgetExhausted);
    };
    let keep = snap_down(&rendered, keep_budget);
    if keep == 0 {
        return Err(EmbeddingHeaderWithholdReasonV1::BudgetExhausted);
    }
    rendered.truncate(keep);
    rendered.push_str(HEADER_TRUNCATION_MARK);
    Ok(rendered)
}

/// Collapse every whitespace run to one space and trim; the result must be
/// canonical text (non-empty, no control characters).
fn one_line(value: &str) -> Result<String, EmbeddingHeaderWithholdReasonV1> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if is_canonical_text(&collapsed) {
        Ok(collapsed)
    } else {
        Err(EmbeddingHeaderWithholdReasonV1::NonCanonicalSymbolText)
    }
}

/// The enclosing scope of a symbol: its qualified name without the trailing
/// simple name and separator. A qualified name that does not end in the
/// simple name is kept whole, because it is the only breadcrumb the extractor
/// attested.
fn scope_breadcrumb<'a>(qualified: &'a str, name: &str) -> Option<&'a str> {
    if qualified == name {
        return None;
    }
    match qualified.strip_suffix(name).map(str::trim_end) {
        Some(prefix) if prefix.ends_with(SCOPE_SEPARATORS) => {
            let scope = prefix.trim_end_matches(|character: char| {
                SCOPE_SEPARATORS.contains(&character) || character == ' '
            });
            (!scope.is_empty()).then_some(scope)
        }
        _ => Some(qualified),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        BoundedSanitizedText, ChunkerRevision, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
        ContentDigest, EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1,
        EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingTruncationSideV1, FileIdentityDigest,
        FileOccurrenceId, LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId,
        PrivacyDomainId, SanitizerRevision, SensitivityDecision, SourceSpan, SymbolIdentityDigest,
    };

    const BATCH_SIZE: u32 = 8;
    const DOCUMENT_BUDGET: usize = 2_048;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn generation() -> CodeGenerationId {
        id("generation.fixture")
    }

    fn key(composition: EmbeddingDocumentCompositionV1) -> EmbeddingProjectionKeyV1 {
        EmbeddingProjectionKeyV1 {
            model_artifact_digest: id::<ManifestDigest>(&digest('a')),
            tokenizer_digest: id::<ManifestDigest>(&digest('b')),
            config_digest: id::<ManifestDigest>(&digest('c')),
            query_instruction_digest: None,
            document_instruction_digest: None,
            document_composition: composition,
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 512,
            inference_batch_size: BATCH_SIZE,
            inference_batch_bytes: BATCH_SIZE * DOCUMENT_BUDGET as u32,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "ort-fixture".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 8,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            privacy_domain: id::<PrivacyDomainId>("privacy.fixture"),
            privacy_key_epoch: 1,
        }
    }

    fn header_key() -> EmbeddingProjectionKeyV1 {
        key(EmbeddingDocumentCompositionV1::SymbolContextHeader)
    }

    fn record(
        occurrence: &str,
        kind: &str,
        qualified_name: &str,
        simple_name: &str,
    ) -> Arc<LineageSymbolRecordV1> {
        // Distinct per occurrence: the symbol index rejects duplicate identities.
        let mut identity_hex = hex::encode(occurrence);
        identity_hex.truncate(64);
        Arc::new(LineageSymbolRecordV1 {
            occurrence: id(occurrence),
            identity: id::<SymbolIdentityDigest>(&format!("sha256:{identity_hex:0<64}")),
            qualified_name: qualified_name.to_owned(),
            simple_name: simple_name.to_owned(),
            kind: kind.to_owned(),
            visibility: "public".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            line_span: 1,
            start_line: 0,
            signature: Some("fn get(&self, key: u32) -> Option<u32>".to_owned()),
            skip_test_coverage: false,
            file_identity: id::<FileIdentityDigest>(&digest('f')),
            content_digest: id::<ContentDigest>(&digest('d')),
        })
    }

    fn chunk(
        grain: CodeSearchChunkGrainV1,
        occurrence: Option<&str>,
        level: SensitivityLevelV1,
        text: &str,
    ) -> CodeSearchChunkV1 {
        CodeSearchChunkV1 {
            id: id("chunk.fixture"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation(),
                file_occurrence_id: id::<FileOccurrenceId>("file.fixture"),
                symbol_occurrence_id: occurrence.map(id),
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: text.len() as u64,
                },
                grain,
                ordinal: 0,
            },
            content_digest: id::<ContentDigest>(&digest('e')),
            language_descriptor_revision: id::<LanguageDescriptorRevision>("rust.v1"),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
            sensitivity: SensitivityDecision {
                level,
                policy_revision: id::<PolicyRevisionId>("policy.v1"),
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new(text).expect("bounded text"),
        }
    }

    fn composer(records: Vec<Arc<LineageSymbolRecordV1>>) -> EmbeddingDocumentComposerV1 {
        let index = GenerationSymbolIndexV1::new(generation(), records).expect("symbol index");
        EmbeddingDocumentComposerV1::new(EmbeddingSymbolContextIndexV1::from_generation_symbols(
            &index,
        ))
    }

    fn method_record() -> Arc<LineageSymbolRecordV1> {
        record("symbol.get", "method", "Holder::get", "get")
    }

    #[test]
    fn sanitized_text_composition_is_the_chunk_text_unchanged() {
        let composer = composer(vec![method_record()]);
        let chunk = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.get"),
            SensitivityLevelV1::Public,
            "pub fn get(&self, key: u32) -> Option<u32> {\n    self.map.get(&key).copied()\n}",
        );
        let document = composer
            .compose(&key(EmbeddingDocumentCompositionV1::SanitizedText), &chunk)
            .expect("document");
        assert_eq!(document.text(), chunk.sanitized_text.as_str());
        assert_eq!(document.header(), EmbeddingDocumentHeaderV1::NotComposed);
    }

    #[test]
    fn symbol_grains_render_the_same_header_ahead_of_their_text() {
        let composer = composer(vec![method_record()]);
        for (grain, text) in [
            (
                CodeSearchChunkGrainV1::SymbolSignature,
                "pub fn get(&self, key: u32) -> Option<u32> {",
            ),
            (
                CodeSearchChunkGrainV1::SymbolBody,
                "pub fn get(&self, key: u32) -> Option<u32> {\n    self.map.get(&key).copied()\n}",
            ),
            (
                CodeSearchChunkGrainV1::SymbolMember,
                "self.map.get(&key).copied()",
            ),
        ] {
            let chunk = chunk(grain, Some("symbol.get"), SensitivityLevelV1::Public, text);
            let document = composer.compose(&header_key(), &chunk).expect("document");
            assert_eq!(
                document.text(),
                format!("symbol: method get\nscope: Holder\n{text}"),
                "{grain:?}"
            );
            assert_eq!(document.header(), EmbeddingDocumentHeaderV1::Rendered);
        }
    }

    #[test]
    fn file_grains_have_no_header_and_no_empty_lines() {
        let composer = composer(vec![method_record()]);
        for grain in [
            CodeSearchChunkGrainV1::FilePreamble,
            CodeSearchChunkGrainV1::FileWindow,
        ] {
            let chunk = chunk(grain, None, SensitivityLevelV1::Public, "use std::fmt;\n");
            let document = composer.compose(&header_key(), &chunk).expect("document");
            assert_eq!(document.text(), "use std::fmt;\n", "{grain:?}");
            assert_eq!(document.header(), EmbeddingDocumentHeaderV1::NoSymbol);
        }
    }

    #[test]
    fn top_level_symbols_omit_the_scope_line() {
        let composer = composer(vec![record("symbol.alpha", "function", "alpha", "alpha")]);
        let chunk = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.alpha"),
            SensitivityLevelV1::Public,
            "pub fn alpha(x: u32) -> u32 {\n    x + 1\n}",
        );
        let document = composer.compose(&header_key(), &chunk).expect("document");
        assert_eq!(
            document.text(),
            "symbol: function alpha\npub fn alpha(x: u32) -> u32 {\n    x + 1\n}"
        );
    }

    #[test]
    fn scope_strips_the_simple_name_only_behind_a_separator() {
        assert_eq!(scope_breadcrumb("Holder::get", "get"), Some("Holder"));
        assert_eq!(
            scope_breadcrumb("pkg.module.Class.method", "method"),
            Some("pkg.module.Class")
        );
        assert_eq!(scope_breadcrumb("Widget#render", "render"), Some("Widget"));
        assert_eq!(scope_breadcrumb("get", "get"), None);
        assert_eq!(scope_breadcrumb("::get", "get"), None);
        assert_eq!(scope_breadcrumb("Fooget", "get"), Some("Fooget"));
        assert_eq!(
            scope_breadcrumb("Holder::fetch", "get"),
            Some("Holder::fetch")
        );
    }

    #[test]
    fn header_values_are_normalized_to_one_line() {
        let composer = composer(vec![record(
            "symbol.get",
            "method",
            "Holder ::\n\tget",
            "get",
        )]);
        let chunk = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.get"),
            SensitivityLevelV1::Public,
            "body",
        );
        let document = composer.compose(&header_key(), &chunk).expect("document");
        assert_eq!(document.text(), "symbol: method get\nscope: Holder\nbody");
    }

    #[test]
    fn non_canonical_symbol_text_withholds_the_header() {
        let composer = composer(vec![record(
            "symbol.get",
            "method",
            "Holder::g\u{7}et",
            "g\u{7}et",
        )]);
        let chunk = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.get"),
            SensitivityLevelV1::Public,
            "body",
        );
        let document = composer.compose(&header_key(), &chunk).expect("document");
        assert_eq!(document.text(), "body");
        assert_eq!(
            document.header(),
            EmbeddingDocumentHeaderV1::Withheld(
                EmbeddingHeaderWithholdReasonV1::NonCanonicalSymbolText
            )
        );
    }

    #[test]
    fn redacted_and_restricted_chunks_embed_body_only() {
        let composer = composer(vec![method_record()]);
        for level in [SensitivityLevelV1::Redacted, SensitivityLevelV1::Restricted] {
            let chunk = chunk(
                CodeSearchChunkGrainV1::SymbolBody,
                Some("symbol.get"),
                level,
                "pub fn get(&self) {}",
            );
            let document = composer.compose(&header_key(), &chunk).expect("document");
            assert_eq!(document.text(), "pub fn get(&self) {}", "{level:?}");
            assert_eq!(
                document.header(),
                EmbeddingDocumentHeaderV1::Withheld(EmbeddingHeaderWithholdReasonV1::Sensitivity)
            );
        }
        let internal = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.get"),
            SensitivityLevelV1::Internal,
            "pub fn get(&self) {}",
        );
        assert_eq!(
            composer
                .compose(&header_key(), &internal)
                .expect("document")
                .header(),
            EmbeddingDocumentHeaderV1::Rendered
        );
    }

    #[test]
    fn missing_symbol_record_is_a_typed_error_not_a_bare_body() {
        let composer = composer(vec![method_record()]);
        let chunk = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.unknown"),
            SensitivityLevelV1::Public,
            "body",
        );
        assert_eq!(
            composer.compose(&header_key(), &chunk),
            Err(EmbeddingDocumentErrorV1::MissingSymbolRecord {
                chunk_id: chunk.id.clone(),
                occurrence: id("symbol.unknown"),
            })
        );
    }

    #[test]
    fn foreign_generation_chunks_are_rejected() {
        let composer = composer(vec![method_record()]);
        let mut chunk = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.get"),
            SensitivityLevelV1::Public,
            "body",
        );
        chunk.anchor.generation_id = id("generation.other");
        assert_eq!(
            composer.compose(&header_key(), &chunk),
            Err(EmbeddingDocumentErrorV1::ForeignGeneration {
                chunk_id: chunk.id.clone(),
                generation_id: generation(),
            })
        );
        assert!(
            composer
                .compose(&key(EmbeddingDocumentCompositionV1::SanitizedText), &chunk)
                .is_ok(),
            "the shipped composition consults no symbol index"
        );
    }

    #[test]
    fn header_is_cut_exactly_at_a_quarter_of_the_document_budget() {
        let long_scope = "a".repeat(600);
        let record = record("symbol.get", "method", &format!("{long_scope}::get"), "get");
        let full = format!("symbol: method get\nscope: {long_scope}\n");
        let header_budget = DOCUMENT_BUDGET / HEADER_BUDGET_DIVISOR;
        assert!(full.len() > header_budget);

        let rendered = render_symbol_context_header(&record, DOCUMENT_BUDGET).expect("header");
        assert_eq!(rendered.len(), header_budget);
        assert!(rendered.ends_with("...\n"));
        assert_eq!(
            &rendered[..header_budget - HEADER_TRUNCATION_MARK.len()],
            &full[..header_budget - HEADER_TRUNCATION_MARK.len()]
        );

        // Exactly at the boundary the header is kept whole; one byte over
        // truncates.
        let exact = render_symbol_context_header(&record, full.len() * HEADER_BUDGET_DIVISOR)
            .expect("header");
        assert_eq!(exact, full);
        let over = render_symbol_context_header(&record, (full.len() - 1) * HEADER_BUDGET_DIVISOR)
            .expect("header");
        assert_eq!(over.len(), full.len() - 1);
        assert!(over.ends_with("...\n"));
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let record = record("symbol.get", "method", "Hölder::get", "get");
        // A budget whose quarter lands inside the two-byte `ö` snaps down.
        let full = "symbol: method get\nscope: Hölder\n";
        let cut_inside =
            full.find('ö').expect("multibyte scope") + 1 + HEADER_TRUNCATION_MARK.len();
        let rendered = render_symbol_context_header(&record, cut_inside * HEADER_BUDGET_DIVISOR)
            .expect("header");
        assert_eq!(rendered, "symbol: method get\nscope: H...\n");
    }

    #[test]
    fn exhausted_header_budget_withholds_the_header() {
        let record = method_record();
        assert_eq!(
            render_symbol_context_header(
                &record,
                HEADER_TRUNCATION_MARK.len() * HEADER_BUDGET_DIVISOR
            ),
            Err(EmbeddingHeaderWithholdReasonV1::BudgetExhausted)
        );
        assert_eq!(
            render_symbol_context_header(&record, 4),
            Err(EmbeddingHeaderWithholdReasonV1::BudgetExhausted)
        );
        let minimal = render_symbol_context_header(
            &record,
            (HEADER_TRUNCATION_MARK.len() + 1) * HEADER_BUDGET_DIVISOR,
        )
        .expect("one kept byte");
        assert_eq!(minimal, "s...\n");
    }

    #[test]
    fn body_shrinks_by_the_header_and_the_document_stays_within_budget() {
        let composer = composer(vec![method_record()]);
        let text = "x".repeat(DOCUMENT_BUDGET + 100);
        let chunk = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.get"),
            SensitivityLevelV1::Public,
            &text,
        );
        let document = composer.compose(&header_key(), &chunk).expect("document");
        let header = "symbol: method get\nscope: Holder\n";
        assert_eq!(document.text().len(), DOCUMENT_BUDGET);
        assert!(document.text().starts_with(header));
        assert_eq!(
            &document.text()[header.len()..],
            &text[..DOCUMENT_BUDGET - header.len()]
        );

        let short = chunk_with_text(&composer, "short body");
        assert_eq!(short.text(), format!("{header}short body"));
    }

    fn chunk_with_text(composer: &EmbeddingDocumentComposerV1, text: &str) -> EmbeddingDocumentV1 {
        let chunk = chunk(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("symbol.get"),
            SensitivityLevelV1::Public,
            text,
        );
        composer.compose(&header_key(), &chunk).expect("document")
    }

    #[test]
    fn a_full_group_of_composed_documents_fits_the_inference_byte_ceiling() {
        let composer = composer(vec![method_record()]);
        let key = header_key();
        let text = "y".repeat(3 * DOCUMENT_BUDGET);
        let total: usize = (0..BATCH_SIZE)
            .map(|_| {
                let chunk = chunk(
                    CodeSearchChunkGrainV1::SymbolBody,
                    Some("symbol.get"),
                    SensitivityLevelV1::Public,
                    &text,
                );
                composer
                    .compose(&key, &chunk)
                    .expect("document")
                    .text()
                    .len()
            })
            .sum();
        assert!(total <= key.inference_batch_bytes as usize);
    }

    #[test]
    fn composition_is_deterministic_across_runs() {
        let build = || {
            let composer = composer(vec![
                record("symbol.zeta", "function", "outer::zeta", "zeta"),
                method_record(),
                record("symbol.alpha", "struct", "Holder", "Holder"),
            ]);
            let chunk = chunk(
                CodeSearchChunkGrainV1::SymbolBody,
                Some("symbol.get"),
                SensitivityLevelV1::Public,
                "body text",
            );
            composer.compose(&header_key(), &chunk).expect("document")
        };
        let first = build();
        let second = build();
        assert_eq!(first, second);
        assert_eq!(first.text(), "symbol: method get\nscope: Holder\nbody text");
    }
}

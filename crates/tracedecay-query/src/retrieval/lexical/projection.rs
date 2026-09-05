use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BoundedSanitizedText, CodeGenerationId, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
    CodeSearchChunkId, CodeSearchChunkV1, CompactCandidate, ComponentRevision, EvidenceRole,
    ExactFieldV1, ExactTechnicalTermKindV1, ExactTechnicalTermV1, ExtractionAdmittedChunkV1,
    FileOccurrenceId, FixedPointScore, FreshnessCompatibilityV1, LanguageDescriptorRevision,
    LogicalEvidenceId, RepositoryId, RetrievalAnchorId, RetrievalBudget, RetrieverBatch,
    RetrieverCoverage, RetrieverKind, RetrieverOutcome, ScoreDomainId, SourceFreshness,
    SourceOccurrenceId, SymbolOccurrenceId, exact_search_canonical, technical_tokens,
    validate_code_logical_path,
};

use super::{
    LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest, MAX_FUZZY_TERM_EXPANSIONS_V1,
};
use crate::retrieval::exact::{ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, ExactTermPostingReadPort, LexicalPostingReadPort,
    RetrievalPortError, contract_error,
};

mod artifact;
mod postings;

pub use artifact::{
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeExactLexicalArtifactReaderV1,
    CodeLexicalArtifactBatchLimitV1, CodeLexicalArtifactBuildProgressV1,
    CodeLexicalArtifactBuilderV1, CodeLexicalArtifactErrorV1,
    CodeLexicalArtifactFinalizationPhaseV1, CodeLexicalArtifactFinalizationStepV1,
    CodeLexicalArtifactOccurrenceV1, CodeLexicalArtifactReaderV1,
    CodeLexicalArtifactSectionDigestV1, CodeLexicalArtifactWriterRevisionV1,
    CodeLexicalImportMembershipWitnessV1, PreparedCodeLexicalArtifactBatchV1,
    PreparedCodeLexicalArtifactPageV1, VerifiedCodeLexicalArtifactV1,
};

use postings::{ByteNgramBudget, ByteNgramPostings, FuzzyTermIndex};

const BM25_K1_MILLIS: u64 = 1_200;
const BM25_B_MILLIS: u64 = 750;
const FUZZY_SCORE_MILLIS: u64 = 500;
const PHRASE_SCORE_MILLIS: u64 = 2_000;
const ECHO_SCORE_MILLIS: u64 = 750;
const BYTE_NGRAM_POSTINGS_MEMORY_BUDGET_BYTES_V1: usize = 512 * 1024 * 1024;

/// Wall-clock bound for materializing one lexical generation's postings.
/// First-query `new` / `new_admitted` is O(store); a missing caller deadline
/// must not let that build run unbounded on the daemon query path.
pub const LEXICAL_PROJECTION_BUILD_DEADLINE_MICROS_V1: u64 = 30_000_000;

/// A set `deadline_micros`, including `Some(0)`, is used as-is. `None` uses the
/// crate 30s fallback. This is not request-over-profile: a caller that has both
/// a lane and a base deadline must pass the tighter value.
pub fn lexical_projection_build_deadline_micros(request_deadline_micros: Option<u64>) -> u64 {
    request_deadline_micros.unwrap_or(LEXICAL_PROJECTION_BUILD_DEADLINE_MICROS_V1)
}

fn map_postings_build_error(error: String) -> RetrievalPortError {
    if error == postings::LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED
        || error.starts_with(postings::LEXICAL_PROJECTION_NGRAM_MEMORY_BUDGET_EXCEEDED)
    {
        RetrievalPortError::BudgetExceeded
    } else {
        RetrievalPortError::Contract(error)
    }
}

fn check_projection_build_deadline(deadline: Instant) -> Result<(), RetrievalPortError> {
    if Instant::now() >= deadline {
        Err(RetrievalPortError::BudgetExceeded)
    } else {
        Ok(())
    }
}

/// Generation and source metadata bound to one immutable lexical projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeLexicalProjectionMetadataV1 {
    pub generation: CodeGenerationId,
    pub repository_id: Option<RepositoryId>,
    pub logical_paths: BTreeMap<FileOccurrenceId, String>,
    pub freshness: SourceFreshness,
    pub exact_retriever_revision: ComponentRevision,
    pub lexical_retriever_revision: ComponentRevision,
    pub exact_score_domain: ScoreDomainId,
}

impl CodeLexicalProjectionMetadataV1 {
    fn validate(&self) -> Result<(), RetrievalPortError> {
        self.generation.validate().map_err(contract_error)?;
        if let Some(repository_id) = &self.repository_id {
            repository_id.validate().map_err(contract_error)?;
        }
        for (file, path) in &self.logical_paths {
            file.validate().map_err(contract_error)?;
            validate_code_logical_path(path).map_err(contract_error)?;
        }
        self.freshness
            .source_namespace
            .validate()
            .map_err(contract_error)?;
        self.freshness
            .source_instance
            .validate()
            .map_err(contract_error)?;
        self.freshness
            .policy_revision
            .validate()
            .map_err(contract_error)?;
        self.exact_retriever_revision
            .validate()
            .map_err(contract_error)?;
        self.lexical_retriever_revision
            .validate()
            .map_err(contract_error)?;
        self.exact_score_domain.validate().map_err(contract_error)
    }
}

#[derive(Clone, Debug)]
struct ProjectedChunkV1 {
    id: CodeSearchChunkId,
    anchor: CodeSearchChunkAnchorV1,
    language_descriptor_revision: LanguageDescriptorRevision,
    exact_terms: Vec<ExactTechnicalTermV1>,
    sanitized_text: BoundedSanitizedText,
    logical_path: String,
    symbol_simple_name: Option<String>,
    symbol_qualified_name: Option<String>,
    symbol_kind: Option<String>,
    field_lengths: BTreeMap<LexicalFieldV1, usize>,
    normalized_text: String,
}

impl ProjectedChunkV1 {
    fn new(
        chunk: CodeSearchChunkV1,
        logical_path: String,
        qualified_name: Option<&str>,
    ) -> (Self, BTreeMap<LexicalFieldV1, Vec<String>>) {
        let fields = Self::projected_fields(&chunk, &logical_path, qualified_name);
        let normalized_text = normalize_lexical(chunk.sanitized_text.as_str());
        Self::from_parts(
            chunk.id,
            chunk.anchor,
            chunk.language_descriptor_revision,
            chunk.exact_terms,
            chunk.sanitized_text,
            logical_path,
            None,
            qualified_name.map(str::to_owned),
            None,
            normalized_text,
            fields,
        )
    }

    /// Clone only the fields the projection retains. Sealed pages lend chunks,
    /// so this avoids `admitted.clone().into_chunk()` copying subtokens and
    /// other dropped payloads on the artifact append path.
    fn from_ref(
        chunk: &CodeSearchChunkV1,
        logical_path: String,
        display: Option<&tracedecay_code_index::production::VerifiedSealedLexicalSymbolDisplayV1>,
    ) -> (Self, BTreeMap<LexicalFieldV1, Vec<String>>) {
        let fields =
            Self::projected_fields(chunk, &logical_path, display.map(|d| d.qualified_name()));
        let normalized_text = normalize_lexical(chunk.sanitized_text.as_str());
        Self::from_parts(
            chunk.id.clone(),
            chunk.anchor.clone(),
            chunk.language_descriptor_revision.clone(),
            chunk.exact_terms.clone(),
            chunk.sanitized_text.clone(),
            logical_path,
            display.map(|display| display.simple_name().to_owned()),
            display.map(|display| display.qualified_name().to_owned()),
            display.map(|display| display.kind().to_owned()),
            normalized_text,
            fields,
        )
    }

    /// `qualified_name` is the parser-attested extracted qualified name of the
    /// symbol this chunk anchors. Rust (and every language whose declarations
    /// name the type and the member separately) never spells `Type::member` in
    /// source, so the whole-term postings derived from chunk text and the
    /// simple-name whole-symbol term cannot answer a qualified-name query. The
    /// extractor is the only authority for that complete name, so the
    /// projection indexes it directly instead of re-deriving one.
    fn projected_fields(
        chunk: &CodeSearchChunkV1,
        logical_path: &str,
        qualified_name: Option<&str>,
    ) -> BTreeMap<LexicalFieldV1, Vec<String>> {
        let mut fields: BTreeMap<LexicalFieldV1, Vec<String>> = BTreeMap::new();
        if let Some(qualified_name) = qualified_name.filter(|name| !name.is_empty()) {
            fields
                .entry(LexicalFieldV1::QualifiedName)
                .or_default()
                .push(normalize_lexical(qualified_name));
        }
        let text_field = if chunk.anchor.grain == CodeSearchChunkGrainV1::FilePreamble {
            LexicalFieldV1::PreambleText
        } else {
            LexicalFieldV1::BodyText
        };
        fields.insert(text_field, lexical_tokens(chunk.sanitized_text.as_str()));
        fields.insert(LexicalFieldV1::Path, vec![normalize_lexical(logical_path)]);
        fields.insert(
            LexicalFieldV1::Subtoken,
            chunk
                .subtokens
                .iter()
                .map(|term| normalize_lexical(term))
                .collect(),
        );
        for term in &chunk.exact_terms {
            let Ok(canonical) = std::str::from_utf8(term.canonical_bytes()) else {
                continue;
            };
            let canonical = normalize_lexical(canonical);
            fields
                .entry(LexicalFieldV1::ExactTerm)
                .or_default()
                .push(canonical.clone());
            match term.kind() {
                ExactTechnicalTermKindV1::WholeSymbol
                    if matches!(
                        chunk.anchor.grain,
                        CodeSearchChunkGrainV1::SymbolSignature
                            | CodeSearchChunkGrainV1::SymbolMember
                    ) =>
                {
                    fields
                        .entry(LexicalFieldV1::SymbolName)
                        .or_default()
                        .push(canonical);
                }
                ExactTechnicalTermKindV1::QualifiedName => {
                    fields
                        .entry(LexicalFieldV1::QualifiedName)
                        .or_default()
                        .push(canonical);
                }
                ExactTechnicalTermKindV1::Path => {
                    fields
                        .entry(LexicalFieldV1::Path)
                        .or_default()
                        .push(canonical);
                }
                _ => {}
            }
        }
        fields
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        id: CodeSearchChunkId,
        anchor: CodeSearchChunkAnchorV1,
        language_descriptor_revision: LanguageDescriptorRevision,
        exact_terms: Vec<ExactTechnicalTermV1>,
        sanitized_text: BoundedSanitizedText,
        logical_path: String,
        symbol_simple_name: Option<String>,
        symbol_qualified_name: Option<String>,
        symbol_kind: Option<String>,
        normalized_text: String,
        fields: BTreeMap<LexicalFieldV1, Vec<String>>,
    ) -> (Self, BTreeMap<LexicalFieldV1, Vec<String>>) {
        let field_lengths = fields
            .iter()
            .map(|(field, terms)| (*field, terms.len()))
            .collect();
        (
            Self {
                id,
                anchor,
                language_descriptor_revision,
                exact_terms,
                sanitized_text,
                logical_path,
                symbol_simple_name,
                symbol_qualified_name,
                symbol_kind,
                field_lengths,
                normalized_text,
            },
            fields,
        )
    }

    fn exact_match_view(&self) -> ExactMatchRowViewV1<'_> {
        ExactMatchRowViewV1 {
            sanitized_text: self.sanitized_text.as_str(),
            logical_path: &self.logical_path,
            exact_terms: &self.exact_terms,
        }
    }
}

/// Immutable adapter over generation-bound code chunks.
///
/// The value implements the lexical posting port directly. Exact retrieval is
/// enabled independently by deriving an [`CodeExactProjectionAdapterV1`] with
/// the central admission authority; constructing this lexical adapter alone
/// never enables or mints exact proofs.
#[derive(Clone, Debug)]
pub struct CodeLexicalProjectionAdapterV1 {
    metadata: CodeLexicalProjectionMetadataV1,
    rows: Arc<Vec<ProjectedChunkV1>>,
    postings: Arc<LexicalGenerationPostingsV1>,
}

#[derive(Clone, Debug)]
struct LexicalGenerationPostingsV1 {
    term_documents: BTreeMap<LexicalFieldV1, BTreeMap<String, LexicalTermPostingV1>>,
    exact_documents: BTreeMap<ExactFieldV1, BTreeMap<Vec<u8>, RoaringBitmap>>,
    normalized_text: Arc<ByteNgramPostings>,
    raw_text: Arc<ByteNgramPostings>,
    fuzzy_terms: FuzzyTermIndex,
    average_field_lengths: BTreeMap<LexicalFieldV1, usize>,
}

#[derive(Clone, Debug, Default)]
struct LexicalTermPostingV1 {
    documents: RoaringBitmap,
    frequencies: Vec<(u32, u32)>,
}

impl LexicalTermPostingV1 {
    fn insert(&mut self, document: u32, frequency: u32) {
        self.documents.insert(document);
        self.frequencies.push((document, frequency));
    }

    fn frequency(&self, document: u32) -> usize {
        self.frequencies
            .binary_search_by_key(&document, |(document, _)| *document)
            .ok()
            .map(|index| self.frequencies[index].1 as usize)
            .unwrap_or_default()
    }
}

#[derive(Debug)]
struct LexicalGenerationPostingsBuildV1 {
    term_documents: BTreeMap<LexicalFieldV1, BTreeMap<String, LexicalTermPostingV1>>,
    exact_documents: BTreeMap<ExactFieldV1, BTreeMap<Vec<u8>, RoaringBitmap>>,
    normalized_text: ByteNgramPostings,
    raw_text: ByteNgramPostings,
    vocabulary: BTreeSet<String>,
    field_lengths: BTreeMap<LexicalFieldV1, usize>,
    ngram_budget: ByteNgramBudget,
}

impl Default for LexicalGenerationPostingsBuildV1 {
    fn default() -> Self {
        Self {
            term_documents: BTreeMap::new(),
            exact_documents: BTreeMap::new(),
            normalized_text: ByteNgramPostings::default(),
            raw_text: ByteNgramPostings::default(),
            vocabulary: BTreeSet::new(),
            field_lengths: BTreeMap::new(),
            ngram_budget: ByteNgramBudget::new(BYTE_NGRAM_POSTINGS_MEMORY_BUDGET_BYTES_V1),
        }
    }
}

impl LexicalGenerationPostingsBuildV1 {
    fn insert_row(
        &mut self,
        document: u32,
        row: &ProjectedChunkV1,
        fields: &BTreeMap<LexicalFieldV1, Vec<String>>,
    ) -> Result<(), RetrievalPortError> {
        for (field, terms) in fields {
            *self.field_lengths.entry(*field).or_default() += terms.len();
            let mut frequencies = BTreeMap::<&str, u32>::new();
            for term in terms {
                if *field != LexicalFieldV1::Subtoken {
                    self.vocabulary.insert(term.clone());
                }
                frequencies
                    .entry(term.as_str())
                    .and_modify(|frequency| *frequency = frequency.saturating_add(1))
                    .or_insert(1);
            }
            for (term, frequency) in frequencies {
                self.term_documents
                    .entry(*field)
                    .or_default()
                    .entry(term.to_owned())
                    .or_default()
                    .insert(document, frequency);
            }
        }
        self.exact_documents
            .entry(ExactFieldV1::Path)
            .or_default()
            .entry(row.logical_path.as_bytes().to_vec())
            .or_default()
            .insert(document);
        for term in &row.exact_terms {
            let canonical = canonical_projected_exact_term(term);
            self.exact_documents
                .entry(exact_field_for_kind(term.kind()))
                .or_default()
                .entry(canonical.into_owned())
                .or_default()
                .insert(document);
        }
        self.normalized_text
            .insert_document(
                document,
                row.normalized_text.as_bytes(),
                &mut self.ngram_budget,
            )
            .map_err(map_postings_build_error)
    }

    fn insert_raw_text(
        &mut self,
        document: u32,
        row: &ProjectedChunkV1,
    ) -> Result<(), RetrievalPortError> {
        self.raw_text
            .insert_document(
                document,
                row.sanitized_text.as_str().as_bytes(),
                &mut self.ngram_budget,
            )
            .map_err(map_postings_build_error)
    }

    fn finish(
        self,
        document_count: usize,
        raw_matches_normalized: bool,
        deadline: Option<Instant>,
    ) -> Result<LexicalGenerationPostingsV1, RetrievalPortError> {
        let divisor = document_count.max(1);
        let average_field_lengths = self
            .field_lengths
            .into_iter()
            .map(|(field, total)| (field, total.div_ceil(divisor).max(1)))
            .collect();
        let normalized_text = Arc::new(self.normalized_text);
        let raw_text = if raw_matches_normalized {
            Arc::clone(&normalized_text)
        } else {
            Arc::new(self.raw_text)
        };
        let fuzzy_terms = FuzzyTermIndex::from_terms(self.vocabulary, deadline)
            .map_err(map_postings_build_error)?;
        Ok(LexicalGenerationPostingsV1 {
            term_documents: self.term_documents,
            exact_documents: self.exact_documents,
            normalized_text,
            raw_text,
            fuzzy_terms,
            average_field_lengths,
        })
    }
}

#[derive(Clone, Debug)]
pub enum CodeLexicalProjectionBuildStepV1 {
    Pending {
        completed_documents: usize,
        total_documents: usize,
    },
    Ready(Box<CodeLexicalProjectionAdapterV1>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeLexicalProjectionBuildPhaseV1 {
    Rows,
    RawText { next_document: usize },
    Complete,
}

/// Generation-owned, in-memory lexical projection work that advances by a
/// caller-selected number of document operations and preserves partial state
/// between bounded scheduler windows.
#[derive(Debug)]
pub struct CodeLexicalProjectionBuildV1 {
    metadata: CodeLexicalProjectionMetadataV1,
    /// Parser-attested extracted qualified name per symbol occurrence. The
    /// sealed-page artifact path carries the same authority per chunk on its
    /// symbol display; this is how the in-memory build receives it.
    symbol_qualified_names: BTreeMap<SymbolOccurrenceId, String>,
    chunks: Vec<Option<CodeSearchChunkV1>>,
    rows: Vec<ProjectedChunkV1>,
    postings: Option<LexicalGenerationPostingsBuildV1>,
    next_document: usize,
    raw_matches_normalized: bool,
    extraction_admitted: bool,
    phase: CodeLexicalProjectionBuildPhaseV1,
}

impl CodeLexicalProjectionBuildV1 {
    pub fn new_admitted<C>(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<C>,
        symbol_qualified_names: BTreeMap<SymbolOccurrenceId, String>,
    ) -> Result<Self, RetrievalPortError>
    where
        C: ExtractionAdmittedChunkV1,
    {
        Self::new_inner(
            metadata,
            chunks
                .into_iter()
                .map(ExtractionAdmittedChunkV1::into_admitted_chunk)
                .collect(),
            symbol_qualified_names,
            true,
        )
    }

    fn new_inner(
        metadata: CodeLexicalProjectionMetadataV1,
        mut chunks: Vec<CodeSearchChunkV1>,
        symbol_qualified_names: BTreeMap<SymbolOccurrenceId, String>,
        extraction_admitted: bool,
    ) -> Result<Self, RetrievalPortError> {
        metadata.validate()?;
        if chunks.len() > u32::MAX as usize {
            return Err(RetrievalPortError::Contract(
                "lexical projection exceeds the posting document-id range".to_owned(),
            ));
        }
        chunks.sort_by(|left, right| left.id.cmp(&right.id));
        if chunks.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(RetrievalPortError::Contract(
                "lexical projection chunk identities must be unique".to_owned(),
            ));
        }
        let row_capacity = chunks.len();
        Ok(Self {
            metadata,
            symbol_qualified_names,
            chunks: chunks.into_iter().map(Some).collect(),
            rows: Vec::with_capacity(row_capacity),
            postings: Some(LexicalGenerationPostingsBuildV1::default()),
            next_document: 0,
            raw_matches_normalized: true,
            extraction_admitted,
            phase: CodeLexicalProjectionBuildPhaseV1::Rows,
        })
    }

    #[hotpath::measure(label = "query.artifact.projection_advance")]
    pub fn advance(
        &mut self,
        maximum_documents: usize,
    ) -> Result<CodeLexicalProjectionBuildStepV1, RetrievalPortError> {
        self.advance_inner(maximum_documents, None)
    }

    fn advance_inner(
        &mut self,
        maximum_documents: usize,
        deadline: Option<Instant>,
    ) -> Result<CodeLexicalProjectionBuildStepV1, RetrievalPortError> {
        if maximum_documents == 0 {
            return Err(RetrievalPortError::Contract(
                "lexical projection build window must admit at least one document".to_owned(),
            ));
        }
        if self.phase == CodeLexicalProjectionBuildPhaseV1::Complete {
            return Err(RetrievalPortError::Contract(
                "lexical projection build is already complete".to_owned(),
            ));
        }
        let mut remaining = maximum_documents;
        while remaining > 0 {
            if let Some(deadline) = deadline {
                check_projection_build_deadline(deadline)?;
            }
            match self.phase {
                CodeLexicalProjectionBuildPhaseV1::Rows => {
                    if self.next_document == self.chunks.len() {
                        self.phase = if self.raw_matches_normalized {
                            CodeLexicalProjectionBuildPhaseV1::Complete
                        } else {
                            CodeLexicalProjectionBuildPhaseV1::RawText { next_document: 0 }
                        };
                        continue;
                    }
                    let document = self.next_document;
                    let chunk = self.chunks[document].take().ok_or_else(|| {
                        RetrievalPortError::Contract(
                            "lexical projection row was advanced more than once".to_owned(),
                        )
                    })?;
                    chunk.validate().map_err(contract_error)?;
                    if !self.extraction_admitted
                        && chunk
                            .exact_terms
                            .iter()
                            .any(ExactTechnicalTermV1::requires_extraction_authority)
                    {
                        return Err(RetrievalPortError::Contract(
                            "raw exact terms require parser-backed extraction admission".to_owned(),
                        ));
                    }
                    if chunk.anchor.generation_id != self.metadata.generation {
                        return Err(RetrievalPortError::GenerationMismatch);
                    }
                    let logical_path = self
                        .metadata
                        .logical_paths
                        .get(&chunk.anchor.file_occurrence_id)
                        .cloned()
                        .ok_or_else(|| {
                            RetrievalPortError::Contract(format!(
                                "lexical projection is missing the logical path for {}",
                                chunk.anchor.file_occurrence_id
                            ))
                        })?;
                    let qualified_name = chunk
                        .anchor
                        .symbol_occurrence_id
                        .as_ref()
                        .and_then(|symbol| self.symbol_qualified_names.get(symbol))
                        .map(String::as_str);
                    let (row, fields) = ProjectedChunkV1::new(chunk, logical_path, qualified_name);
                    self.raw_matches_normalized &=
                        row.sanitized_text.as_str().as_bytes() == row.normalized_text.as_bytes();
                    self.postings
                        .as_mut()
                        .ok_or_else(|| {
                            RetrievalPortError::Contract(
                                "lexical projection build state is missing".to_owned(),
                            )
                        })?
                        .insert_row(document as u32, &row, &fields)?;
                    self.rows.push(row);
                    self.next_document += 1;
                    remaining -= 1;
                }
                CodeLexicalProjectionBuildPhaseV1::RawText { next_document } => {
                    if next_document == self.rows.len() {
                        self.phase = CodeLexicalProjectionBuildPhaseV1::Complete;
                        continue;
                    }
                    self.postings
                        .as_mut()
                        .ok_or_else(|| {
                            RetrievalPortError::Contract(
                                "lexical projection build state is missing".to_owned(),
                            )
                        })?
                        .insert_raw_text(next_document as u32, &self.rows[next_document])?;
                    self.phase = CodeLexicalProjectionBuildPhaseV1::RawText {
                        next_document: next_document + 1,
                    };
                    remaining -= 1;
                }
                CodeLexicalProjectionBuildPhaseV1::Complete => break,
            }
        }
        if matches!(
            self.phase,
            CodeLexicalProjectionBuildPhaseV1::Rows
                if self.next_document == self.chunks.len()
        ) {
            self.phase = if self.raw_matches_normalized {
                CodeLexicalProjectionBuildPhaseV1::Complete
            } else {
                CodeLexicalProjectionBuildPhaseV1::RawText { next_document: 0 }
            };
        }
        if matches!(
            self.phase,
            CodeLexicalProjectionBuildPhaseV1::RawText { next_document }
                if next_document == self.rows.len()
        ) {
            self.phase = CodeLexicalProjectionBuildPhaseV1::Complete;
        }
        if self.phase != CodeLexicalProjectionBuildPhaseV1::Complete {
            crate::hotpath_metrics::Residency::Cold.record("query.artifact.residency");
            hotpath::gauge!("query.artifact.rows").set(self.next_document);
            return Ok(CodeLexicalProjectionBuildStepV1::Pending {
                completed_documents: self.next_document,
                total_documents: self.chunks.len(),
            });
        }
        let postings = self
            .postings
            .take()
            .ok_or_else(|| {
                RetrievalPortError::Contract("lexical projection build state is missing".to_owned())
            })?
            .finish(self.rows.len(), self.raw_matches_normalized, deadline)?;
        crate::hotpath_metrics::Residency::Warm.record("query.artifact.residency");
        hotpath::gauge!("query.artifact.rows").set(self.rows.len());
        Ok(CodeLexicalProjectionBuildStepV1::Ready(Box::new(
            CodeLexicalProjectionAdapterV1 {
                metadata: self.metadata.clone(),
                rows: Arc::new(std::mem::take(&mut self.rows)),
                postings: Arc::new(postings),
            },
        )))
    }
}

impl LexicalGenerationPostingsV1 {
    fn retained_owned_bytes(&self) -> usize {
        let term_bytes = self
            .term_documents
            .values()
            .fold(0usize, |bytes, postings| {
                postings.iter().fold(bytes, |bytes, (term, posting)| {
                    bytes
                        .saturating_add(term.capacity())
                        .saturating_add(
                            (posting.documents.len() as usize)
                                .saturating_mul(std::mem::size_of::<u32>()),
                        )
                        .saturating_add(
                            posting
                                .frequencies
                                .capacity()
                                .saturating_mul(std::mem::size_of::<(u32, u32)>()),
                        )
                })
            });
        let exact_bytes = self
            .exact_documents
            .values()
            .fold(0usize, |bytes, postings| {
                postings.iter().fold(bytes, |bytes, (term, documents)| {
                    bytes.saturating_add(term.capacity()).saturating_add(
                        (documents.len() as usize).saturating_mul(std::mem::size_of::<u32>()),
                    )
                })
            });
        let raw_text_bytes = if Arc::ptr_eq(&self.normalized_text, &self.raw_text) {
            0
        } else {
            self.raw_text.retained_owned_bytes()
        };
        term_bytes
            .saturating_add(exact_bytes)
            .saturating_add(self.normalized_text.retained_owned_bytes())
            .saturating_add(raw_text_bytes)
            .saturating_add(self.fuzzy_terms.retained_owned_bytes())
            .saturating_add(
                self.average_field_lengths
                    .len()
                    .saturating_mul(std::mem::size_of::<(LexicalFieldV1, usize)>()),
            )
    }

    fn document_frequency(&self, field: LexicalFieldV1, term: &str) -> usize {
        self.term_documents
            .get(&field)
            .and_then(|postings| postings.get(term))
            .map(|posting| posting.documents.len() as usize)
            .unwrap_or_default()
    }

    fn term_frequency(&self, field: LexicalFieldV1, term: &str, document: u32) -> usize {
        self.term_documents
            .get(&field)
            .and_then(|postings| postings.get(term))
            .map(|posting| posting.frequency(document))
            .unwrap_or_default()
    }

    fn average_field_length(&self, field: LexicalFieldV1) -> usize {
        self.average_field_lengths.get(&field).copied().unwrap_or(1)
    }

    fn lexical_documents(
        &self,
        request: &LexicalLaneRequest<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_candidates: &BTreeMap<String, RoaringBitmap>,
    ) -> RoaringBitmap {
        let mut documents = RoaringBitmap::new();
        for term in &request.whole_terms {
            self.union_whole_term(&normalize_lexical(term), &mut documents);
            if let Some(expansions) = fuzzy.by_query.get(term) {
                for expansion in expansions {
                    self.union_whole_term(expansion, &mut documents);
                }
            }
        }
        if let Some(postings) = self.term_documents.get(&LexicalFieldV1::Subtoken) {
            for subtoken in &request.subtokens {
                if let Some(posting) = postings.get(&normalize_lexical(subtoken)) {
                    documents |= &posting.documents;
                }
            }
        }
        // Reuse the per-phrase n-gram candidate sets computed once by the
        // caller. Union is idempotent, so unioning the deduplicated normalized
        // phrases yields exactly the same document set as re-intersecting the
        // n-gram postings for every raw phrase here.
        for candidates in phrase_candidates.values() {
            documents |= candidates;
        }
        documents
    }

    /// The n-gram candidate-document set for one already-normalized phrase.
    /// Computed once per phrase and shared by both the phrase document-frequency
    /// tally and the lexical document set.
    fn phrase_candidate_documents(&self, normalized_phrase: &str) -> RoaringBitmap {
        self.normalized_text
            .candidate_documents(normalized_phrase.as_bytes())
    }

    fn exact_candidate_documents(&self, request: &ExactLaneRequest) -> RoaringBitmap {
        let mut documents = RoaringBitmap::new();
        for literal in &request.literals {
            if matches!(
                literal.field,
                ExactFieldV1::QuotedPhrase
                    | ExactFieldV1::DiagnosticText
                    | ExactFieldV1::CompilerOrRuntimeError
            ) {
                documents |= self.raw_text.candidate_documents(&literal.original_bytes);
            }
            if let Some(posting) = self
                .exact_documents
                .get(&literal.field)
                .and_then(|postings| postings.get(&literal.canonical_bytes))
            {
                documents |= posting;
            }
        }
        documents
    }

    fn phrase_document_frequency(
        &self,
        rows: &[ProjectedChunkV1],
        phrase: &str,
        candidates: &RoaringBitmap,
    ) -> usize {
        candidates
            .iter()
            .filter(|document| {
                substring_count(&rows[*document as usize].normalized_text, phrase) > 0
            })
            .count()
    }

    fn union_whole_term(&self, term: &str, documents: &mut RoaringBitmap) {
        for (field, postings) in &self.term_documents {
            if *field == LexicalFieldV1::Subtoken {
                continue;
            }
            if let Some(posting) = postings.get(term) {
                *documents |= &posting.documents;
            }
        }
    }
}

impl CodeLexicalProjectionAdapterV1 {
    /// Count heap payload bytes owned exclusively by this immutable projection.
    /// Shared sanitized chunk text is deliberately excluded because its Arc
    /// backing remains owned by the sealed generation; derived normalized text,
    /// posting keys/frequencies, n-grams, exact keys, and the fuzzy FST count.
    pub fn retained_owned_bytes(&self) -> usize {
        let row_bytes = self.rows.iter().fold(0usize, |bytes, row| {
            let exact_term_bytes = row.exact_terms.iter().fold(0usize, |bytes, term| {
                bytes
                    .saturating_add(term.original_bytes().len())
                    .saturating_add(term.canonical_bytes().len())
            });
            bytes
                .saturating_add(std::mem::size_of::<ProjectedChunkV1>())
                .saturating_add(row.id.as_str().len())
                .saturating_add(row.anchor.generation_id.as_str().len())
                .saturating_add(row.anchor.file_occurrence_id.as_str().len())
                .saturating_add(
                    row.anchor
                        .symbol_occurrence_id
                        .as_ref()
                        .map(|symbol| symbol.as_str().len())
                        .unwrap_or_default(),
                )
                .saturating_add(row.logical_path.capacity())
                .saturating_add(row.normalized_text.capacity())
                .saturating_add(exact_term_bytes)
                .saturating_add(
                    row.field_lengths
                        .len()
                        .saturating_mul(std::mem::size_of::<(LexicalFieldV1, usize)>()),
                )
        });
        let metadata_path_bytes =
            self.metadata
                .logical_paths
                .iter()
                .fold(0usize, |bytes, (file, path)| {
                    bytes
                        .saturating_add(file.as_str().len())
                        .saturating_add(path.capacity())
                });
        row_bytes
            .saturating_add(metadata_path_bytes)
            .saturating_add(self.postings.retained_owned_bytes())
    }

    pub fn new(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<CodeSearchChunkV1>,
    ) -> Result<Self, RetrievalPortError> {
        Self::new_inner(metadata, chunks, BTreeMap::new(), false, None)
    }

    /// Hard-wires `deadline_micros = None` (crate 30s fallback). Live daemon
    /// mount is [`Self::new_admitted_with_budget`].
    ///
    /// `symbol_qualified_names` carries the extractor's qualified name for
    /// every symbol occurrence in `chunks`; the sealed-page artifact path
    /// carries the same authority on its per-chunk symbol display. Passing an
    /// empty map projects no qualified-name postings, so qualified-symbol
    /// queries lose their exact recall.
    pub fn new_admitted<C>(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<C>,
        symbol_qualified_names: BTreeMap<SymbolOccurrenceId, String>,
    ) -> Result<Self, RetrievalPortError>
    where
        C: ExtractionAdmittedChunkV1,
    {
        Self::new_admitted_with_deadline(metadata, chunks, symbol_qualified_names, None)
    }

    /// Budget-aware admitted build for the daemon mount. A set
    /// `budget.deadline_micros`, including `Some(0)`, is used as-is; `None`
    /// uses the crate 30s fallback. Callers with lane+base must pass the tighter
    /// value on the budget they hand in.
    pub fn new_admitted_with_budget<C>(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<C>,
        symbol_qualified_names: BTreeMap<SymbolOccurrenceId, String>,
        budget: &RetrievalBudget,
    ) -> Result<Self, RetrievalPortError>
    where
        C: ExtractionAdmittedChunkV1,
    {
        Self::new_admitted_with_deadline(
            metadata,
            chunks,
            symbol_qualified_names,
            budget.deadline_micros,
        )
    }

    fn new_admitted_with_deadline<C>(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<C>,
        symbol_qualified_names: BTreeMap<SymbolOccurrenceId, String>,
        deadline_micros: Option<u64>,
    ) -> Result<Self, RetrievalPortError>
    where
        C: ExtractionAdmittedChunkV1,
    {
        Self::new_inner(
            metadata,
            chunks
                .into_iter()
                .map(ExtractionAdmittedChunkV1::into_admitted_chunk)
                .collect(),
            symbol_qualified_names,
            true,
            deadline_micros,
        )
    }

    fn new_inner(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<CodeSearchChunkV1>,
        symbol_qualified_names: BTreeMap<SymbolOccurrenceId, String>,
        extraction_admitted: bool,
        deadline_micros: Option<u64>,
    ) -> Result<Self, RetrievalPortError> {
        let deadline = Instant::now()
            + Duration::from_micros(lexical_projection_build_deadline_micros(deadline_micros));
        check_projection_build_deadline(deadline)?;
        let mut build = CodeLexicalProjectionBuildV1::new_inner(
            metadata,
            chunks,
            symbol_qualified_names,
            extraction_admitted,
        )?;
        match build.advance_inner(usize::MAX, Some(deadline))? {
            CodeLexicalProjectionBuildStepV1::Ready(projection) => Ok(*projection),
            CodeLexicalProjectionBuildStepV1::Pending { .. } => Err(RetrievalPortError::Contract(
                "unbounded lexical projection build did not complete".to_owned(),
            )),
        }
    }

    pub fn exact_adapter<A>(&self, authority: A) -> CodeExactProjectionAdapterV1<A>
    where
        A: ExactAdmissionAuthority,
    {
        CodeExactProjectionAdapterV1 {
            projection: self.clone(),
            authority,
        }
    }

    fn validate_generation(&self, generation: &CodeGenerationId) -> Result<(), RetrievalPortError> {
        if generation != &self.metadata.generation {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        Ok(())
    }

    fn stale_outcome<T>(&self) -> Option<RetrieverOutcome<T>> {
        (self.metadata.freshness.compatibility != FreshnessCompatibilityV1::Current).then(|| {
            crate::hotpath_metrics::Residency::Rebuilding.record("query.lane.lexical.residency");
            RetrieverOutcome::Stale(self.metadata.freshness.clone())
        })
    }

    #[hotpath::measure(label = "query.lane.lexical.generate")]
    fn lexical_batch(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverBatch<LexicalLaneEvidence>, RetrievalPortError> {
        let fuzzy = self.fuzzy_expansions(request)?;
        let prepared = PreparedLexicalQueryV1::new(request);
        // Intersect the n-gram postings for each normalized phrase exactly once,
        // then reuse the candidate set for both the document-frequency tally and
        // the lexical document set below (previously each phrase was intersected
        // twice per query).
        let phrase_candidates: BTreeMap<String, RoaringBitmap> = prepared
            .phrases
            .iter()
            .map(|(_, normalized)| {
                let candidates = self.postings.phrase_candidate_documents(normalized);
                (normalized.clone(), candidates)
            })
            .collect();
        let phrase_document_frequencies = phrase_candidates
            .iter()
            .map(|(phrase, candidates)| {
                let frequency = self
                    .postings
                    .phrase_document_frequency(&self.rows, phrase, candidates);
                (phrase.clone(), frequency)
            })
            .collect::<BTreeMap<_, _>>();
        let documents = self
            .postings
            .lexical_documents(request, &fuzzy, &phrase_candidates);
        let mut pairs = Vec::new();
        let mut excluded = self.rows.len() as u64 - documents.len();
        for document in documents {
            let row = &self.rows[document as usize];
            let score = self.score_row(
                document,
                row,
                &prepared,
                &fuzzy,
                &phrase_document_frequencies,
            );
            if score.field_scores.is_empty() {
                excluded += 1;
                continue;
            }
            let candidate = self.candidate(
                row,
                RetrieverKind::Lexical,
                self.metadata.lexical_retriever_revision.clone(),
                request.score_domain.clone(),
                None,
            )?;
            let evidence = LexicalLaneEvidence {
                binding: self.binding(row, &candidate, score.matched_kinds),
                field_scores_micros: score.field_scores,
                matched_whole_terms: score.matched_whole_terms,
                matched_subtokens: score.matched_subtokens,
                matched_phrases: score.matched_phrases,
                typo_recovery_applied: score.typo_recovery_applied,
                echo_penalty_applied: score.echo_penalty_applied,
            };
            pairs.push((candidate, evidence));
        }
        pairs.sort_by(|left, right| {
            left.0
                .source_occurrence_id
                .cmp(&right.0.source_occurrence_id)
        });
        let mut candidates = Vec::with_capacity(pairs.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, (mut candidate, evidence)) in pairs.into_iter().enumerate() {
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        hotpath::gauge!("query.lane.lexical.candidates").set(candidates.len());
        hotpath::gauge!("query.lane.lexical.examined").set(self.rows.len());
        Ok(RetrieverBatch {
            coverage: RetrieverCoverage {
                examined: self.rows.len() as u64,
                eligible: candidates.len() as u64,
                excluded,
                capped: 0,
                unknown: 0,
            },
            candidates,
            evidence_by_occurrence,
            continuation: None,
        })
    }

    #[hotpath::measure(label = "query.lane.fuzzy.expand")]
    fn fuzzy_expansions(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<FuzzyExpansionsV1, RetrievalPortError> {
        if request.fuzzy_budget == 0 {
            return Ok(FuzzyExpansionsV1::default());
        }
        let limit = request.fuzzy_budget.min(MAX_FUZZY_TERM_EXPANSIONS_V1) as usize;
        let mut group_by_query = BTreeMap::<String, usize>::new();
        let mut groups = Vec::<FuzzyQueryGroupV1>::new();
        for (query_ordinal, query) in request.whole_terms.iter().enumerate() {
            let normalized_query = normalize_lexical(query);
            let bound = fuzzy_distance_bound(&normalized_query);
            if bound == 0 {
                continue;
            }
            if let Some(group) = group_by_query.get(&normalized_query).copied() {
                groups[group].queries.insert(query.clone());
                continue;
            }
            let group = groups.len();
            group_by_query.insert(normalized_query.clone(), group);
            groups.push(FuzzyQueryGroupV1 {
                first_ordinal: query_ordinal,
                normalized_query,
                queries: BTreeSet::from([query.clone()]),
                bound,
                seen: BTreeSet::new(),
            });
        }
        groups.sort_by_key(|group| group.first_ordinal);
        let maximum_distance = groups.iter().map(|group| group.bound).max().unwrap_or(0);
        let mut selected = Vec::<(usize, String)>::with_capacity(limit);
        'distance: for distance in 1..=maximum_distance {
            for (group_index, group) in groups.iter_mut().enumerate() {
                if distance > group.bound {
                    continue;
                }
                let remaining = limit.saturating_sub(selected.len());
                if remaining == 0 {
                    break 'distance;
                }
                let slice = self
                    .postings
                    .fuzzy_terms
                    .terms_at_distance(
                        &group.normalized_query,
                        distance,
                        remaining,
                        &mut group.seen,
                    )
                    .map_err(RetrievalPortError::Contract)?;
                selected.extend(slice.terms.into_iter().map(|term| (group_index, term)));
            }
        }
        let mut by_query: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let expansion_count = selected.len();
        for (group_index, term) in selected {
            for query in &groups[group_index].queries {
                by_query
                    .entry(query.clone())
                    .or_default()
                    .insert(term.clone());
            }
        }
        hotpath::gauge!("query.lane.fuzzy.expansions").set(expansion_count);
        Ok(FuzzyExpansionsV1 { by_query })
    }

    fn score_row(
        &self,
        document: u32,
        row: &ProjectedChunkV1,
        prepared: &PreparedLexicalQueryV1<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_document_frequencies: &BTreeMap<String, usize>,
    ) -> LexicalRowScoreV1 {
        crate::hotpath_metrics::measure_frequent("query.lane.lexical.score_row", || {
            self.score_row_inner(document, row, prepared, fuzzy, phrase_document_frequencies)
        })
    }

    fn score_row_inner(
        &self,
        document: u32,
        row: &ProjectedChunkV1,
        prepared: &PreparedLexicalQueryV1<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_document_frequencies: &BTreeMap<String, usize>,
    ) -> LexicalRowScoreV1 {
        let mut field_scores: BTreeMap<LexicalFieldV1, u64> = BTreeMap::new();
        let mut matched_whole_terms = BTreeSet::new();
        let mut matched_subtokens = BTreeSet::new();
        let mut matched_phrases = BTreeSet::new();
        let mut matched_kinds = BTreeSet::new();
        let mut typo_recovery_applied = false;
        for field in row.field_lengths.keys() {
            if *field != LexicalFieldV1::Subtoken {
                for (query_term, normalized_query) in &prepared.whole_terms {
                    let exact_tf = self
                        .postings
                        .term_frequency(*field, normalized_query, document);
                    if exact_tf > 0 {
                        add_score(
                            &mut field_scores,
                            *field,
                            self.term_score(*field, normalized_query, exact_tf, row),
                        );
                        matched_whole_terms.insert((*query_term).to_owned());
                        collect_term_kinds(&row.exact_terms, normalized_query, &mut matched_kinds);
                    }
                    if let Some(expansions) = fuzzy.by_query.get(*query_term) {
                        for expansion in expansions {
                            let fuzzy_tf =
                                self.postings.term_frequency(*field, expansion, document);
                            if fuzzy_tf == 0 {
                                continue;
                            }
                            let score = self
                                .term_score(*field, expansion, fuzzy_tf, row)
                                .saturating_mul(FUZZY_SCORE_MILLIS)
                                / 1_000;
                            add_score(&mut field_scores, *field, score);
                            matched_whole_terms.insert((*query_term).to_owned());
                            typo_recovery_applied = true;
                            collect_term_kinds(&row.exact_terms, expansion, &mut matched_kinds);
                        }
                    }
                }
            }
            if *field == LexicalFieldV1::Subtoken {
                for (subtoken, normalized) in &prepared.subtokens {
                    let tf = self.postings.term_frequency(*field, normalized, document);
                    if tf > 0 {
                        add_score(
                            &mut field_scores,
                            *field,
                            self.term_score(*field, normalized, tf, row),
                        );
                        matched_subtokens.insert((*subtoken).to_owned());
                    }
                }
            }
        }
        for (phrase, normalized) in &prepared.phrases {
            let tf = substring_count(&row.normalized_text, normalized);
            if tf == 0 {
                continue;
            }
            let field = if row.anchor.grain == CodeSearchChunkGrainV1::FilePreamble {
                LexicalFieldV1::PreambleText
            } else {
                LexicalFieldV1::BodyText
            };
            let score = self
                .phrase_score(
                    field,
                    tf,
                    row,
                    phrase_document_frequencies
                        .get(normalized)
                        .copied()
                        .unwrap_or_default(),
                )
                .saturating_mul(PHRASE_SCORE_MILLIS)
                / 1_000;
            add_score(&mut field_scores, field, score);
            matched_phrases.insert((*phrase).to_owned());
        }
        let echo_penalty_applied =
            !prepared.echo_query.is_empty() && prepared.echo_query == row.normalized_text.trim();
        if echo_penalty_applied {
            for score in field_scores.values_mut() {
                *score = score.saturating_mul(ECHO_SCORE_MILLIS) / 1_000;
            }
        }
        LexicalRowScoreV1 {
            field_scores: field_scores.into_iter().collect(),
            matched_whole_terms: matched_whole_terms.into_iter().collect(),
            matched_subtokens: matched_subtokens.into_iter().collect(),
            matched_phrases: matched_phrases.into_iter().collect(),
            matched_kinds: matched_kinds.into_iter().collect(),
            typo_recovery_applied,
            echo_penalty_applied,
        }
    }

    fn term_score(
        &self,
        field: LexicalFieldV1,
        term: &str,
        term_frequency: usize,
        row: &ProjectedChunkV1,
    ) -> u64 {
        let document_frequency = self.postings.document_frequency(field, term);
        let document_length = row.field_lengths.get(&field).copied().unwrap_or(0).max(1);
        let average_length = self.postings.average_field_length(field);
        bm25_score_micros(
            self.rows.len(),
            document_frequency,
            term_frequency,
            document_length,
            average_length,
            field_weight_millis(field),
        )
    }

    fn phrase_score(
        &self,
        field: LexicalFieldV1,
        term_frequency: usize,
        row: &ProjectedChunkV1,
        document_frequency: usize,
    ) -> u64 {
        let document_length = row.field_lengths.get(&field).copied().unwrap_or(0).max(1);
        bm25_score_micros(
            self.rows.len(),
            document_frequency,
            term_frequency,
            document_length,
            self.postings.average_field_length(field),
            field_weight_millis(field),
        )
    }

    fn candidate(
        &self,
        row: &ProjectedChunkV1,
        retriever: RetrieverKind,
        retriever_revision: ComponentRevision,
        score_domain: ScoreDomainId,
        exact_admission_proof: Option<tracedecay_domain::ExactAdmissionProof>,
    ) -> Result<CompactCandidate, RetrievalPortError> {
        let lane = retriever.as_str();
        let chunk_id = row.id.as_str();
        let generation = row.anchor.generation_id.as_str();
        let evidence_id = row.anchor.symbol_occurrence_id.as_ref().map_or_else(
            || format!("code-chunk:{chunk_id}"),
            |symbol| format!("code-symbol:{}", symbol.as_str()),
        );
        Ok(CompactCandidate {
            anchor_id: retrieval_anchor(evidence_id.clone())?,
            logical_evidence_id: LogicalEvidenceId::new(evidence_id).map_err(contract_error)?,
            source_occurrence_id: SourceOccurrenceId::new(format!(
                "code-chunk:{generation}:{chunk_id}"
            ))
            .map_err(contract_error)?,
            file_occurrence_id: Some(row.anchor.file_occurrence_id.clone()),
            source_namespace: self.metadata.freshness.source_namespace.clone(),
            repository_id: self.metadata.repository_id.clone(),
            session_or_thread_id: None,
            logical_copy_cluster_id: None,
            logical_copy_evidence_anchor: None,
            evidence_role: EvidenceRole::Primary,
            retriever,
            retriever_revision,
            score_domain,
            raw_score: FixedPointScore::ZERO,
            ordinal_rank: 0,
            exact_admission_proof,
            retriever_evidence_anchor: retrieval_anchor(format!("code-lexical:{lane}:{chunk_id}"))?,
            freshness: self.metadata.freshness.clone(),
        })
    }

    fn binding(
        &self,
        row: &ProjectedChunkV1,
        candidate: &CompactCandidate,
        matched_term_kinds: Vec<ExactTechnicalTermKindV1>,
    ) -> CodeCandidateBindingV1 {
        CodeCandidateBindingV1 {
            candidate_anchor: candidate.anchor_id.clone(),
            occurrence: CodeOccurrenceRefV1 {
                generation: row.anchor.generation_id.clone(),
                file: row.anchor.file_occurrence_id.clone(),
                symbol: row.anchor.symbol_occurrence_id.clone(),
                chunk: Some(row.id.clone()),
            },
            language_descriptor_revision: row.language_descriptor_revision.clone(),
            matched_term_kinds,
            source_occurrence: candidate.source_occurrence_id.clone(),
        }
    }
}

impl LexicalPostingReadPort for CodeLexicalProjectionAdapterV1 {
    fn read_lexical_postings(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        self.validate_generation(&request.generation)?;
        if let Some(outcome) = self.stale_outcome() {
            return Ok(outcome);
        }
        Ok(RetrieverOutcome::Complete(self.lexical_batch(request)?))
    }
}

/// Exact-reader view over the same immutable lexical projection.
///
/// This type cannot exist without an [`ExactAdmissionAuthority`], and every
/// emitted proof comes from that authority's `admit` method.
#[derive(Clone, Debug)]
pub struct CodeExactProjectionAdapterV1<A> {
    projection: CodeLexicalProjectionAdapterV1,
    authority: A,
}

impl<A> ExactTermPostingReadPort for CodeExactProjectionAdapterV1<A>
where
    A: ExactAdmissionAuthority,
{
    fn read_exact_postings(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        self.projection.validate_generation(&request.generation)?;
        if let Some(outcome) = self.projection.stale_outcome() {
            return Ok(outcome);
        }
        let documents = self.projection.postings.exact_candidate_documents(request);
        let mut pairs = Vec::new();
        let mut excluded = self.projection.rows.len() as u64 - documents.len();
        let mut proofs = LiteralProofCacheV1::new(request.literals.len());
        for document in documents {
            let row = &self.projection.rows[document as usize];
            let (matched_literals, matched_kinds) = exact_matches(row.exact_match_view(), request);
            if matched_literals.is_empty() {
                excluded += 1;
                continue;
            }
            let (_, proof) = proofs
                .first_admitted(&matched_literals, request, &self.authority)?
                .ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "central authority rejected every projected exact match".to_owned(),
                    )
                })?;
            let matched_literals = matched_literals
                .iter()
                .map(|ordinal| request.literals[*ordinal].clone())
                .collect::<Vec<_>>();
            let candidate = self.projection.candidate(
                row,
                RetrieverKind::ExactLiteral,
                self.projection.metadata.exact_retriever_revision.clone(),
                self.projection.metadata.exact_score_domain.clone(),
                Some(proof.clone()),
            )?;
            let evidence = ExactLaneEvidence {
                binding: self.projection.binding(row, &candidate, matched_kinds),
                matched_literals,
                admission_proof: proof,
            };
            pairs.push((candidate, evidence));
        }
        pairs.sort_by(|left, right| {
            left.0
                .source_occurrence_id
                .cmp(&right.0.source_occurrence_id)
        });
        let mut candidates = Vec::with_capacity(pairs.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, (mut candidate, evidence)) in pairs.into_iter().enumerate() {
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        Ok(RetrieverOutcome::Complete(RetrieverBatch {
            coverage: RetrieverCoverage {
                examined: self.projection.rows.len() as u64,
                eligible: candidates.len() as u64,
                excluded,
                capped: 0,
                unknown: 0,
            },
            candidates,
            evidence_by_occurrence,
            continuation: None,
        }))
    }
}

#[derive(Default)]
struct FuzzyExpansionsV1 {
    by_query: BTreeMap<String, BTreeSet<String>>,
}

struct FuzzyQueryGroupV1 {
    first_ordinal: usize,
    normalized_query: String,
    queries: BTreeSet<String>,
    bound: usize,
    seen: BTreeSet<String>,
}

struct LexicalRowScoreV1 {
    field_scores: Vec<(LexicalFieldV1, u64)>,
    matched_whole_terms: Vec<String>,
    matched_subtokens: Vec<String>,
    matched_phrases: Vec<String>,
    matched_kinds: Vec<ExactTechnicalTermKindV1>,
    typo_recovery_applied: bool,
    echo_penalty_applied: bool,
}

/// The borrowed subset of a projected chunk that exact-literal matching
/// reads, so artifact rows never deep-clone chunk text per visited document.
struct ExactMatchRowViewV1<'a> {
    sanitized_text: &'a str,
    logical_path: &'a str,
    exact_terms: &'a [ExactTechnicalTermV1],
}

/// Match one row against every request literal, returning matched literal
/// ordinals into `request.literals`. Ordinals defer the literal clones to
/// the cap-bounded winners instead of paying them per visited document.
fn exact_matches(
    row: ExactMatchRowViewV1<'_>,
    request: &ExactLaneRequest,
) -> (Vec<usize>, Vec<ExactTechnicalTermKindV1>) {
    let mut matched_literals = Vec::new();
    let mut matched_kinds = BTreeSet::new();
    for (ordinal, literal) in request.literals.iter().enumerate() {
        let mut matched = false;
        if matches!(
            literal.field,
            ExactFieldV1::QuotedPhrase
                | ExactFieldV1::DiagnosticText
                | ExactFieldV1::CompilerOrRuntimeError
        ) {
            matched = contains_bytes(row.sanitized_text.as_bytes(), &literal.original_bytes);
        }
        if literal.field == ExactFieldV1::Path
            && row.logical_path.as_bytes() == literal.canonical_bytes.as_slice()
        {
            matched = true;
            matched_kinds.insert(ExactTechnicalTermKindV1::Path);
        }
        for term in row.exact_terms {
            if exact_field_for_kind(term.kind()) == literal.field
                && canonical_projected_exact_term(term).as_ref()
                    == literal.canonical_bytes.as_slice()
            {
                matched = true;
                matched_kinds.insert(term.kind());
            }
        }
        if matched {
            matched_literals.push(ordinal);
        }
    }
    (matched_literals, matched_kinds.into_iter().collect())
}

/// Per-request lazily admitted proofs, one slot per request literal.
///
/// An admission proof depends only on the literal and the request — never on
/// the matched document — while one `admit` costs four canonical-JSON SHA-256
/// digests. Both posting adapters previously re-admitted per matching
/// document, which dominated exact retrieval for high-cardinality literals.
/// Laziness preserves the original failure surface: a literal no document
/// matches is never admitted at all.
struct LiteralProofCacheV1 {
    slots: Vec<Option<Option<tracedecay_domain::ExactAdmissionProof>>>,
}

impl LiteralProofCacheV1 {
    fn new(literal_count: usize) -> Self {
        Self {
            slots: vec![None; literal_count],
        }
    }

    /// The first matched literal ordinal the central authority admits, with
    /// its proof — the same first-admitting-literal selection the per-document
    /// `find_map` performed, at most one `admit` per literal per request.
    fn first_admitted<A>(
        &mut self,
        matched_ordinals: &[usize],
        request: &ExactLaneRequest<'_>,
        authority: &A,
    ) -> Result<Option<(usize, tracedecay_domain::ExactAdmissionProof)>, RetrievalPortError>
    where
        A: ExactAdmissionAuthority,
    {
        for ordinal in matched_ordinals {
            let slot = self.slots.get_mut(*ordinal).ok_or_else(|| {
                RetrievalPortError::Contract(
                    "exact match ordinal is outside the request literals".to_owned(),
                )
            })?;
            if slot.is_none() {
                let literal = &request.literals[*ordinal];
                *slot = Some(
                    authority
                        .admit(literal.field, &literal.original_bytes, &request.base)
                        .map_err(contract_error)?,
                );
            }
            if let Some(Some(proof)) = slot {
                return Ok(Some((*ordinal, proof.clone())));
            }
        }
        Ok(None)
    }

    /// The already-admitted proof for one literal ordinal. Winner
    /// materialization resolves through this instead of retaining a proof
    /// clone per visited document.
    fn admitted_proof(
        &self,
        ordinal: usize,
    ) -> Result<tracedecay_domain::ExactAdmissionProof, RetrievalPortError> {
        self.slots
            .get(ordinal)
            .and_then(|slot| slot.as_ref())
            .and_then(|admitted| admitted.clone())
            .ok_or_else(|| {
                RetrievalPortError::Contract(
                    "exact winner names a literal the authority never admitted".to_owned(),
                )
            })
    }
}

/// Query-derived strings normalized once per retrieval. Row scoring reuses
/// these instead of re-normalizing every query term, subtoken, phrase, and
/// the whole echo query for every visited document.
struct PreparedLexicalQueryV1<'request> {
    /// `(original, normalized)` per request whole term.
    whole_terms: Vec<(&'request str, String)>,
    /// `(original, normalized)` per request subtoken.
    subtokens: Vec<(&'request str, String)>,
    /// `(original, normalized)` per request phrase.
    phrases: Vec<(&'request str, String)>,
    /// The normalized quote-trimmed query for the echo penalty.
    echo_query: String,
}

impl<'request> PreparedLexicalQueryV1<'request> {
    fn new(request: &'request LexicalLaneRequest<'_>) -> Self {
        Self {
            whole_terms: request
                .whole_terms
                .iter()
                .map(|term| (term.as_str(), normalize_lexical(term)))
                .collect(),
            subtokens: request
                .subtokens
                .iter()
                .map(|subtoken| (subtoken.as_str(), normalize_lexical(subtoken)))
                .collect(),
            phrases: request
                .phrases
                .iter()
                .map(|phrase| (phrase.as_str(), normalize_lexical(phrase)))
                .collect(),
            echo_query: normalize_lexical(request.query_view.as_str().trim_matches('"')),
        }
    }
}

fn exact_field_for_kind(kind: ExactTechnicalTermKindV1) -> ExactFieldV1 {
    match kind {
        ExactTechnicalTermKindV1::WholeSymbol => ExactFieldV1::Identifier,
        ExactTechnicalTermKindV1::QualifiedName => ExactFieldV1::QualifiedName,
        ExactTechnicalTermKindV1::Path => ExactFieldV1::Path,
        ExactTechnicalTermKindV1::CompilerErrorCode
        | ExactTechnicalTermKindV1::RuntimeErrorCode => ExactFieldV1::DiagnosticCode,
        ExactTechnicalTermKindV1::CompilerErrorText
        | ExactTechnicalTermKindV1::RuntimeErrorText => ExactFieldV1::CompilerOrRuntimeError,
        ExactTechnicalTermKindV1::CliFlag => ExactFieldV1::CliFlag,
        ExactTechnicalTermKindV1::ToolName => ExactFieldV1::ToolName,
        ExactTechnicalTermKindV1::ConfigurationKey => ExactFieldV1::ConfigurationKey,
        ExactTechnicalTermKindV1::CommitIdentifier => ExactFieldV1::CommitIdentifier,
    }
}

/// Posting-key form of one minted term: the shared per-field search
/// canonicalization over the mint canonical, after stripping the extraction
/// `commit:` prefix so bare-hash query literals address the same key.
fn canonical_projected_exact_term(term: &ExactTechnicalTermV1) -> Cow<'_, [u8]> {
    let bytes = term.canonical_bytes();
    let Ok(value) = std::str::from_utf8(bytes) else {
        return Cow::Borrowed(bytes);
    };
    let value = if term.kind() == ExactTechnicalTermKindV1::CommitIdentifier {
        value.strip_prefix("commit:").unwrap_or(value)
    } else {
        value
    };
    let canonical = exact_search_canonical(exact_field_for_kind(term.kind()), value);
    if canonical.as_bytes() == bytes {
        Cow::Borrowed(bytes)
    } else {
        Cow::Owned(canonical.into_owned().into_bytes())
    }
}

fn collect_term_kinds(
    exact_terms: &[ExactTechnicalTermV1],
    normalized_term: &str,
    kinds: &mut BTreeSet<ExactTechnicalTermKindV1>,
) {
    for term in exact_terms {
        // `normalized_term` is already ASCII-lowercased, so the allocation-free
        // case-insensitive comparison is exactly `normalize_lexical(value) ==
        // normalized_term`.
        if std::str::from_utf8(term.canonical_bytes())
            .is_ok_and(|value| value.eq_ignore_ascii_case(normalized_term))
        {
            kinds.insert(term.kind());
        }
    }
}

fn retrieval_anchor(value: String) -> Result<RetrievalAnchorId, RetrievalPortError> {
    RetrievalAnchorId::new(value).map_err(contract_error)
}

fn normalize_lexical(value: &str) -> String {
    value.to_ascii_lowercase()
}

fn lexical_tokens(value: &str) -> Vec<String> {
    technical_tokens(value)
        .map(|(_, token)| normalize_lexical(token))
        .collect()
}

fn substring_count(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn add_score(scores: &mut BTreeMap<LexicalFieldV1, u64>, field: LexicalFieldV1, score: u64) {
    scores
        .entry(field)
        .and_modify(|current| *current = current.saturating_add(score))
        .or_insert(score);
}

fn field_weight_millis(field: LexicalFieldV1) -> u64 {
    match field {
        LexicalFieldV1::SymbolName => 4_000,
        LexicalFieldV1::QualifiedName => 3_500,
        LexicalFieldV1::Path => 3_000,
        LexicalFieldV1::ExactTerm => 2_500,
        LexicalFieldV1::Subtoken => 1_500,
        LexicalFieldV1::BodyText | LexicalFieldV1::PreambleText => 1_000,
    }
}

fn bm25_score_micros(
    document_count: usize,
    document_frequency: usize,
    term_frequency: usize,
    document_length: usize,
    average_length: usize,
    field_weight_millis: u64,
) -> u64 {
    if document_count == 0 || document_frequency == 0 || term_frequency == 0 {
        return 0;
    }
    let idf_micros = fixed_ln_ratio_micros(
        (document_count as u64).saturating_mul(2).saturating_add(2),
        (document_frequency as u64)
            .saturating_mul(2)
            .saturating_add(1),
    );
    let length_ratio_millis =
        (document_length as u128).saturating_mul(1_000) / average_length.max(1) as u128;
    let normalization_millis = u128::from(1_000 - BM25_B_MILLIS)
        + u128::from(BM25_B_MILLIS).saturating_mul(length_ratio_millis) / 1_000;
    let denominator_millis = (term_frequency as u128).saturating_mul(1_000)
        + u128::from(BM25_K1_MILLIS).saturating_mul(normalization_millis) / 1_000;
    let tf_micros = (term_frequency as u128)
        .saturating_mul(u128::from(BM25_K1_MILLIS + 1_000))
        .saturating_mul(1_000_000)
        / denominator_millis.max(1);
    let score = u128::from(idf_micros)
        .saturating_mul(tf_micros)
        .saturating_mul(u128::from(field_weight_millis))
        / 1_000_000
        / 1_000;
    score.min(u128::from(u64::MAX)) as u64
}

fn fixed_ln_ratio_micros(numerator: u64, denominator: u64) -> u64 {
    const SCALE: u128 = 1_u128 << 40;
    const LN_2_SCALED: u128 = 762_123_384_786;
    let mut ratio = u128::from(numerator).saturating_mul(SCALE) / u128::from(denominator.max(1));
    let mut powers_of_two = 0_u128;
    while ratio >= SCALE.saturating_mul(2) {
        ratio /= 2;
        powers_of_two += 1;
    }
    let z = ratio.saturating_sub(SCALE).saturating_mul(SCALE) / ratio.saturating_add(SCALE).max(1);
    let z_squared = z.saturating_mul(z) / SCALE;
    let mut term = z;
    let mut sum = term;
    for divisor in [3_u128, 5, 7, 9, 11, 13, 15] {
        term = term.saturating_mul(z_squared) / SCALE;
        sum = sum.saturating_add(term / divisor);
    }
    let scaled = sum
        .saturating_mul(2)
        .saturating_add(powers_of_two.saturating_mul(LN_2_SCALED));
    (scaled.saturating_mul(1_000_000) / SCALE).min(u128::from(u64::MAX)) as u64
}

/// Upper byte-length caps that keep the fst Levenshtein automaton inside its
/// fixed 10_000-state capacity. The DFA size is content-dependent — repeated
/// characters collapse states, so uniform strings are the automaton's best
/// case — and the caps are anchored to the measured worst case on fst 0.4
/// (all-distinct bytes: distance 1 builds up to 416 bytes, distance 2 only up
/// to 49) with headroom below those ceilings. Queries beyond a cap skip fuzzy
/// expansion the same way sub-5-character queries do; the exact and phrase
/// lanes still serve them.
const FUZZY_DISTANCE_ONE_MAX_BYTES: usize = 320;
const FUZZY_DISTANCE_TWO_MAX_BYTES: usize = 32;

fn fuzzy_distance_bound(normalized_query: &str) -> usize {
    let character_count = normalized_query.chars().count();
    let byte_count = normalized_query.len();
    if character_count <= 4 || byte_count > FUZZY_DISTANCE_ONE_MAX_BYTES {
        0
    } else if character_count <= 8 || byte_count > FUZZY_DISTANCE_TWO_MAX_BYTES {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod fuzzy_distance_bound_tests {
    use super::*;

    /// Worst-case automaton input: all-distinct bytes defeat the DFA state
    /// collapsing that repeated characters allow.
    fn distinct_byte_query(length: usize) -> String {
        (0u32..length as u32)
            .map(|i| char::from_u32(33 + (i * 7) % 94).expect("printable ascii"))
            .collect()
    }

    #[test]
    fn caps_stay_inside_the_fst_automaton_capacity() {
        // Anchors the byte caps to the automaton implementation: if fst ever
        // tightens its state limit these constructions fail and the caps must
        // shrink with them.
        fst::automaton::Levenshtein::new(&distinct_byte_query(FUZZY_DISTANCE_ONE_MAX_BYTES), 1)
            .expect("distance-1 automaton must fit at the distance-1 byte cap");
        fst::automaton::Levenshtein::new(&distinct_byte_query(FUZZY_DISTANCE_TWO_MAX_BYTES), 2)
            .expect("distance-2 automaton must fit at the distance-2 byte cap");
    }

    #[test]
    fn long_queries_skip_fuzzy_expansion_like_short_ones() {
        assert_eq!(fuzzy_distance_bound("abcd"), 0);
        assert_eq!(fuzzy_distance_bound("abcdef"), 1);
        assert_eq!(fuzzy_distance_bound("abcdefghi"), 2);
        assert_eq!(
            fuzzy_distance_bound(&"a".repeat(FUZZY_DISTANCE_TWO_MAX_BYTES)),
            2
        );
        assert_eq!(
            fuzzy_distance_bound(&"a".repeat(FUZZY_DISTANCE_TWO_MAX_BYTES + 1)),
            1
        );
        assert_eq!(
            fuzzy_distance_bound(&"a".repeat(FUZZY_DISTANCE_ONE_MAX_BYTES)),
            1
        );
        assert_eq!(
            fuzzy_distance_bound(&"a".repeat(FUZZY_DISTANCE_ONE_MAX_BYTES + 1)),
            0
        );
        // Multibyte queries are capped by their UTF-8 byte length, not their
        // character count.
        let multibyte = "\u{3042}".repeat(FUZZY_DISTANCE_ONE_MAX_BYTES / 3 + 1);
        assert_eq!(fuzzy_distance_bound(&multibyte), 0);
    }
}

#[cfg(test)]
mod deadline_budget_tests {
    use super::*;
    use tracedecay_domain::{
        ComponentRevision, ScoreDomainId, SourceInstanceKey, SourceNamespace, UtcMicros,
    };

    fn dummy_metadata() -> CodeLexicalProjectionMetadataV1 {
        CodeLexicalProjectionMetadataV1 {
            generation: CodeGenerationId::new("generation.deadline.v1").expect("generation"),
            repository_id: None,
            logical_paths: BTreeMap::new(),
            freshness: SourceFreshness {
                source_namespace: SourceNamespace::new("ns.deadline").expect("namespace"),
                source_instance: SourceInstanceKey::new("instance.deadline").expect("instance"),
                source_watermark: None,
                projection_watermark: None,
                observed_at: UtcMicros(0),
                source_generation: None,
                generation_lag: None,
                compatibility: FreshnessCompatibilityV1::Unknown,
                policy_revision: ComponentRevision::new("policy.deadline.v1").expect("policy"),
            },
            exact_retriever_revision: ComponentRevision::new("retriever.exact.v1").expect("exact"),
            lexical_retriever_revision: ComponentRevision::new("retriever.lexical.v1")
                .expect("lexical"),
            exact_score_domain: ScoreDomainId::new(crate::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1)
                .expect("score"),
        }
    }

    #[test]
    fn new_admitted_with_budget_zero_deadline_is_immediate_budget_exceeded() {
        let budget = RetrievalBudget {
            max_candidates_per_lane: 1,
            max_fused_candidates: 1,
            max_hydrated_results: 1,
            max_hydration_bytes: 1,
            deadline_micros: Some(0),
        };
        let error = CodeLexicalProjectionAdapterV1::new_inner(
            dummy_metadata(),
            Vec::<CodeSearchChunkV1>::new(),
            BTreeMap::new(),
            true,
            budget.deadline_micros,
        )
        .expect_err("Some(0) must expire before validate");
        assert!(
            matches!(error, RetrievalPortError::BudgetExceeded),
            "Some(0) is a set deadline, not the crate fallback: {error:?}"
        );
    }

    #[test]
    fn ngram_resident_budget_refusal_is_typed_budget_exceeded() {
        let error = format!(
            "{}: maximum 0 bytes",
            postings::LEXICAL_PROJECTION_NGRAM_MEMORY_BUDGET_EXCEEDED
        );

        assert!(matches!(
            map_postings_build_error(error),
            RetrievalPortError::BudgetExceeded
        ));
    }
}

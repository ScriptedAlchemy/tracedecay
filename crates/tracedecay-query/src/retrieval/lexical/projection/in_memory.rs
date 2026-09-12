//! In-memory lexical projection over one generation's chunks.
//!
//! Evaluation-only: the search-quality evaluator and the query suites build
//! this adapter directly from admitted chunks. Production retrieval reads the
//! durable lexical artifact through [`super::CodeLexicalArtifactReaderV1`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use roaring::RoaringBitmap;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, CodeSearchChunkV1, CompactCandidate,
    ComponentRevision, EvidenceRole, ExactFieldV1, ExactTechnicalTermKindV1, ExactTechnicalTermV1,
    ExtractionAdmittedChunkV1, FixedPointScore, FreshnessCompatibilityV1, LogicalEvidenceId,
    RetrieverBatch, RetrieverCoverage, RetrieverKind, RetrieverOutcome, ScoreDomainId,
    SourceOccurrenceId, SymbolOccurrenceId,
};

use super::super::{
    LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest, MAX_FUZZY_TERM_EXPANSIONS_V1,
    admit_candidate_sources, candidate_admission_outcome, lexical_checkpoint,
};
use super::{
    CodeLexicalProjectionMetadataV1, ECHO_SCORE_MILLIS, ExactMatchRowViewV1, FUZZY_SCORE_MILLIS,
    FuzzyExpansionsV1, FuzzyQueryGroupV1, LexicalRowScoreV1, LiteralProofCacheV1,
    PHRASE_SCORE_MILLIS, PreparedLexicalQueryV1, ProjectedChunkV1, add_score, bm25_score_micros,
    canonical_projected_exact_term, collect_term_kinds, exact_field_for_kind, exact_matches,
    field_weight_millis, fuzzy_distance_bound, normalize_lexical, retrieval_anchor,
    substring_count,
};
use crate::retrieval::exact::{ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, ExactTermPostingReadPort, LexicalPostingReadPort,
    RetrievalPortError, contract_error,
};

mod postings;

use postings::{ByteNgramBudget, ByteNgramPostings, FuzzyTermIndex};

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

/// Immutable adapter over generation-bound code chunks.
///
/// The value implements the lexical posting port directly. Exact retrieval is
/// enabled independently by deriving an [`CodeExactProjectionAdapterV1`] with
/// the central admission authority; constructing this lexical adapter alone
/// never enables or mints exact proofs.
///
/// Metadata is shared, not owned: every scoped projection built over one
/// generation reads the same immutable copy instead of cloning its logical
/// path table per scope.
#[derive(Clone, Debug)]
pub struct CodeLexicalProjectionAdapterV1 {
    metadata: Arc<CodeLexicalProjectionMetadataV1>,
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
    metadata: Arc<CodeLexicalProjectionMetadataV1>,
    /// Parser-attested extracted qualified name per symbol occurrence. The
    /// sealed-page artifact path carries the same authority per chunk on its
    /// symbol display; this is how the in-memory build receives it. Shared so
    /// scoped builds over one generation read one corpus-wide map.
    symbol_qualified_names: Arc<BTreeMap<SymbolOccurrenceId, String>>,
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
        metadata: impl Into<Arc<CodeLexicalProjectionMetadataV1>>,
        chunks: Vec<C>,
        symbol_qualified_names: impl Into<Arc<BTreeMap<SymbolOccurrenceId, String>>>,
    ) -> Result<Self, RetrievalPortError>
    where
        C: ExtractionAdmittedChunkV1,
    {
        Self::new_inner(
            metadata.into(),
            chunks
                .into_iter()
                .map(ExtractionAdmittedChunkV1::into_admitted_chunk)
                .collect(),
            symbol_qualified_names.into(),
            true,
        )
    }

    fn new_inner(
        metadata: Arc<CodeLexicalProjectionMetadataV1>,
        mut chunks: Vec<CodeSearchChunkV1>,
        symbol_qualified_names: Arc<BTreeMap<SymbolOccurrenceId, String>>,
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
        pruned: &mut Vec<(String, u64)>,
    ) -> RoaringBitmap {
        let mut sources = Vec::new();
        for term in &request.whole_terms {
            let (frequency, documents) = self.whole_term_documents(&normalize_lexical(term));
            sources.push((frequency, (term.clone(), documents)));
            if let Some(expansions) = fuzzy.by_query.get(term) {
                for expansion in expansions {
                    let (frequency, documents) = self.whole_term_documents(expansion);
                    sources.push((frequency, (expansion.clone(), documents)));
                }
            }
        }
        if let Some(postings) = self.term_documents.get(&LexicalFieldV1::Subtoken) {
            for subtoken in &request.subtokens {
                if let Some(posting) = postings.get(&normalize_lexical(subtoken)) {
                    sources.push((
                        posting.documents.len() as usize,
                        (subtoken.clone(), posting.documents.clone()),
                    ));
                }
            }
        }
        let mut documents = RoaringBitmap::new();
        for (_, source) in admit_candidate_sources(sources, |frequency, (term, _)| {
            pruned.push((term.clone(), frequency as u64));
        }) {
            documents |= source;
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

    /// A whole-term candidate source: the term's documents across every
    /// non-subtoken field, keyed by the summed per-field document frequency
    /// the artifact reader also admits by.
    fn whole_term_documents(&self, term: &str) -> (usize, RoaringBitmap) {
        let mut documents = RoaringBitmap::new();
        let mut frequency = 0usize;
        for (field, postings) in &self.term_documents {
            if *field == LexicalFieldV1::Subtoken {
                continue;
            }
            if let Some(posting) = postings.get(term) {
                frequency = frequency.saturating_add(posting.documents.len() as usize);
                documents |= &posting.documents;
            }
        }
        (frequency, documents)
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
        metadata: impl Into<Arc<CodeLexicalProjectionMetadataV1>>,
        chunks: Vec<CodeSearchChunkV1>,
    ) -> Result<Self, RetrievalPortError> {
        Self::new_inner(
            metadata.into(),
            chunks,
            Arc::new(BTreeMap::new()),
            false,
            None,
        )
    }

    /// The single shared-source constructor: `chunks` carry parser-backed
    /// extraction admission and `symbol_qualified_names` the extractor's
    /// qualified name for every symbol occurrence among them; the sealed-page
    /// artifact path carries the same authority on its per-chunk symbol
    /// display. Passing an empty map projects no qualified-name postings, so
    /// qualified-symbol queries lose their exact recall.
    ///
    /// Both shared inputs are accepted as anything convertible to an `Arc`, so
    /// a caller building one projection per scope over the same generation
    /// hands every scope the same immutable metadata and name map instead of
    /// cloning them per scope.
    ///
    /// Hard-wires `deadline_micros = None` (crate 30s fallback); the daemon
    /// mount passes its own deadline to [`Self::new_admitted_with_deadline`].
    pub fn new_admitted<C>(
        metadata: impl Into<Arc<CodeLexicalProjectionMetadataV1>>,
        chunks: Vec<C>,
        symbol_qualified_names: impl Into<Arc<BTreeMap<SymbolOccurrenceId, String>>>,
    ) -> Result<Self, RetrievalPortError>
    where
        C: ExtractionAdmittedChunkV1,
    {
        Self::new_admitted_with_deadline(
            metadata.into(),
            chunks,
            symbol_qualified_names.into(),
            None,
        )
    }

    fn new_admitted_with_deadline<C>(
        metadata: Arc<CodeLexicalProjectionMetadataV1>,
        chunks: Vec<C>,
        symbol_qualified_names: Arc<BTreeMap<SymbolOccurrenceId, String>>,
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
        metadata: Arc<CodeLexicalProjectionMetadataV1>,
        chunks: Vec<CodeSearchChunkV1>,
        symbol_qualified_names: Arc<BTreeMap<SymbolOccurrenceId, String>>,
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
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
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
        let mut pruned = Vec::new();
        let documents =
            self.postings
                .lexical_documents(request, &fuzzy, &phrase_candidates, &mut pruned);
        let mut pairs = Vec::new();
        let mut excluded = self.rows.len() as u64 - documents.len();
        for document in documents {
            lexical_checkpoint(request.control)?;
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
        Ok(candidate_admission_outcome(
            RetrieverBatch {
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
            },
            pruned,
        ))
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
        self.lexical_batch(request)
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

    fn exact_match_view(&self) -> ExactMatchRowViewV1<'_> {
        ExactMatchRowViewV1 {
            sanitized_text: self.sanitized_text.as_str(),
            logical_path: &self.logical_path,
            exact_terms: &self.exact_terms,
        }
    }
}

#[cfg(test)]
mod deadline_budget_tests {
    use super::*;
    use tracedecay_domain::{
        ComponentRevision, ScoreDomainId, SourceFreshness, SourceInstanceKey, SourceNamespace,
        UtcMicros,
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
    fn zero_deadline_is_immediate_budget_exceeded() {
        let error = CodeLexicalProjectionAdapterV1::new_inner(
            Arc::new(dummy_metadata()),
            Vec::<CodeSearchChunkV1>::new(),
            Arc::new(BTreeMap::new()),
            true,
            Some(0),
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

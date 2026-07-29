use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, CodeSearchChunkV1, CompactCandidate,
    ComponentRevision, EvidenceRole, ExactFieldV1, ExactTechnicalTermKindV1, ExactTechnicalTermV1,
    FileOccurrenceId, FixedPointScore, FreshnessCompatibilityV1, LogicalEvidenceId, RepositoryId,
    RetrievalAnchorId, RetrieverBatch, RetrieverCoverage, RetrieverKind, RetrieverOutcome,
    ScoreDomainId, SourceFreshness, SourceOccurrenceId,
};

use super::{
    LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest, MAX_FUZZY_TERM_EXPANSIONS_V1,
};
use crate::code_index::chunks::ExtractionAdmittedCodeSearchChunkV1;
use crate::query::retrieval::exact::{
    ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest,
};
use crate::query::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, ExactTermPostingReadPort, LexicalPostingReadPort,
    RetrievalPortError,
};

const BM25_K1_MILLIS: u64 = 1_200;
const BM25_B_MILLIS: u64 = 750;
const FUZZY_SCORE_MILLIS: u64 = 500;
const PHRASE_SCORE_MILLIS: u64 = 2_000;
const ECHO_SCORE_MILLIS: u64 = 750;

/// Generation and source metadata bound to one immutable lexical projection.
#[derive(Clone, Debug, PartialEq, Eq)]
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
        self.generation
            .validate()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        if let Some(repository_id) = &self.repository_id {
            repository_id
                .validate()
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        }
        for (file, path) in &self.logical_paths {
            file.validate()
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
            if path.is_empty() {
                return Err(RetrievalPortError::Contract(
                    "lexical projection logical paths must not be empty".to_owned(),
                ));
            }
        }
        self.freshness
            .source_namespace
            .validate()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        self.freshness
            .source_instance
            .validate()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        self.freshness
            .policy_revision
            .validate()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        self.exact_retriever_revision
            .validate()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        self.lexical_retriever_revision
            .validate()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        self.exact_score_domain
            .validate()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))
    }
}

#[derive(Clone, Debug)]
struct ProjectedChunkV1 {
    chunk: CodeSearchChunkV1,
    logical_path: String,
    fields: BTreeMap<LexicalFieldV1, Vec<String>>,
    normalized_text: String,
}

impl ProjectedChunkV1 {
    fn new(chunk: CodeSearchChunkV1, logical_path: String) -> Self {
        let normalized_text = normalize_lexical(chunk.sanitized_text.as_str());
        let mut fields: BTreeMap<LexicalFieldV1, Vec<String>> = BTreeMap::new();
        let text_field = if chunk.anchor.grain == CodeSearchChunkGrainV1::FilePreamble {
            LexicalFieldV1::PreambleText
        } else {
            LexicalFieldV1::BodyText
        };
        fields.insert(text_field, lexical_tokens(chunk.sanitized_text.as_str()));
        fields.insert(LexicalFieldV1::Path, vec![normalize_lexical(&logical_path)]);
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
        Self {
            chunk,
            logical_path,
            fields,
            normalized_text,
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
    statistics: Arc<LexicalCorpusStatisticsV1>,
}

#[derive(Clone, Debug)]
struct LexicalCorpusStatisticsV1 {
    vocabulary: BTreeSet<String>,
    document_frequencies: BTreeMap<LexicalFieldV1, BTreeMap<String, usize>>,
    average_field_lengths: BTreeMap<LexicalFieldV1, usize>,
}

impl LexicalCorpusStatisticsV1 {
    fn from_rows(rows: &[ProjectedChunkV1]) -> Self {
        let mut vocabulary = BTreeSet::new();
        let mut document_frequencies = BTreeMap::<LexicalFieldV1, BTreeMap<String, usize>>::new();
        let mut field_lengths = BTreeMap::<LexicalFieldV1, usize>::new();
        for row in rows {
            for (field, terms) in &row.fields {
                *field_lengths.entry(*field).or_default() += terms.len();
                let mut unique = BTreeSet::new();
                for term in terms {
                    if *field != LexicalFieldV1::Subtoken {
                        vocabulary.insert(term.clone());
                    }
                    unique.insert(term);
                }
                for term in unique {
                    *document_frequencies
                        .entry(*field)
                        .or_default()
                        .entry(term.clone())
                        .or_default() += 1;
                }
            }
        }
        let divisor = rows.len().max(1);
        let average_field_lengths = field_lengths
            .into_iter()
            .map(|(field, total)| (field, total.div_ceil(divisor).max(1)))
            .collect();
        Self {
            vocabulary,
            document_frequencies,
            average_field_lengths,
        }
    }

    fn document_frequency(&self, field: LexicalFieldV1, term: &str) -> usize {
        self.document_frequencies
            .get(&field)
            .and_then(|frequencies| frequencies.get(term))
            .copied()
            .unwrap_or_default()
    }

    fn average_field_length(&self, field: LexicalFieldV1) -> usize {
        self.average_field_lengths.get(&field).copied().unwrap_or(1)
    }
}

impl CodeLexicalProjectionAdapterV1 {
    pub fn new(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<CodeSearchChunkV1>,
    ) -> Result<Self, RetrievalPortError> {
        Self::new_inner(metadata, chunks, false)
    }

    pub fn new_admitted(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<ExtractionAdmittedCodeSearchChunkV1>,
    ) -> Result<Self, RetrievalPortError> {
        Self::new_inner(
            metadata,
            chunks
                .into_iter()
                .map(ExtractionAdmittedCodeSearchChunkV1::into_chunk)
                .collect(),
            true,
        )
    }

    fn new_inner(
        metadata: CodeLexicalProjectionMetadataV1,
        chunks: Vec<CodeSearchChunkV1>,
        extraction_admitted: bool,
    ) -> Result<Self, RetrievalPortError> {
        metadata.validate()?;
        let mut seen = BTreeSet::new();
        let mut rows = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            chunk
                .validate()
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
            if !extraction_admitted
                && chunk
                    .exact_terms
                    .iter()
                    .any(ExactTechnicalTermV1::requires_extraction_authority)
            {
                return Err(RetrievalPortError::Contract(
                    "raw exact terms require parser-backed extraction admission".to_owned(),
                ));
            }
            if chunk.anchor.generation_id != metadata.generation {
                return Err(RetrievalPortError::GenerationMismatch);
            }
            if !seen.insert(chunk.id.clone()) {
                return Err(RetrievalPortError::Contract(
                    "lexical projection chunk identities must be unique".to_owned(),
                ));
            }
            let logical_path = metadata
                .logical_paths
                .get(&chunk.anchor.file_occurrence_id)
                .cloned()
                .ok_or_else(|| {
                    RetrievalPortError::Contract(format!(
                        "lexical projection is missing the logical path for {}",
                        chunk.anchor.file_occurrence_id
                    ))
                })?;
            rows.push(ProjectedChunkV1::new(chunk, logical_path));
        }
        rows.sort_by(|left, right| left.chunk.id.cmp(&right.chunk.id));
        let statistics = Arc::new(LexicalCorpusStatisticsV1::from_rows(&rows));
        Ok(Self {
            metadata,
            rows: Arc::new(rows),
            statistics,
        })
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
        (self.metadata.freshness.compatibility != FreshnessCompatibilityV1::Current)
            .then(|| RetrieverOutcome::Stale(self.metadata.freshness.clone()))
    }

    fn lexical_batch(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverBatch<LexicalLaneEvidence>, RetrievalPortError> {
        let fuzzy = self.fuzzy_expansions(request);
        let phrase_document_frequencies = request
            .phrases
            .iter()
            .map(|phrase| normalize_lexical(phrase))
            .map(|phrase| {
                let frequency = self
                    .rows
                    .iter()
                    .filter(|row| substring_count(&row.normalized_text, &phrase) > 0)
                    .count();
                (phrase, frequency)
            })
            .collect::<BTreeMap<_, _>>();
        let mut pairs = Vec::new();
        let mut excluded = 0_u64;
        for row in self.rows.iter() {
            let score = self.score_row(row, request, &fuzzy, &phrase_document_frequencies);
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

    fn fuzzy_expansions(&self, request: &LexicalLaneRequest<'_>) -> FuzzyExpansionsV1 {
        if request.fuzzy_budget == 0 {
            return FuzzyExpansionsV1::default();
        }
        let mut candidates = Vec::new();
        for (query_ordinal, query) in request.whole_terms.iter().enumerate() {
            let normalized_query = normalize_lexical(query);
            let bound = fuzzy_distance_bound(normalized_query.chars().count());
            if bound == 0 {
                continue;
            }
            for term in &self.statistics.vocabulary {
                if term == &normalized_query {
                    continue;
                }
                if let Some(distance) = bounded_levenshtein(&normalized_query, term, bound) {
                    candidates.push((distance, query_ordinal, query.clone(), term.clone()));
                }
            }
        }
        candidates.sort();
        candidates.dedup_by(|left, right| left.2 == right.2 && left.3 == right.3);
        candidates.truncate(request.fuzzy_budget.min(MAX_FUZZY_TERM_EXPANSIONS_V1) as usize);
        let mut by_query: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (_, _, query, term) in candidates {
            by_query.entry(query).or_default().insert(term);
        }
        FuzzyExpansionsV1 { by_query }
    }

    fn score_row(
        &self,
        row: &ProjectedChunkV1,
        request: &LexicalLaneRequest<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_document_frequencies: &BTreeMap<String, usize>,
    ) -> LexicalRowScoreV1 {
        let mut field_scores: BTreeMap<LexicalFieldV1, u64> = BTreeMap::new();
        let mut matched_whole_terms = BTreeSet::new();
        let mut matched_subtokens = BTreeSet::new();
        let mut matched_phrases = BTreeSet::new();
        let mut matched_kinds = BTreeSet::new();
        let mut typo_recovery_applied = false;
        for (field, document_terms) in &row.fields {
            if *field != LexicalFieldV1::Subtoken {
                for query_term in &request.whole_terms {
                    let normalized_query = normalize_lexical(query_term);
                    let exact_tf = term_frequency(document_terms, &normalized_query);
                    if exact_tf > 0 {
                        add_score(
                            &mut field_scores,
                            *field,
                            self.term_score(*field, &normalized_query, exact_tf, row),
                        );
                        matched_whole_terms.insert(query_term.clone());
                        collect_term_kinds(row, &normalized_query, &mut matched_kinds);
                    }
                    if let Some(expansions) = fuzzy.by_query.get(query_term) {
                        for expansion in expansions {
                            let fuzzy_tf = term_frequency(document_terms, expansion);
                            if fuzzy_tf == 0 {
                                continue;
                            }
                            let score = self
                                .term_score(*field, expansion, fuzzy_tf, row)
                                .saturating_mul(FUZZY_SCORE_MILLIS)
                                / 1_000;
                            add_score(&mut field_scores, *field, score);
                            matched_whole_terms.insert(query_term.clone());
                            typo_recovery_applied = true;
                            collect_term_kinds(row, expansion, &mut matched_kinds);
                        }
                    }
                }
            }
            if *field == LexicalFieldV1::Subtoken {
                for subtoken in &request.subtokens {
                    let normalized = normalize_lexical(subtoken);
                    let tf = term_frequency(document_terms, &normalized);
                    if tf > 0 {
                        add_score(
                            &mut field_scores,
                            *field,
                            self.term_score(*field, &normalized, tf, row),
                        );
                        matched_subtokens.insert(subtoken.clone());
                    }
                }
            }
        }
        for phrase in &request.phrases {
            let normalized = normalize_lexical(phrase);
            let tf = substring_count(&row.normalized_text, &normalized);
            if tf == 0 {
                continue;
            }
            let field = if row.chunk.anchor.grain == CodeSearchChunkGrainV1::FilePreamble {
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
                        .get(&normalized)
                        .copied()
                        .unwrap_or_default(),
                )
                .saturating_mul(PHRASE_SCORE_MILLIS)
                / 1_000;
            add_score(&mut field_scores, field, score);
            matched_phrases.insert(phrase.clone());
        }
        let normalized_query = normalize_lexical(request.query_view.as_str().trim_matches('"'));
        let echo_penalty_applied =
            !normalized_query.is_empty() && normalized_query == row.normalized_text.trim();
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
        let document_frequency = self.statistics.document_frequency(field, term);
        let document_length = row.fields.get(&field).map_or(0, Vec::len).max(1);
        let average_length = self.statistics.average_field_length(field);
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
        let document_length = row.fields.get(&field).map_or(0, Vec::len).max(1);
        bm25_score_micros(
            self.rows.len(),
            document_frequency,
            term_frequency,
            document_length,
            self.statistics.average_field_length(field),
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
        let chunk_id = row.chunk.id.as_str();
        let generation = row.chunk.anchor.generation_id.as_str();
        let evidence_id = row.chunk.anchor.symbol_occurrence_id.as_ref().map_or_else(
            || format!("code-chunk:{chunk_id}"),
            |symbol| format!("code-symbol:{}", symbol.as_str()),
        );
        Ok(CompactCandidate {
            anchor_id: retrieval_anchor(evidence_id.clone())?,
            logical_evidence_id: LogicalEvidenceId::new(evidence_id)
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            source_occurrence_id: SourceOccurrenceId::new(format!(
                "code-chunk:{generation}:{chunk_id}"
            ))
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            file_occurrence_id: Some(row.chunk.anchor.file_occurrence_id.clone()),
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
                generation: row.chunk.anchor.generation_id.clone(),
                file: row.chunk.anchor.file_occurrence_id.clone(),
                symbol: row.chunk.anchor.symbol_occurrence_id.clone(),
                chunk: Some(row.chunk.id.clone()),
            },
            language_descriptor_revision: row.chunk.language_descriptor_revision.clone(),
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
        let mut pairs = Vec::new();
        let mut excluded = 0_u64;
        for row in self.projection.rows.iter() {
            let (matched_literals, matched_kinds) = exact_matches(row, request);
            if matched_literals.is_empty() {
                excluded += 1;
                continue;
            }
            let admitted = matched_literals
                .iter()
                .find_map(|literal| {
                    self.authority
                        .admit(literal.field, &literal.original_bytes, &request.base)
                        .transpose()
                        .map(|result| result.map(|proof| (literal, proof)))
                })
                .transpose()
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?
                .ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "central authority rejected every projected exact match".to_owned(),
                    )
                })?;
            let proof = admitted.1;
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

struct LexicalRowScoreV1 {
    field_scores: Vec<(LexicalFieldV1, u64)>,
    matched_whole_terms: Vec<String>,
    matched_subtokens: Vec<String>,
    matched_phrases: Vec<String>,
    matched_kinds: Vec<ExactTechnicalTermKindV1>,
    typo_recovery_applied: bool,
    echo_penalty_applied: bool,
}

fn exact_matches(
    row: &ProjectedChunkV1,
    request: &ExactLaneRequest,
) -> (
    Vec<crate::query::retrieval::exact::ExactLiteralV1>,
    Vec<ExactTechnicalTermKindV1>,
) {
    let mut matched_literals = Vec::new();
    let mut matched_kinds = BTreeSet::new();
    for literal in &request.literals {
        let mut matched = false;
        if matches!(
            literal.field,
            ExactFieldV1::QuotedPhrase
                | ExactFieldV1::DiagnosticText
                | ExactFieldV1::CompilerOrRuntimeError
        ) {
            matched = contains_bytes(
                row.chunk.sanitized_text.as_str().as_bytes(),
                &literal.original_bytes,
            );
        }
        if literal.field == ExactFieldV1::Path
            && row.logical_path.as_bytes() == literal.canonical_bytes.as_slice()
        {
            matched = true;
            matched_kinds.insert(ExactTechnicalTermKindV1::Path);
        }
        for term in &row.chunk.exact_terms {
            if exact_field_for_kind(term.kind()) == literal.field
                && term.canonical_bytes() == literal.canonical_bytes.as_slice()
            {
                matched = true;
                matched_kinds.insert(term.kind());
            }
        }
        if matched {
            matched_literals.push(literal.clone());
        }
    }
    (matched_literals, matched_kinds.into_iter().collect())
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

fn collect_term_kinds(
    row: &ProjectedChunkV1,
    normalized_term: &str,
    kinds: &mut BTreeSet<ExactTechnicalTermKindV1>,
) {
    for term in &row.chunk.exact_terms {
        if std::str::from_utf8(term.canonical_bytes())
            .is_ok_and(|value| normalize_lexical(value) == normalized_term)
        {
            kinds.insert(term.kind());
        }
    }
}

fn retrieval_anchor(value: String) -> Result<RetrievalAnchorId, RetrievalPortError> {
    RetrievalAnchorId::new(value).map_err(|error| RetrievalPortError::Contract(error.to_string()))
}

fn normalize_lexical(value: &str) -> String {
    value.to_ascii_lowercase()
}

fn lexical_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| {
            !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.' | '/'))
        })
        .filter(|term| !term.is_empty())
        .map(normalize_lexical)
        .collect()
}

fn term_frequency(document_terms: &[String], term: &str) -> usize {
    document_terms
        .iter()
        .filter(|value| value.as_str() == term)
        .count()
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

fn fuzzy_distance_bound(character_count: usize) -> usize {
    match character_count {
        0..=4 => 0,
        5..=8 => 1,
        _ => 2,
    }
}

fn bounded_levenshtein(left: &str, right: &str, bound: usize) -> Option<usize> {
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    if left_len.abs_diff(right_len) > bound {
        return None;
    }
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (left_index, left_char) in left.iter().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(left_index + 1);
        let mut row_minimum = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_char != right_char);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            let distance = substitution.min(insertion).min(deletion);
            current.push(distance);
            row_minimum = row_minimum.min(distance);
        }
        if row_minimum > bound {
            return None;
        }
        previous = current;
    }
    let distance = previous[right.len()];
    (distance <= bound).then_some(distance)
}

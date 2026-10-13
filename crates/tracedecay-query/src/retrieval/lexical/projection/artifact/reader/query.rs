//! Bounded posting traversal, candidate selection, and lexical/exact scoring.

use super::super::super::{
    ExactLiteralSpecificityV1, ExactMatchRowViewV1, ExactOccurrenceOwnersV1, ExactRowMatchV1,
    FuzzyExpansionsV1, FuzzyQueryGroupV1, LexicalFieldTextV1, LexicalRowScoreV1,
    LiteralProofCacheV1, PreparedLexicalQueryV1, bm25_score_micros, exact_matches,
    field_weight_millis, fuzzy_distance_bound, lexical_lane_binding, lexical_lane_candidate,
    normalize_lexical, proximity_count_tokens, proximity_tokens, row_field_texts,
    score_lexical_row, substring_count,
};
use super::super::format::{
    ArtifactRowV1, PostingListDecoderV1, VerifiedCodeLexicalArtifactV1, decode_document_set,
    decode_ngram_bitmap, decode_term_lists, document_set_bytes, term_lists_bytes,
};
use super::super::postings::{NGRAM_NORMALIZED, query_ngrams, raw_override_query_ngrams};
use super::super::row_codec::{
    ConnectionRowDictionaryV1, ROW_BLOCK_BY_DOCUMENT_SQL, RowBlocksV1, ScoringPrefaceIndexV1,
    StoredRowV1, decode_artifact_row,
};
use super::super::schema::{exact_field_code, field_from_code, stable_exact_term_id};
use super::super::{CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactErrorV1};
use super::{map_query_artifact_error, map_query_sql_error};
use crate::retrieval::exact::{ExactAdmissionAuthority, ExactLaneEvidence, ExactLaneRequest};
use crate::retrieval::lexical::{
    LexicalFieldFilterV1, LexicalFieldV1, LexicalLaneEvidence, LexicalLaneRequest,
    MAX_FUZZY_TERM_EXPANSIONS_V1, MAX_LEXICAL_QUERY_TERM_BYTES_V1, admit_candidate_sources,
    candidate_admission_outcome, field_admitted,
};
use crate::retrieval::ports::{
    RETRIEVAL_CANDIDATE_BATCH_SIZE, RetrievalExecutionControl, RetrievalPortError, contract_error,
    lane_candidate_cap, retrieval_checkpoint,
};
use crate::retrieval::source_tier::{
    classify_source_role, exact_definition_match, is_definition_field,
};
use roaring::RoaringBitmap;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use std::borrow::Cow;
#[cfg(test)]
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::sync::{Arc, OnceLock};
use tracedecay_domain::{
    CompactCandidate, ExactFieldV1, ExactTechnicalTermKindV1, RetrieverBatch, RetrieverCoverage,
    RetrieverKind, RetrieverOutcome, SourceOccurrenceId, split_subtokens,
};

pub(super) struct ArtifactQueryV1<'a> {
    connection: &'a Connection,
    metadata: &'a super::super::super::CodeLexicalProjectionMetadataV1,
    document_count: usize,
    metrics: ArtifactQueryMetricsV1,
    fuzzy_vocabulary: &'a OnceLock<Arc<FuzzyVocabularyV1>>,
    /// Row dictionary entries resolved during this query.
    row_dictionary: ConnectionRowDictionaryV1<'a>,
    /// The row block this query decoded last.
    row_blocks: RowBlocksV1<'a>,
}

#[derive(Default)]
pub(super) struct ArtifactQueryMetricsV1 {
    #[cfg(test)]
    pub(super) probes: Cell<u64>,
    #[cfg(test)]
    pub(super) fullscan_steps: Cell<u64>,
    #[cfg(test)]
    pub(super) ngram_decoded_lists: Cell<u64>,
    #[cfg(test)]
    pub(super) ngram_peak_candidates: Cell<u64>,
}

impl ArtifactQueryMetricsV1 {
    #[inline(always)]
    fn probe(&self) {
        #[cfg(test)]
        self.probes.set(self.probes.get().saturating_add(1));
    }

    #[inline(always)]
    fn observe_statement(
        &self,
        #[cfg_attr(not(test), expect(unused_variables))] statement: &rusqlite::Statement<'_>,
    ) -> Result<(), RetrievalPortError> {
        #[cfg(test)]
        {
            let steps =
                u64::try_from(statement.get_status(rusqlite::StatementStatus::FullscanStep))
                    .map_err(contract_error)?;
            self.fullscan_steps
                .set(self.fullscan_steps.get().saturating_add(steps));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn probes(&self) -> u64 {
        self.probes.get()
    }

    #[cfg(test)]
    pub(super) fn observed_fullscan_steps(&self) -> u64 {
        self.fullscan_steps.get()
    }

    #[cfg(test)]
    fn observe_ngram_list(&self) {
        self.ngram_decoded_lists
            .set(self.ngram_decoded_lists.get().saturating_add(1));
    }

    #[cfg(test)]
    fn observe_ngram_candidates(&self, candidates: u64) {
        self.ngram_peak_candidates
            .set(self.ngram_peak_candidates.get().max(candidates));
    }
}

/// Visit a candidate set in ascending document order. The set holds at most
/// one bit per artifact document however many sources contributed; request
/// authority is consulted before each candidate batch and at completion.
pub(super) fn visit_document_ids(
    documents: &RoaringBitmap,
    control: &dyn RetrievalExecutionControl,
    mut visitor: impl FnMut(u32) -> Result<(), RetrievalPortError>,
) -> Result<(), RetrievalPortError> {
    {
        let _span = tracing::trace_span!("query.stream.visit_documents").entered();
        {
            for (visited, document) in documents.iter().enumerate() {
                if visited.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                    retrieval_checkpoint(control)?;
                }
                visitor(document)?;
            }
            retrieval_checkpoint(control)?;
            Ok(())
        }
    }
}

/// Stream each candidate row with every request-term frequency it carries.
/// The request's posting lists advance once in document order alongside the
/// ascending candidates, so a row costs at most one keyed block read and no
/// posting probe. Phrase document frequencies need these rows; ordinary
/// term scoring reads the block preface instead.
pub(super) fn visit_lexical_rows(
    connection: &Connection,
    rows: &RowBlocksV1<'_>,
    documents: &RoaringBitmap,
    postings: &RequestTermPostingsV1,
    metrics: &ArtifactQueryMetricsV1,
    control: &dyn RetrievalExecutionControl,
    mut visitor: impl FnMut(
        u32,
        StoredRowV1,
        LexicalTermFrequenciesV1,
    ) -> Result<(), RetrievalPortError>,
) -> Result<(), RetrievalPortError> {
    {
        let _span = tracing::trace_span!("query.stream.visit_lexical_rows").entered();
        {
            let mut cursors = postings.cursors().map_err(map_query_artifact_error)?;
            metrics.probe();
            let mut visited = 0u64;
            for document in documents {
                if visited.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE as u64) {
                    retrieval_checkpoint(control)?;
                }
                let stored = rows.row(document).map_err(map_query_artifact_error)?;
                let mut entries = Vec::new();
                for cursor in &mut cursors {
                    if let Some(frequency) = cursor
                        .frequency_at(document)
                        .map_err(map_query_artifact_error)?
                    {
                        entries.push((
                            cursor.field,
                            cursor.term.to_owned(),
                            usize::try_from(frequency).map_err(contract_error)?,
                        ));
                    }
                }
                visitor(document, stored, LexicalTermFrequenciesV1(entries))?;
                visited = visited.saturating_add(1);
            }
            retrieval_checkpoint(control)?;
            if !documents.is_empty() {
                let statement = connection
                    .prepare_cached(ROW_BLOCK_BY_DOCUMENT_SQL)
                    .map_err(map_query_sql_error)?;
                metrics.observe_statement(&statement)?;
            }
            Ok(())
        }
    }
}

/// Every sealed `(term, field)` posting list of one request's terms, read
/// once: scoring statistics, candidate sources, and per-row frequencies all
/// come from these lists.
#[derive(Default)]
pub(super) struct RequestTermPostingsV1 {
    pub(super) by_term: BTreeMap<String, Vec<TermFieldPostingsV1>>,
}

pub(super) struct TermFieldPostingsV1 {
    pub(super) field: LexicalFieldV1,
    pub(super) postings: Vec<u8>,
}

impl RequestTermPostingsV1 {
    fn cursors(&self) -> Result<Vec<PostingCursorV1<'_>>, CodeLexicalArtifactErrorV1> {
        self.by_term
            .iter()
            .flat_map(|(term, lists)| lists.iter().map(move |list| (term, list)))
            .map(|(term, list)| PostingCursorV1::new(term, list))
            .collect()
    }

    /// Documents holding `term` in any field `admit` accepts.
    pub(super) fn documents(
        &self,
        term: &str,
        admit: impl Fn(LexicalFieldV1) -> bool,
    ) -> Result<RoaringBitmap, CodeLexicalArtifactErrorV1> {
        let mut documents = RoaringBitmap::new();
        for list in self.by_term.get(term).into_iter().flatten() {
            if !admit(list.field) {
                continue;
            }
            for posting in PostingListDecoderV1::new(&list.postings, true) {
                documents.insert(posting?.0);
            }
        }
        Ok(documents)
    }
}

struct PostingCursorV1<'a> {
    field: LexicalFieldV1,
    term: &'a str,
    decoder: PostingListDecoderV1<'a>,
    current: Option<(u32, u32)>,
}

impl<'a> PostingCursorV1<'a> {
    fn new(
        term: &'a str,
        list: &'a TermFieldPostingsV1,
    ) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let mut decoder = PostingListDecoderV1::new(&list.postings, true);
        let current = decoder.next().transpose()?;
        Ok(Self {
            field: list.field,
            term,
            decoder,
            current,
        })
    }

    /// Frequency of this list's term at `document`; callers ask in strictly
    /// ascending document order.
    fn frequency_at(&mut self, document: u32) -> Result<Option<u32>, CodeLexicalArtifactErrorV1> {
        while let Some((current, frequency)) = self.current {
            if current >= document {
                return Ok((current == document).then_some(frequency));
            }
            self.current = self.decoder.next().transpose()?;
        }
        Ok(None)
    }
}

pub(super) const ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1: usize = 16;

/// SQLite distributions are required to support at least 999 variables. Keep
/// generated statements within that portable ceiling instead of depending on
/// the larger build-time limit of a particular linked SQLite library.
pub(super) const ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1: usize = 999;
/// Caps request-projected text/blob input, and therefore the largest
/// request-relevant frequency aggregate SQLite can return as one row. The
/// lexical row stream checks the request control between rows, never inside
/// one SQLite call, so each individual call stays deterministically bounded.
pub(super) const ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1: usize =
    ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1 * MAX_LEXICAL_QUERY_TERM_BYTES_V1;
/// One encoded n-gram list is retained only while it is decoded and
/// intersected. Keep that transient allocation below one eighth of the cache.
const ARTIFACT_NGRAM_MAX_ENCODED_LIST_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 8;
/// The synchronous query may inspect at most one quarter of the reader cache
/// in encoded list bytes across all selected n-grams.
const ARTIFACT_NGRAM_QUERY_ENCODED_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 4;
/// A sparse Roaring candidate can require containers and two-byte values in
/// addition to its identifiers. Eight bytes per admitted identifier is a
/// conservative authority that bounds the first (rarest) full list.
const ARTIFACT_NGRAM_CANDIDATE_BITMAP_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 4;
const ARTIFACT_NGRAM_CANDIDATE_BYTES_PER_DOCUMENT_V1: usize = 8;
pub(super) const ARTIFACT_NGRAM_MAX_CANDIDATES_V1: u64 = (ARTIFACT_NGRAM_CANDIDATE_BITMAP_BYTES_V1
    / ARTIFACT_NGRAM_CANDIDATE_BYTES_PER_DOCUMENT_V1)
    as u64;
/// One request's term posting lists are held encoded for the whole row
/// stream; bound them by the same quarter of the reader cache.
const ARTIFACT_TERM_POSTING_QUERY_BYTES_V1: usize =
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1 / 4;
pub(super) fn ensure_sqlite_bind_capacity(
    fixed_parameters: usize,
    dynamic_parameters: usize,
) -> Result<(), RetrievalPortError> {
    let parameters = fixed_parameters
        .checked_add(dynamic_parameters)
        .ok_or(RetrievalPortError::BudgetExceeded)?;
    if parameters > ARTIFACT_SQLITE_MAX_BIND_PARAMETERS_V1 {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    Ok(())
}

pub(super) fn ensure_sqlite_bound_value_bytes<'a>(
    maximum_bytes: usize,
    fixed_parameters: &[Value],
    dynamic_text: impl IntoIterator<Item = &'a str>,
) -> Result<(), RetrievalPortError> {
    let fixed_bytes = fixed_parameters.iter().try_fold(0usize, |bytes, value| {
        let value_bytes = match value {
            Value::Text(value) => value.len(),
            Value::Blob(value) => value.len(),
            Value::Null | Value::Integer(_) | Value::Real(_) => 0,
        };
        bytes
            .checked_add(value_bytes)
            .ok_or(RetrievalPortError::BudgetExceeded)
    })?;
    let total_bytes = dynamic_text
        .into_iter()
        .try_fold(fixed_bytes, |bytes, value| {
            bytes
                .checked_add(value.len())
                .ok_or(RetrievalPortError::BudgetExceeded)
        })?;
    if total_bytes > maximum_bytes {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    Ok(())
}

/// The rarest fixed number of distinct n-grams forms a selective, bounded
/// prefilter. It may admit a superset for a very long phrase; the row-level
/// substring check remains the correctness authority before scoring.
pub(super) fn ngram_document_query(
    connection: &Connection,
    kind: i64,
    bytes: &[u8],
    metrics: &ArtifactQueryMetricsV1,
) -> Result<RoaringBitmap, RetrievalPortError> {
    {
        let _span = tracing::trace_span!("query.artifact.ngram.bitmap_query").entered();
        {
            let ngrams = query_ngrams(bytes)
                .into_iter()
                .map(|ngram| (kind, ngram))
                .collect::<Vec<_>>();
            if ngrams.is_empty() {
                return Ok(RoaringBitmap::new());
            }
            let candidates = ngram_bitmap_candidates(connection, &ngrams, metrics)?;

            Ok(candidates)
        }
    }
}

#[derive(Clone, Copy)]
struct NgramSelectivityV1 {
    kind: i64,
    ngram: u32,
    cardinality: u64,
}

/// One query's n-gram budget: the encoded-byte allowance every intersection
/// charges against.
struct NgramListBudgetV1 {
    remaining_encoded_bytes: usize,
}

impl NgramListBudgetV1 {
    fn for_query() -> Self {
        Self {
            remaining_encoded_bytes: ARTIFACT_NGRAM_QUERY_ENCODED_BYTES_V1,
        }
    }
}

pub(super) fn ngram_bitmap_candidates(
    connection: &Connection,
    ngrams: &[(i64, u32)],
    _metrics: &ArtifactQueryMetricsV1,
) -> Result<RoaringBitmap, RetrievalPortError> {
    let mut budget = NgramListBudgetV1::for_query();
    let mut selectivities = Vec::with_capacity(ngrams.len());
    let mut selectivity_statement = connection
        .prepare_cached(
            "SELECT document_frequency FROM ngram_postings WHERE kind = ?1 AND ngram = ?2",
        )
        .map_err(map_query_sql_error)?;
    for &(kind, ngram) in ngrams {
        let cardinality = selectivity_statement
            .query_row([kind, i64::from(ngram)], |row| row.get::<_, i64>(0))
            .optional()
            .map_err(map_query_sql_error)?;
        let Some(cardinality) = cardinality else {
            return Ok(RoaringBitmap::new());
        };
        let cardinality = u64::try_from(cardinality).map_err(contract_error)?;
        ensure_ngram_candidate_cardinality(cardinality)?;
        selectivities.push(NgramSelectivityV1 {
            kind,
            ngram,
            cardinality,
        });
    }
    drop(selectivity_statement);
    selectivities.sort_unstable_by_key(|selectivity| {
        (selectivity.cardinality, selectivity.kind, selectivity.ngram)
    });
    selectivities.truncate(ARTIFACT_NGRAM_INTERSECTION_SCRATCH_V1);

    let mut candidates = None::<RoaringBitmap>;
    let mut list_statement = connection
        .prepare_cached(
            "SELECT documents, document_frequency FROM ngram_postings WHERE kind = ?1 AND ngram = ?2",
        )
        .map_err(map_query_sql_error)?;
    for selectivity in selectivities {
        let (encoded, cardinality): (Vec<u8>, i64) = list_statement
            .query_row([selectivity.kind, i64::from(selectivity.ngram)], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(map_query_sql_error)?;
        let encoded = document_set_bytes(&encoded).map_err(map_query_artifact_error)?;
        charge_ngram_encoded_list_bytes(
            &mut budget.remaining_encoded_bytes,
            encoded.len(),
            ARTIFACT_NGRAM_MAX_ENCODED_LIST_BYTES_V1,
        )?;
        let mut list = decode_document_set(&encoded).map_err(map_query_artifact_error)?;
        if i64::try_from(list.len()).map_err(contract_error)? != cardinality {
            return Err(RetrievalPortError::Contract(
                "lexical artifact ngram list cardinality changed after verification".to_owned(),
            ));
        }
        if let Some(current) = candidates.as_ref() {
            list &= current;
        }
        ensure_ngram_candidate_cardinality(list.len())?;
        #[cfg(test)]
        {
            _metrics.observe_ngram_list();
            _metrics.observe_ngram_candidates(list.len());
        }
        let exhausted = list.is_empty();
        candidates = Some(list);
        if exhausted {
            break;
        }
    }
    Ok(candidates.unwrap_or_default())
}

pub(super) fn ensure_ngram_candidate_cardinality(
    cardinality: u64,
) -> Result<(), RetrievalPortError> {
    if cardinality > ARTIFACT_NGRAM_MAX_CANDIDATES_V1 {
        Err(RetrievalPortError::BudgetExceeded)
    } else {
        Ok(())
    }
}

pub(super) fn charge_ngram_encoded_list_bytes(
    remaining_bytes: &mut usize,
    list_bytes: usize,
    maximum_list_bytes: usize,
) -> Result<(), RetrievalPortError> {
    if list_bytes > maximum_list_bytes {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    *remaining_bytes = remaining_bytes
        .checked_sub(list_bytes)
        .ok_or(RetrievalPortError::BudgetExceeded)?;
    Ok(())
}

impl<'a> ArtifactQueryV1<'a> {
    pub(super) fn new(
        connection: &'a Connection,
        metadata: &'a super::super::super::CodeLexicalProjectionMetadataV1,
        receipt: &'a VerifiedCodeLexicalArtifactV1,
        fuzzy_vocabulary: &'a OnceLock<Arc<FuzzyVocabularyV1>>,
        prefaces: &'a ScoringPrefaceIndexV1,
    ) -> Result<Self, RetrievalPortError> {
        Ok(Self {
            connection,
            metadata,
            // Admitted documents, which the sealed `rows` section counts;
            // source chunks the projection does not index are not documents.
            document_count: receipt
                .section_digests()
                .iter()
                .find(|section| section.name == "rows")
                .map(|section| usize::try_from(section.row_count).map_err(contract_error))
                .transpose()?
                .ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "lexical artifact receipt has no rows section".to_owned(),
                    )
                })?,
            metrics: ArtifactQueryMetricsV1::default(),
            fuzzy_vocabulary,
            row_dictionary: ConnectionRowDictionaryV1::new(connection),
            row_blocks: RowBlocksV1::new(connection, prefaces),
        })
    }

    pub(super) fn lexical_batch(
        &self,
        request: &LexicalLaneRequest<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>, RetrievalPortError> {
        let control = request.control;
        let fuzzy = self.fuzzy_expansions(request)?;
        let prepared = PreparedLexicalQueryV1::new(request);
        let terms = lexical_terms(&prepared, &fuzzy);
        let stats = self.lexical_stats(&terms)?;
        retrieval_checkpoint(control)?;
        let mut phrase_queries = BTreeMap::new();
        for (_, normalized) in &prepared.phrases {
            let query = ngram_document_query(
                self.connection,
                NGRAM_NORMALIZED,
                normalized.as_bytes(),
                &self.metrics,
            )?;
            phrase_queries.insert(normalized.clone(), query);
        }
        let mut phrase_frequencies = phrase_queries
            .keys()
            .cloned()
            .map(|phrase| (phrase, 0usize))
            .collect::<BTreeMap<_, _>>();
        let phrase_documents = phrase_queries
            .values()
            .fold(RoaringBitmap::new(), |union, documents| union | documents);
        visit_lexical_rows(
            self.connection,
            &self.row_blocks,
            &phrase_documents,
            &RequestTermPostingsV1::default(),
            &self.metrics,
            control,
            |_, stored, _| {
                let row = self.decode_row(&stored).map_err(map_query_artifact_error)?;
                let field_texts = row_field_texts(&row);
                // Frequency counts documents once per normalized phrase, even
                // when the request includes multiple original spellings.
                for (phrase, frequency) in &mut phrase_frequencies {
                    if field_texts
                        .iter()
                        .any(|(_, text)| substring_count(text, phrase) > 0)
                    {
                        *frequency += 1;
                    }
                }
                Ok(())
            },
        )?;
        let mut pruned = Vec::new();
        let documents =
            self.lexical_documents(request, &fuzzy, &stats, &phrase_queries, &mut pruned)?;
        let mut text_documents = phrase_documents;
        if !prepared.proximities.is_empty() {
            text_documents |= self.proximity_documents(&prepared, &stats)?;
        }
        // Term and fuzzy scores need field lengths, which the block preface
        // stores uncompressed. Phrase and proximity counts still read text,
        // and only the cap-bounded winners are inflated into candidates.
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let (selected, eligible, excluded) = self.select_lexical_winners(
            &documents,
            &text_documents,
            &prepared,
            &fuzzy,
            &phrase_frequencies,
            &stats,
            &request.field_filters,
            request.path_prefix,
            cap,
            control,
        )?;
        let truncated = eligible - selected.len() as u64;
        let mut winner_documents = selected
            .iter()
            .map(|entry| entry.document)
            .collect::<Vec<_>>();
        winner_documents.sort_unstable();
        let mut winner_rows = BTreeMap::new();
        for document in winner_documents {
            retrieval_checkpoint(control)?;
            winner_rows.insert(document, self.row(document)?);
        }
        let mut candidates = Vec::with_capacity(selected.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in selected.into_iter().enumerate() {
            if ordinal.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                retrieval_checkpoint(control)?;
            }
            let row = winner_rows.remove(&entry.document).ok_or_else(|| {
                RetrievalPortError::Contract("lexical winner row was not read".to_owned())
            })?;
            self.preface_matches_row(&entry.draft, &row)?;
            let frequencies = &entry.draft.frequencies;
            let score = self.score_row(
                &row,
                &prepared,
                &fuzzy,
                &phrase_frequencies,
                &stats,
                frequencies,
                &request.field_filters,
            );
            let ranking =
                admitted_score_micros(&score, &request.field_filters)?.ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "lexical winner lost the score that selected it".to_owned(),
                    )
                })?;
            if ranking != entry.rank {
                return Err(RetrievalPortError::Contract(
                    "lexical scoring preface diverged from the stored row".to_owned(),
                ));
            }
            let mut candidate = lexical_lane_candidate(
                &row,
                &self.metadata.freshness,
                self.metadata.repository_id.clone(),
                RetrieverKind::Lexical,
                self.metadata.lexical_retriever_revision.clone(),
                request.score_domain.clone(),
                None,
            )?;
            candidate.ordinal_rank = ordinal as u32;
            let definition_match = lexical_definition_match(&row, &prepared, &fuzzy);
            let evidence = LexicalLaneEvidence {
                binding: lexical_lane_binding(&row, &candidate, score.matched_kinds),
                field_scores_micros: score.field_scores,
                matched_whole_terms: score.matched_whole_terms,
                matched_subtokens: score.matched_subtokens,
                matched_phrases: score.matched_phrases,
                matched_proximities: score.matched_proximities,
                spelling_variants: score.spelling_variants,
                typo_recovery_applied: score.typo_recovery_applied,
                echo_penalty_applied: score.echo_penalty_applied,
                source_role: classify_source_role(&row.logical_path, definition_match),
            };
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        retrieval_checkpoint(control)?;
        Ok(candidate_admission_outcome(
            capped_batch(
                self.document_count,
                eligible,
                excluded,
                truncated,
                candidates,
                evidence_by_occurrence,
            ),
            pruned,
        ))
    }

    pub(super) fn exact_batch<A: ExactAdmissionAuthority>(
        &self,
        request: &ExactLaneRequest,
        authority: &A,
    ) -> Result<RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>, RetrievalPortError> {
        retrieval_checkpoint(request.control)?;
        let documents = self.exact_documents(request)?;
        // Same bounded selection as the lexical lane: keys mirror the exact
        // lane's canonical order (production-definition rank, then
        // matched-literal specificity, then occurrence), and only the
        // selected winners are rehydrated into evidence.
        // Central admission runs BEFORE heap eligibility: a document whose
        // matched literals are all denied is excluded, never selected, so a
        // denied best match can never displace an admitted candidate or
        // fail the batch. Ownership needs every matched row before any row
        // answers, so matched rows are held until the visit ends; the heap
        // retains at most `cap` ranking keys, and proofs are admitted at
        // most once per request literal and cloned only for winners.
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut owners = ExactOccurrenceOwnersV1::default();
        let mut matched_rows = Vec::new();
        self.visit_documents(&documents, request.control, |document| {
            let row = self.row(document)?;
            let matches = exact_matches_artifact(&row, request);
            if !matches.is_empty() {
                owners.offer(&row.anchor, document, &matches);
                matched_rows.push((
                    document,
                    row.id.as_str().to_owned(),
                    row.anchor.file_occurrence_id,
                    classify_source_role(
                        &row.logical_path,
                        exact_definition_match(
                            row.anchor.grain,
                            row.symbol_simple_name.as_deref(),
                            exact_matched_symbol_names(&matches, request),
                        ),
                    ),
                    matches,
                ));
            }
            Ok(())
        })?;
        let mut proofs = LiteralProofCacheV1::new(request.literals.len());
        let mut admitted_rows = Vec::new();
        let mut document_frequencies = vec![0usize; request.literals.len()];
        for (visited, (document, row_id, file, source_role, matches)) in
            matched_rows.into_iter().enumerate()
        {
            if visited.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                retrieval_checkpoint(request.control)?;
            }
            let (matched_literals, matched_kinds) = owners.owned(&file, document, &matches);
            if matched_literals.is_empty() {
                continue;
            }
            let Some((admitted_ordinal, _)) =
                proofs.first_admitted(&matched_literals, request, authority)?
            else {
                continue;
            };
            for &literal in &matched_literals {
                document_frequencies[literal] += 1;
            }
            admitted_rows.push((
                document,
                row_id,
                source_role,
                admitted_ordinal,
                matched_literals,
                matched_kinds,
            ));
        }
        // Specificity needs every admitted row's literals first. It keys the
        // cap, so a common query word's matches cannot crowd out the rare
        // identifier the query names.
        let specificity = ExactLiteralSpecificityV1::new(&document_frequencies);
        let eligible = admitted_rows.len() as u64;
        let excluded = (self.document_count as u64).saturating_sub(eligible);
        let mut ranked = BinaryHeap::new();
        for (
            visited,
            (document, row_id, source_role, admitted_ordinal, matched_literals, matched_kinds),
        ) in admitted_rows.into_iter().enumerate()
        {
            if visited.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                retrieval_checkpoint(request.control)?;
            }
            let specificity_micros = specificity.share_micros(&matched_literals);
            retain_bounded(
                &mut ranked,
                cap,
                Keyed {
                    key: (
                        source_role.admission_rank(),
                        Reverse(specificity_micros),
                        row_id,
                        document,
                    ),
                    value: (
                        admitted_ordinal,
                        matched_literals,
                        matched_kinds,
                        source_role,
                    ),
                },
            );
        }
        retrieval_checkpoint(request.control)?;
        let selected = ranked.into_sorted_vec();
        let truncated = eligible - selected.len() as u64;
        // Winners re-read in document order so each row block inflates once.
        let mut winner_documents = selected.iter().map(|entry| entry.key.3).collect::<Vec<_>>();
        winner_documents.sort_unstable();
        let mut winner_rows = BTreeMap::new();
        for document in winner_documents {
            winner_rows.insert(document, self.row(document)?);
        }
        let mut candidates = Vec::with_capacity(selected.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in selected.into_iter().enumerate() {
            if ordinal.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                retrieval_checkpoint(request.control)?;
            }
            let Keyed {
                key: (_, Reverse(specificity_micros), _, document),
                value: (admitted_ordinal, matched_literals, matched_kinds, source_role),
            } = entry;
            let proof = proofs.admitted_proof(admitted_ordinal)?;
            let matched_literals = matched_literals
                .iter()
                .map(|literal| request.literals[*literal].clone())
                .collect::<Vec<_>>();
            let row = winner_rows.remove(&document).ok_or_else(|| {
                RetrievalPortError::Contract("exact lane winner row was not read".to_owned())
            })?;
            let mut candidate = lexical_lane_candidate(
                &row,
                &self.metadata.freshness,
                self.metadata.repository_id.clone(),
                RetrieverKind::ExactLiteral,
                self.metadata.exact_retriever_revision.clone(),
                self.metadata.exact_score_domain.clone(),
                Some(proof.clone()),
            )?;
            candidate.ordinal_rank = ordinal as u32;
            let evidence = ExactLaneEvidence {
                binding: lexical_lane_binding(&row, &candidate, matched_kinds),
                matched_literals,
                admission_proof: proof,
                source_role,
                specificity_micros,
            };
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        retrieval_checkpoint(request.control)?;
        Ok(RetrieverOutcome::Complete(capped_batch(
            self.document_count,
            eligible,
            excluded,
            truncated,
            candidates,
            evidence_by_occurrence,
        )))
    }

    fn proximity_documents(
        &self,
        prepared: &PreparedLexicalQueryV1<'_>,
        stats: &LexicalStatsCacheV1,
    ) -> Result<RoaringBitmap, RetrievalPortError> {
        let mut matched = RoaringBitmap::new();
        for proximity in &prepared.proximities {
            let mut intersection = None;
            for term in &proximity.terms {
                let documents = stats
                    .postings
                    .documents(term, |field| field != LexicalFieldV1::Subtoken)
                    .map_err(map_query_artifact_error)?;
                intersection = Some(match intersection {
                    Some(current) => current & documents,
                    None => documents,
                });
            }
            if let Some(documents) = intersection {
                matched |= documents;
            }
        }
        Ok(matched)
    }

    /// Keep one transient scoring record and only cap-bounded winners.
    /// Phrase frequencies are complete before this scan; ordinary term queries
    /// need only the uncompressed preface and posting cursors.
    #[allow(clippy::too_many_arguments)]
    fn select_lexical_winners(
        &self,
        documents: &RoaringBitmap,
        text_documents: &RoaringBitmap,
        prepared: &PreparedLexicalQueryV1<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_frequencies: &BTreeMap<String, usize>,
        stats: &LexicalStatsCacheV1,
        filters: &[LexicalFieldFilterV1],
        path_prefix: Option<&str>,
        cap: usize,
        control: &dyn RetrievalExecutionControl,
    ) -> Result<(Vec<SelectedLexicalV1>, u64, u64), RetrievalPortError> {
        {
            let _span = tracing::trace_span!("query.stream.score_from_preface").entered();
            {
                let mut cursors = stats.postings.cursors().map_err(map_query_artifact_error)?;
                let mut ranked = LexicalWinnerHeap::new();
                let mut excluded = self.document_count as u64;
                let mut eligible = 0u64;
                for (ordinal, document) in documents.iter().enumerate() {
                    if ordinal.is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE) {
                        retrieval_checkpoint(control)?;
                    }
                    let preface = self
                        .row_blocks
                        .scoring_preface(document)
                        .map_err(map_query_artifact_error)?;
                    let mut entries = Vec::new();
                    for cursor in &mut cursors {
                        if let Some(frequency) = cursor
                            .frequency_at(document)
                            .map_err(map_query_artifact_error)?
                        {
                            entries.push((
                                cursor.field,
                                cursor.term.to_owned(),
                                usize::try_from(frequency).map_err(contract_error)?,
                            ));
                        }
                    }
                    let mut phrase_tfs = Vec::new();
                    let mut proximity_tfs = Vec::new();
                    if text_documents.contains(document) {
                        let stored = self
                            .row_blocks
                            .row(document)
                            .map_err(map_query_artifact_error)?;
                        let row = self.decode_row(&stored).map_err(map_query_artifact_error)?;
                        if preface.chunk_id != row.id.as_str()
                            || preface.field_lengths != row.field_lengths
                            || preface.trimmed_normalized_len != row.normalized_text.trim().len()
                        {
                            return Err(RetrievalPortError::Contract(
                                "lexical scoring preface does not match its row".to_owned(),
                            ));
                        }
                        let field_texts = row_field_texts(&row);
                        let field_tokens: Vec<(LexicalFieldV1, Vec<String>)> =
                            if prepared.proximities.is_empty() {
                                Vec::new()
                            } else {
                                field_texts
                                    .iter()
                                    .map(|(field, text)| (*field, proximity_tokens(text)))
                                    .collect()
                            };
                        for (index, (_, normalized)) in prepared.phrases.iter().enumerate() {
                            for (field, text) in &field_texts {
                                let count = substring_count(text, normalized);
                                if count > 0 {
                                    phrase_tfs.push((*field, index, count));
                                }
                            }
                        }
                        for (index, proximity) in prepared.proximities.iter().enumerate() {
                            for (field, tokens) in &field_tokens {
                                let count = proximity_count_tokens(
                                    tokens,
                                    &proximity.terms,
                                    proximity.original.maximum_gap,
                                );
                                if count > 0 {
                                    proximity_tfs.push((*field, index, count));
                                }
                            }
                        }
                    }
                    let draft = LexicalDraftV1 {
                        chunk_id: preface.chunk_id,
                        field_lengths: preface.field_lengths,
                        frequencies: LexicalTermFrequenciesV1(entries),
                        phrase_tfs,
                        proximity_tfs,
                        trimmed_normalized_len: preface.trimmed_normalized_len,
                    };
                    let score = self.score_draft(
                        &draft,
                        prepared,
                        fuzzy,
                        phrase_frequencies,
                        stats,
                        false,
                        filters,
                    );
                    let Some(upper) = admitted_score_micros(&score, filters)? else {
                        continue;
                    };
                    // ponytail: the preface has no path, so a scoped read inflates
                    // every admitted row; store the path in the preface if scoped
                    // queries over large trees get slow.
                    if path_prefix.is_some()
                        && !tracedecay_domain::path_matches_scope(
                            &self.row(document)?.logical_path,
                            path_prefix,
                        )
                    {
                        continue;
                    }
                    eligible += 1;
                    excluded = excluded.saturating_sub(1);
                    if cap == 0 {
                        continue;
                    }
                    // Echo only lowers the score. A candidate whose upper bound
                    // loses to the current worst winner never needs text hydration.
                    // Name/signature matches may be production definitions; those
                    // outrank every test hit, so their path is read before the
                    // bound can reject them. Other hits share one measured-score
                    // tier and do not inflate just to classify a test path.
                    let role_rank =
                        lexical_admission_rank(self, document, &draft, prepared, fuzzy)?;
                    let best_key = (role_rank, Reverse(upper), draft.chunk_id.clone(), document);
                    if ranked.len() == cap
                        && ranked.peek().is_some_and(|worst| best_key >= worst.key)
                    {
                        continue;
                    }
                    let echo_possible = !prepared.echo_query.is_empty()
                        && draft.trimmed_normalized_len == prepared.echo_query.len();
                    let rank = if echo_possible {
                        retrieval_checkpoint(control)?;
                        let row = self.row(document)?;
                        self.preface_matches_row(&draft, &row)?;
                        let score = self.score_row(
                            &row,
                            prepared,
                            fuzzy,
                            phrase_frequencies,
                            stats,
                            &draft.frequencies,
                            filters,
                        );
                        admitted_score_micros(&score, filters)?.ok_or_else(|| {
                            RetrievalPortError::Contract(
                                "lexical echo confirmation lost an admitted score".to_owned(),
                            )
                        })?
                    } else {
                        upper
                    };
                    retain_bounded(
                        &mut ranked,
                        cap,
                        Keyed {
                            key: (role_rank, Reverse(rank), draft.chunk_id.clone(), document),
                            value: SelectedLexicalV1 {
                                draft,
                                document,
                                rank,
                            },
                        },
                    );
                }
                retrieval_checkpoint(control)?;
                Ok((
                    ranked
                        .into_sorted_vec()
                        .into_iter()
                        .map(|entry| entry.value)
                        .collect(),
                    eligible,
                    excluded,
                ))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn score_draft(
        &self,
        draft: &LexicalDraftV1,
        prepared: &PreparedLexicalQueryV1<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_frequencies: &BTreeMap<String, usize>,
        stats: &LexicalStatsCacheV1,
        echo_penalty: bool,
        field_filters: &[LexicalFieldFilterV1],
    ) -> LexicalRowScoreV1 {
        score_lexical_row(
            &draft.field_lengths,
            &[],
            prepared,
            field_filters,
            fuzzy,
            phrase_frequencies,
            |field, term| term_frequency(&draft.frequencies, field, term),
            |field, term| stats.document_frequency(field, term),
            |field, term_frequency, document_frequency| {
                self.term_score_with_lengths(
                    field,
                    term_frequency,
                    &draft.field_lengths,
                    document_frequency,
                    stats,
                )
            },
            |field, phrase| {
                prepared
                    .phrases
                    .iter()
                    .position(|(_, normalized)| normalized == phrase)
                    .and_then(|index| {
                        draft
                            .phrase_tfs
                            .iter()
                            .find_map(|(matched, matched_index, count)| {
                                (*matched == field && *matched_index == index).then_some(*count)
                            })
                    })
                    .unwrap_or(0)
            },
            |field, proximity| {
                prepared
                    .proximities
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, proximity))
                    .and_then(|index| {
                        draft
                            .proximity_tfs
                            .iter()
                            .find_map(|(matched, matched_index, count)| {
                                (*matched == field && *matched_index == index).then_some(*count)
                            })
                    })
                    .unwrap_or(0)
            },
            echo_penalty,
        )
    }

    fn preface_matches_row(
        &self,
        draft: &LexicalDraftV1,
        row: &ArtifactRowV1,
    ) -> Result<(), RetrievalPortError> {
        if draft.chunk_id == row.id.as_str()
            && draft.field_lengths == row.field_lengths
            && draft.trimmed_normalized_len == row.normalized_text.trim().len()
        {
            Ok(())
        } else {
            Err(RetrievalPortError::Contract(
                "lexical scoring preface does not match its row".to_owned(),
            ))
        }
    }

    fn row(&self, document: u32) -> Result<ArtifactRowV1, RetrievalPortError> {
        self.metrics.probe();
        let stored = self
            .row_blocks
            .row(document)
            .map_err(map_query_artifact_error)?;
        self.decode_row(&stored).map_err(map_query_artifact_error)
    }

    fn decode_row(
        &self,
        stored: &StoredRowV1,
    ) -> Result<ArtifactRowV1, CodeLexicalArtifactErrorV1> {
        decode_artifact_row(
            &self.metadata.generation,
            &stored.chunk_id,
            &stored.row,
            &stored.text,
            &self.row_dictionary,
        )
    }

    /// The candidate document set for one lexical request: every phrase
    /// match plus the term sources `admit_candidate_sources` keeps under
    /// `MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1`.
    fn lexical_documents(
        &self,
        request: &LexicalLaneRequest<'_>,
        fuzzy: &FuzzyExpansionsV1,
        stats: &LexicalStatsCacheV1,
        phrase_queries: &BTreeMap<String, RoaringBitmap>,
        pruned: &mut Vec<(String, u64)>,
    ) -> Result<RoaringBitmap, RetrievalPortError> {
        let mut whole_terms = Vec::new();
        for term in request.whole_terms.iter() {
            whole_terms.push(normalize_lexical(term));
            if let Some(expansions) = fuzzy.by_query.get(term) {
                whole_terms.extend(expansions.iter().cloned());
            }
        }
        whole_terms.extend(
            request
                .proximities
                .iter()
                .flat_map(|proximity| &proximity.terms)
                .map(|term| normalize_lexical(term)),
        );
        let subtokens = request
            .subtokens
            .iter()
            .map(|subtoken| normalize_lexical(subtoken))
            .collect::<Vec<_>>();
        let mut sources = Vec::with_capacity(whole_terms.len() + subtokens.len());
        for term in whole_terms {
            if stats.postings.by_term.contains_key(&term) {
                sources.push((stats.whole_term_documents(&term), (term, false)));
            }
        }
        for subtoken in subtokens {
            if stats.postings.by_term.contains_key(&subtoken) {
                sources.push((
                    stats.document_frequency(LexicalFieldV1::Subtoken, &subtoken),
                    (subtoken, true),
                ));
            }
        }
        let mut documents = phrase_queries
            .values()
            .fold(RoaringBitmap::new(), |union, documents| union | documents);
        for (term, subtoken) in admit_candidate_sources(sources, |frequency, (term, _)| {
            pruned.push((term.clone(), frequency as u64));
        }) {
            documents |= stats
                .postings
                .documents(&term, |field| {
                    (field == LexicalFieldV1::Subtoken) == subtoken
                })
                .map_err(map_query_artifact_error)?;
        }
        Ok(documents)
    }

    fn exact_documents(
        &self,
        request: &ExactLaneRequest,
    ) -> Result<RoaringBitmap, RetrievalPortError> {
        let mut documents = RoaringBitmap::new();
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT documents FROM exact_postings WHERE term_id = ?1 AND field = ?2",
            )
            .map_err(map_query_sql_error)?;
        for literal in &request.literals {
            if matches!(
                literal.field,
                ExactFieldV1::QuotedPhrase
                    | ExactFieldV1::DiagnosticText
                    | ExactFieldV1::CompilerOrRuntimeError
            ) {
                documents |= ngram_document_query(
                    self.connection,
                    NGRAM_NORMALIZED,
                    &literal.original_bytes,
                    &self.metrics,
                )?;
                if let Some(ngrams) = raw_override_query_ngrams(&literal.original_bytes) {
                    documents |= ngram_bitmap_candidates(self.connection, &ngrams, &self.metrics)?;
                }
            }
            self.metrics.probe();
            let encoded: Option<Vec<u8>> = statement
                .query_row(
                    [
                        stable_exact_term_id(&literal.canonical_bytes),
                        exact_field_code(literal.field),
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_query_sql_error)?;
            if let Some(encoded) = encoded {
                documents |= decode_ngram_bitmap(&encoded).map_err(map_query_artifact_error)?;
            }
        }
        Ok(documents)
    }

    fn visit_documents(
        &self,
        documents: &RoaringBitmap,
        control: &dyn RetrievalExecutionControl,
        visitor: impl FnMut(u32) -> Result<(), RetrievalPortError>,
    ) -> Result<(), RetrievalPortError> {
        self.metrics.probe();
        visit_document_ids(documents, control, visitor)
    }

    #[tracing::instrument(name = "query.lane.fuzzy.expand", level = "trace", skip_all)]
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
            } else {
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
        }
        if groups.is_empty() {
            return Ok(FuzzyExpansionsV1::default());
        }
        groups.sort_by_key(|group| group.first_ordinal);
        let maximum_distance = groups.iter().map(|group| group.bound).max().unwrap_or(0);
        let vocabulary = self.load_vocabulary()?;
        let mut selected = Vec::with_capacity(limit);
        let mut scratch = EditDistanceScratchV1::default();
        'distance: for distance in 1..=maximum_distance {
            for (group_index, group) in groups.iter_mut().enumerate() {
                let remaining = limit.saturating_sub(selected.len());
                if remaining == 0 {
                    break 'distance;
                }
                if distance > group.bound {
                    continue;
                }
                scratch.prepare_query(&group.normalized_query);
                let query_len = scratch.query_chars.len();
                // Levenshtein distance is at least the character-length gap,
                // so terms outside this window cannot match at `distance`.
                // Indices stay in vocabulary load order, which is the order
                // the expansion budget fills.
                let candidates = vocabulary.indices_within(
                    query_len.saturating_sub(distance),
                    query_len.saturating_add(distance),
                );
                let mut added = 0usize;
                for index in candidates {
                    if added >= remaining {
                        break;
                    }
                    let term = &vocabulary.terms[usize::try_from(index).map_err(contract_error)?];
                    if term != &group.normalized_query
                        && scratch.bounded_edit_distance(term, distance) == Some(distance)
                        && group.seen.insert(term.clone())
                    {
                        selected.push((group_index, term.clone()));
                        added += 1;
                    }
                }
            }
        }
        let mut by_query = BTreeMap::<String, BTreeSet<String>>::new();
        for (group_index, term) in selected {
            for query in &groups[group_index].queries {
                by_query
                    .entry(query.clone())
                    .or_default()
                    .insert(term.clone());
            }
        }
        Ok(FuzzyExpansionsV1 { by_query })
    }

    #[tracing::instrument(name = "query.artifact.vocabulary.load", level = "trace", skip_all)]
    fn load_vocabulary(&self) -> Result<Arc<FuzzyVocabularyV1>, RetrievalPortError> {
        if let Some(cached) = self.fuzzy_vocabulary.get() {
            return Ok(Arc::clone(cached));
        }
        let loaded = self.load_vocabulary_from_sqlite()?;
        Ok(Arc::clone(self.fuzzy_vocabulary.get_or_init(|| loaded)))
    }

    /// One pass over the term-keyed table; its lists sit after both columns
    /// read here, so the walk never follows a list's overflow pages.
    pub(super) const VOCABULARY_SQL: &'static str =
        "SELECT term FROM term_postings WHERE in_fuzzy = 1";

    fn load_vocabulary_from_sqlite(&self) -> Result<Arc<FuzzyVocabularyV1>, RetrievalPortError> {
        self.metrics.probe();
        let mut statement = self
            .connection
            .prepare_cached(Self::VOCABULARY_SQL)
            .map_err(map_query_sql_error)?;
        let mut rows = statement.query([]).map_err(map_query_sql_error)?;
        let mut vocabulary = Vec::new();
        while let Some(row) = rows.next().map_err(map_query_sql_error)? {
            vocabulary.push(row.get(0).map_err(map_query_sql_error)?);
        }
        drop(rows);
        self.metrics.observe_statement(&statement)?;
        Ok(Arc::new(FuzzyVocabularyV1::from_terms(vocabulary)?))
    }

    /// Read the document-independent scoring statistics once per request.
    ///
    /// Per-field totals and per-(field, term) document frequencies depend
    /// only on the artifact corpus and the query terms, so one upfront read
    /// replaces the two SQL probes each scored document would otherwise
    /// repeat per term.
    #[tracing::instrument(name = "query.artifact.stats.read", level = "trace", skip_all)]
    fn lexical_stats(
        &self,
        terms: &BTreeSet<String>,
    ) -> Result<LexicalStatsCacheV1, RetrievalPortError> {
        ensure_sqlite_bind_capacity(0, terms.len())?;
        ensure_sqlite_bound_value_bytes(
            ARTIFACT_SQLITE_MAX_BOUND_VALUE_BYTES_V1,
            &[],
            terms.iter().map(String::as_str),
        )?;
        let mut field_totals = BTreeMap::new();
        self.metrics.probe();
        let mut statement = self
            .connection
            .prepare_cached("SELECT field, total_length FROM field_stats")
            .map_err(map_query_sql_error)?;
        let mut rows = statement.query([]).map_err(map_query_sql_error)?;
        while let Some(row) = rows.next().map_err(map_query_sql_error)? {
            let field = field_from_code(row.get::<_, i64>(0).map_err(map_query_sql_error)?)
                .map_err(map_query_artifact_error)?;
            let total: i64 = row.get(1).map_err(map_query_sql_error)?;
            field_totals.insert(field, usize::try_from(total).map_err(contract_error)?);
        }
        drop(rows);
        self.metrics.observe_statement(&statement)?;
        let mut document_frequencies = BTreeMap::<LexicalFieldV1, BTreeMap<String, usize>>::new();
        let mut postings = RequestTermPostingsV1::default();
        if !terms.is_empty() {
            for term in terms {
                postings.by_term.insert(term.clone(), Vec::new());
            }
            let placeholders = std::iter::repeat_n("?", terms.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query =
                format!("SELECT term, lists FROM term_postings WHERE term IN ({placeholders})");
            self.metrics.probe();
            let mut statement = self
                .connection
                .prepare(&query)
                .map_err(map_query_sql_error)?;
            let mut rows = statement
                .query(params_from_iter(terms.iter()))
                .map_err(map_query_sql_error)?;
            let mut remaining_bytes = ARTIFACT_TERM_POSTING_QUERY_BYTES_V1;
            while let Some(row) = rows.next().map_err(map_query_sql_error)? {
                let term: String = row.get(0).map_err(map_query_sql_error)?;
                let stored = row
                    .get_ref(1)
                    .and_then(|value| value.as_blob().map_err(rusqlite::Error::from))
                    .map_err(map_query_sql_error)?;
                let encoded = term_lists_bytes(stored).map_err(map_query_artifact_error)?;
                let encoded = encoded.as_ref();
                remaining_bytes = remaining_bytes
                    .checked_sub(encoded.len())
                    .ok_or(RetrievalPortError::BudgetExceeded)?;
                let Some(lists) = postings.by_term.get_mut(&term) else {
                    continue;
                };
                for (field, document_frequency, list) in
                    decode_term_lists(encoded).map_err(map_query_artifact_error)?
                {
                    let field = field_from_code(field).map_err(map_query_artifact_error)?;
                    document_frequencies.entry(field).or_default().insert(
                        term.clone(),
                        usize::try_from(document_frequency).map_err(contract_error)?,
                    );
                    lists.push(TermFieldPostingsV1 {
                        field,
                        postings: list.to_vec(),
                    });
                }
            }
            drop(rows);
            self.metrics.observe_statement(&statement)?;
        }
        Ok(LexicalStatsCacheV1 {
            field_totals,
            document_frequencies,
            postings,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn score_row(
        &self,
        row: &ArtifactRowV1,
        prepared: &PreparedLexicalQueryV1<'_>,
        fuzzy: &FuzzyExpansionsV1,
        phrase_frequencies: &BTreeMap<String, usize>,
        stats: &LexicalStatsCacheV1,
        frequencies: &LexicalTermFrequenciesV1,
        field_filters: &[LexicalFieldFilterV1],
    ) -> LexicalRowScoreV1 {
        crate::observe::measure_frequent("query.lane.lexical.score_row", || {
            // Phrase and proximity scoring asks one (field, term) count at a
            // time; normalize each field's text and tokens once per row so a
            // query with T terms across F fields scans F texts, not T * F * F.
            let field_texts: Vec<(LexicalFieldV1, Cow<'_, str>)> =
                if prepared.phrases.is_empty() && prepared.proximities.is_empty() {
                    Vec::new()
                } else {
                    row_field_texts(row)
                };
            let field_tokens: Vec<(LexicalFieldV1, Vec<String>)> =
                if prepared.proximities.is_empty() {
                    Vec::new()
                } else {
                    field_texts
                        .iter()
                        .map(|(field, text)| (*field, proximity_tokens(text)))
                        .collect()
                };
            score_lexical_row(
                row.field_lengths(),
                &row.exact_terms,
                prepared,
                field_filters,
                fuzzy,
                phrase_frequencies,
                |field, term| term_frequency(frequencies, field, term),
                |field, term| stats.document_frequency(field, term),
                |field, term_frequency, document_frequency| {
                    self.term_score_with_df(field, term_frequency, row, document_frequency, stats)
                },
                |field, phrase| {
                    field_texts
                        .iter()
                        .find(|(matched, _)| *matched == field)
                        .map(|(_, text)| substring_count(text, phrase))
                        .unwrap_or(0)
                },
                |field, proximity| {
                    field_tokens
                        .iter()
                        .find(|(matched, _)| *matched == field)
                        .map(|(_, tokens)| {
                            proximity_count_tokens(
                                tokens,
                                &proximity.terms,
                                proximity.original.maximum_gap,
                            )
                        })
                        .unwrap_or(0)
                },
                !prepared.echo_query.is_empty()
                    && prepared.echo_query == row.normalized_text().trim(),
            )
        })
    }

    fn term_score_with_lengths(
        &self,
        field: LexicalFieldV1,
        term_frequency: usize,
        field_lengths: &BTreeMap<LexicalFieldV1, usize>,
        document_frequency: usize,
        stats: &LexicalStatsCacheV1,
    ) -> u64 {
        let total = stats.field_total(field);
        let average = total.div_ceil(self.document_count.max(1)).max(1);
        let document_length = field_lengths.get(&field).copied().unwrap_or(0).max(1);
        bm25_score_micros(
            self.document_count,
            document_frequency,
            term_frequency,
            document_length,
            average,
            field_weight_millis(field),
        )
    }

    fn term_score_with_df(
        &self,
        field: LexicalFieldV1,
        term_frequency: usize,
        row: &ArtifactRowV1,
        document_frequency: usize,
        stats: &LexicalStatsCacheV1,
    ) -> u64 {
        self.term_score_with_lengths(
            field,
            term_frequency,
            &row.field_lengths,
            document_frequency,
            stats,
        )
    }
}

/// Request-relevant per-row term frequencies decoded from one SQLite JSON
/// aggregate. Absent entries mean the artifact holds no posting for that key
/// and score as zero, exactly like the SQL probes they replace. One row
/// carries at most the request's term count, so a linear scan of the decoded
/// entries beats rebuilding two nested maps per visited row.
pub(super) struct LexicalTermFrequenciesV1(Vec<(LexicalFieldV1, String, usize)>);

struct LexicalStatsCacheV1 {
    field_totals: BTreeMap<LexicalFieldV1, usize>,
    document_frequencies: BTreeMap<LexicalFieldV1, BTreeMap<String, usize>>,
    postings: RequestTermPostingsV1,
}

fn lexical_terms(
    prepared: &PreparedLexicalQueryV1<'_>,
    fuzzy: &FuzzyExpansionsV1,
) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    for (_, normalized) in &prepared.whole_terms {
        terms.insert(normalized.clone());
    }
    for expansions in fuzzy.by_query.values() {
        terms.extend(expansions.iter().cloned());
    }
    for (_, normalized) in &prepared.subtokens {
        terms.insert(normalized.clone());
    }
    for proximity in &prepared.proximities {
        terms.extend(proximity.terms.iter().cloned());
    }
    terms
}

pub(super) fn term_frequency(
    frequencies: &LexicalTermFrequenciesV1,
    field: LexicalFieldV1,
    term: &str,
) -> usize {
    frequencies
        .0
        .iter()
        .find_map(|(entry_field, entry_term, frequency)| {
            (*entry_field == field && entry_term == term).then_some(*frequency)
        })
        .unwrap_or_default()
}

impl LexicalStatsCacheV1 {
    fn field_total(&self, field: LexicalFieldV1) -> usize {
        self.field_totals.get(&field).copied().unwrap_or_default()
    }

    fn document_frequency(&self, field: LexicalFieldV1, term: &str) -> usize {
        self.document_frequencies
            .get(&field)
            .and_then(|frequencies| frequencies.get(term))
            .copied()
            .unwrap_or_default()
    }

    /// Upper bound on the documents a whole-term source enumerates: the
    /// term's frequency summed over every non-subtoken field.
    fn whole_term_documents(&self, term: &str) -> usize {
        self.document_frequencies
            .iter()
            .filter(|(field, _)| **field != LexicalFieldV1::Subtoken)
            .map(|(_, frequencies)| frequencies.get(term).copied().unwrap_or_default())
            .fold(0usize, usize::saturating_add)
    }
}

struct LexicalDraftV1 {
    chunk_id: String,
    field_lengths: BTreeMap<LexicalFieldV1, usize>,
    frequencies: LexicalTermFrequenciesV1,
    phrase_tfs: Vec<(LexicalFieldV1, usize, usize)>,
    proximity_tfs: Vec<(LexicalFieldV1, usize, usize)>,
    trimmed_normalized_len: usize,
}

struct SelectedLexicalV1 {
    draft: LexicalDraftV1,
    document: u32,
    rank: u64,
}

type LexicalWinnerHeap = BinaryHeap<Keyed<(u8, Reverse<u64>, String, u32), SelectedLexicalV1>>;

fn draft_definition_match(draft: &LexicalDraftV1) -> bool {
    draft
        .frequencies
        .0
        .iter()
        .any(|(field, _, frequency)| *frequency > 0 && is_definition_field(*field))
        || draft
            .phrase_tfs
            .iter()
            .any(|(field, _, count)| *count > 0 && is_definition_field(*field))
        || draft
            .proximity_tfs
            .iter()
            .any(|(field, _, count)| *count > 0 && is_definition_field(*field))
}

fn lexical_admission_rank(
    query: &ArtifactQueryV1<'_>,
    document: u32,
    draft: &LexicalDraftV1,
    prepared: &PreparedLexicalQueryV1<'_>,
    fuzzy: &FuzzyExpansionsV1,
) -> Result<u8, RetrievalPortError> {
    if !draft_definition_match(draft) {
        return Ok(tracedecay_domain::RetrievalSourceRoleV1::ProductionOther.admission_rank());
    }
    let row = query.row(document)?;
    let definition_match = lexical_definition_match(&row, prepared, fuzzy);
    Ok(classify_source_role(&row.logical_path, definition_match).admission_rank())
}

// A signature's parameter and return types reference other symbols. Definition
// priority requires the query or its admitted spelling variant to name this
// row. A short alias still names the row when every term is a subtoken of the
// declared identifier (`cache grant` → `VerifiedCacheGrantSnapshotV1`).
fn lexical_definition_match(
    row: &ArtifactRowV1,
    prepared: &PreparedLexicalQueryV1<'_>,
    fuzzy: &FuzzyExpansionsV1,
) -> bool {
    let Some(name) = row.symbol_simple_name.as_deref() else {
        return false;
    };
    let normalized_name = normalize_lexical(name);
    if exact_definition_match(
        row.anchor.grain,
        Some(&normalized_name),
        prepared
            .whole_terms
            .iter()
            .chain(&prepared.phrases)
            .map(|(_, normalized)| normalized.as_str())
            .chain(fuzzy.by_query.values().flatten().map(String::as_str)),
    ) {
        return true;
    }
    let terms = prepared
        .whole_terms
        .iter()
        .map(|(_, normalized)| normalized.as_str())
        .collect::<Vec<_>>();
    if !(2..=3).contains(&terms.len()) {
        return false;
    }
    let name_tokens = split_subtokens(name);
    !name_tokens.is_empty()
        && terms
            .iter()
            .all(|term| name_tokens.iter().any(|token| token == term))
}

/// Heap entry ordered by `key` alone. Payload is excluded from equality so a
/// worst-first `BinaryHeap` ranks capped winners without comparing row
/// material.
struct Keyed<K, V> {
    key: K,
    value: V,
}

impl<K: PartialEq, V> PartialEq for Keyed<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<K: Eq, V> Eq for Keyed<K, V> {}

impl<K: Ord, V> PartialOrd for Keyed<K, V> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: Ord, V> Ord for Keyed<K, V> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

/// Retain at most `cap` best-ranked entries in a worst-first max-heap.
///
/// The heap key ranks worst entries greatest, so popping after an over-cap
/// push always evicts the current worst. Between calls the heap never holds
/// more than `cap` entries, which bounds retained materialization to the
/// lane candidate cap before any winner is hydrated.
pub(super) fn retain_bounded<K: Ord>(ranked: &mut BinaryHeap<K>, cap: usize, entry: K) {
    if cap == 0 {
        return;
    }
    ranked.push(entry);
    if ranked.len() > cap {
        ranked.pop();
    }
}

/// The lexical ranking key: the checked sum of the filter-admitted field
/// scores, or `None` when the typed field filters admit no scored field.
/// The same exclusion the lexical lane applies after the port returns.
fn admitted_score_micros(
    score: &LexicalRowScoreV1,
    filters: &[LexicalFieldFilterV1],
) -> Result<Option<u64>, RetrievalPortError> {
    let mut total: Option<u64> = None;
    for (field, micros) in &score.field_scores {
        if !field_admitted(filters, *field) {
            continue;
        }
        let sum = total.unwrap_or(0).checked_add(*micros).ok_or_else(|| {
            RetrievalPortError::Contract("lexical artifact ranking score overflowed".to_owned())
        })?;
        total = Some(sum);
    }
    Ok(total)
}

fn capped_batch<E>(
    examined: usize,
    eligible: u64,
    excluded: u64,
    truncated: u64,
    candidates: Vec<CompactCandidate>,
    evidence_by_occurrence: BTreeMap<SourceOccurrenceId, E>,
) -> RetrieverBatch<E> {
    RetrieverBatch {
        coverage: RetrieverCoverage {
            examined: examined as u64,
            eligible,
            excluded,
            capped: truncated,
            unknown: 0,
        },
        candidates,
        evidence_by_occurrence,
        continuation: None,
    }
}

fn exact_matches_artifact(row: &ArtifactRowV1, request: &ExactLaneRequest) -> Vec<ExactRowMatchV1> {
    exact_matches(
        ExactMatchRowViewV1 {
            anchor: &row.anchor,
            sanitized_text: row.sanitized_text.as_str(),
            logical_path: &row.logical_path,
            exact_terms: &row.exact_terms,
        },
        request,
    )
}

fn exact_matched_symbol_names<'a>(
    matches: &'a [ExactRowMatchV1],
    request: &'a ExactLaneRequest<'_>,
) -> impl Iterator<Item = &'a str> {
    matches.iter().filter_map(|matched| {
        if !matches!(
            matched.kind,
            Some(ExactTechnicalTermKindV1::WholeSymbol)
                | Some(ExactTechnicalTermKindV1::QualifiedName)
        ) {
            return None;
        }
        request
            .literals
            .get(matched.literal)
            .and_then(|literal| std::str::from_utf8(&literal.canonical_bytes).ok())
    })
}

/// In-fuzzy terms plus a character-length index. Buckets keep load order so
/// merging a length window and sorting indices reproduces the order a full
/// scan would have visited those terms.
pub(super) struct FuzzyVocabularyV1 {
    terms: Vec<String>,
    by_char_len: Vec<Vec<u32>>,
}

impl FuzzyVocabularyV1 {
    pub(super) fn from_terms(terms: Vec<String>) -> Result<Self, RetrievalPortError> {
        let mut by_char_len = Vec::new();
        for (index, term) in terms.iter().enumerate() {
            let len = term.chars().count();
            if by_char_len.len() <= len {
                by_char_len.resize(len + 1, Vec::new());
            }
            by_char_len[len].push(u32::try_from(index).map_err(contract_error)?);
        }
        Ok(Self { terms, by_char_len })
    }

    /// Terms whose character length is in `min..=max`, in load order.
    pub(super) fn indices_within(&self, min: usize, max: usize) -> Vec<u32> {
        if min > max || min >= self.by_char_len.len() {
            return Vec::new();
        }
        let end = max.min(self.by_char_len.len() - 1);
        let mut indices = Vec::new();
        for bucket in &self.by_char_len[min..=end] {
            indices.extend_from_slice(bucket);
        }
        indices.sort_unstable();
        indices
    }
}

/// Reusable buffers for the vocabulary edit-distance sweep. One expansion
/// pass compares the query against the length window that can still match;
/// per-comparison `Vec` allocations dominated that sweep.
#[derive(Default)]
struct EditDistanceScratchV1 {
    query_chars: Vec<char>,
    term_chars: Vec<char>,
    previous: Vec<usize>,
    current: Vec<usize>,
}

impl EditDistanceScratchV1 {
    fn prepare_query(&mut self, query: &str) {
        self.query_chars.clear();
        self.query_chars.extend(query.chars());
    }

    /// Levenshtein distance of the prepared query to `right` when it is at
    /// most `limit`, without per-call allocation. Byte-length prechecks prune
    /// most of the vocabulary before any character walk: one UTF-8 character
    /// is one to four bytes, so a term shorter than `chars(query) - limit`
    /// bytes or longer than `(chars(query) + limit) * 4` bytes cannot be
    /// within `limit` edits.
    fn bounded_edit_distance(&mut self, right: &str, limit: usize) -> Option<usize> {
        let query_len = self.query_chars.len();
        if right.len() < query_len.saturating_sub(limit)
            || right.len() > query_len.saturating_add(limit).saturating_mul(4)
        {
            return None;
        }
        self.term_chars.clear();
        self.term_chars.extend(right.chars());
        if query_len.abs_diff(self.term_chars.len()) > limit {
            return None;
        }
        let width = self.term_chars.len() + 1;
        self.previous.clear();
        self.previous.extend(0..width);
        self.current.clear();
        self.current.resize(width, 0);
        for (left_index, left_character) in self.query_chars.iter().enumerate() {
            self.current[0] = left_index + 1;
            let mut row_minimum = self.current[0];
            for (right_index, right_character) in self.term_chars.iter().enumerate() {
                let value = (self.previous[right_index + 1] + 1)
                    .min(self.current[right_index] + 1)
                    .min(
                        self.previous[right_index] + usize::from(left_character != right_character),
                    );
                self.current[right_index + 1] = value;
                row_minimum = row_minimum.min(value);
            }
            // The minimum of a Levenshtein DP row never decreases in later
            // rows, so a row already past the limit can never come back.
            if row_minimum > limit {
                return None;
            }
            std::mem::swap(&mut self.previous, &mut self.current);
        }
        (self.previous[width - 1] <= limit).then_some(self.previous[width - 1])
    }
}

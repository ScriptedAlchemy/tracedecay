//! Typed generation admission and canonical native result translation.
//!
//! Storage adapters implement [`NativeRecordReadPortV1`] over one immutable
//! generation. The query kernel validates that generation once, preserves
//! every typed lane outcome, and emits transport-independent records.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, CompactCandidate, ExactTechnicalTermKindV1,
    FileOccurrenceId, FixedPointScore, RetrievalBudgetUsage, RetrievalFailure, RetrieverBatch,
    RetrieverCoverage, RetrieverOutcome, SourceFreshness, SourceSpan, SymbolOccurrenceId,
};

use super::exact::ExactLaneEvidence;
use super::graph::GraphLaneEvidence;
use super::lexical::LexicalLaneEvidence;
use super::ports::CodeCandidateBindingV1;
use super::semantic::CodeSemanticEvidenceV1;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum QueryExecutionContractErrorV1 {
    #[error("the native record authority is bound to another generation")]
    GenerationMismatch,
    #[error("the admitted generation identity is invalid")]
    InvalidGeneration,
    #[error("lane evidence violates the canonical retrieval contract")]
    InvalidLaneEvidence,
    #[error("the generation-bound native record is unavailable")]
    RecordUnavailable,
    #[error("the native record identity does not match its lane evidence")]
    RecordIdentityMismatch,
}

/// Query-native occurrence shape. Application and transport records adapt
/// from this value; they do not reconstruct source identity themselves.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeCodeOccurrenceV1 {
    pub file: FileOccurrenceId,
    pub symbol: Option<SymbolOccurrenceId>,
    pub chunk: Option<CodeSearchChunkId>,
    pub path: String,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeExactRecordV1 {
    pub occurrence: NativeCodeOccurrenceV1,
    pub matched_kind: ExactTechnicalTermKindV1,
    pub matched_literal: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeLexicalRecordV1 {
    pub occurrence: NativeCodeOccurrenceV1,
    pub score_micros: u64,
    pub matched_phrases: Vec<String>,
    pub matched_terms: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeSymbolRecordV1 {
    pub occurrence: SymbolOccurrenceId,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub path: String,
    pub span: SourceSpan,
    pub start_line_zero_based: u32,
    pub end_line_zero_based: u32,
    pub line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub is_async: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeGraphRecordV1 {
    pub symbol: NativeSymbolRecordV1,
    pub edge_kind: Option<tracedecay_domain::RelationEdgeKindV1>,
    pub depth: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeSemanticRecordV1 {
    pub occurrence: NativeCodeOccurrenceV1,
    pub distance_micros: i64,
    pub score: FixedPointScore,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeLanePageV1<T> {
    pub generation: CodeGenerationId,
    pub items: Vec<T>,
    pub total_eligible: u64,
    pub coverage: RetrieverCoverage,
}

/// Truthful query-layer lane outcome. It preserves denial, stale source,
/// cancellation and budget states instead of collapsing them into an empty
/// page or transport-specific omission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeLaneOutcomeV1<T> {
    Complete(NativeLanePageV1<T>),
    Partial {
        page: NativeLanePageV1<T>,
        reason: RetrievalFailure,
    },
    Unavailable(RetrievalFailure),
    Denied,
    Stale(SourceFreshness),
    BudgetExceeded(RetrievalBudgetUsage),
    TimedOut(RetrievalBudgetUsage),
    Cancelled,
}

/// Immutable source authority required for canonical result translation.
///
/// Implementations may borrow code-index projections, but this crate depends
/// only on these native query values.
pub trait NativeRecordReadPortV1 {
    fn generation(&self) -> &CodeGenerationId;

    fn occurrence(
        &self,
        binding: &CodeCandidateBindingV1,
    ) -> Result<NativeCodeOccurrenceV1, QueryExecutionContractErrorV1>;

    fn occurrence_by_chunk(
        &self,
        chunk: &CodeSearchChunkId,
    ) -> Result<NativeCodeOccurrenceV1, QueryExecutionContractErrorV1>;

    fn symbol(
        &self,
        symbol: &SymbolOccurrenceId,
        file: &FileOccurrenceId,
    ) -> Result<NativeSymbolRecordV1, QueryExecutionContractErrorV1>;
}

/// One generation checked against its native record authority.
pub struct AdmittedGenerationContextV1<'a, P: ?Sized> {
    generation: CodeGenerationId,
    records: &'a P,
}

impl<'a, P> AdmittedGenerationContextV1<'a, P>
where
    P: NativeRecordReadPortV1 + ?Sized,
{
    pub fn admit(
        generation: CodeGenerationId,
        records: &'a P,
    ) -> Result<Self, QueryExecutionContractErrorV1> {
        generation
            .validate()
            .map_err(|_| QueryExecutionContractErrorV1::InvalidGeneration)?;
        if records.generation() != &generation {
            return Err(QueryExecutionContractErrorV1::GenerationMismatch);
        }
        Ok(Self {
            generation,
            records,
        })
    }

    pub fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    // PERF: the four lane translators below each issue one
    // `NativeRecordReadPortV1` lookup per candidate (`occurrence` /
    // `occurrence_by_chunk` / `symbol`). That per-row shape is deliberate:
    // every lookup is interleaved with per-record validation
    // (`validate_occurrence`, `RecordIdentityMismatch`, `path_admitted`), so
    // batching at this loop would not be a low-risk change.
    //
    // Keeping the per-row shape is only sound while each lookup is cheap. The
    // production impl on `LatestCompleteCodeIndexV1`
    // (code_index_scheduler/queries.rs) used to resolve every lookup
    // with a linear `.iter().find(..)` over the in-memory `files` / `chunks` /
    // `symbols` vectors, which made each lane O(candidates x records); it now
    // answers them from `HashMap` indices memoized per sealed generation, so
    // each lane is O(candidates). Any new port implementor must offer the same
    // amortized-O(1) lookups rather than rescanning per candidate.
    pub fn exact(
        &self,
        outcome: RetrieverOutcome<RetrieverBatch<ExactLaneEvidence>>,
        matched_literal: &str,
        expected_kind: Option<ExactTechnicalTermKindV1>,
        path_admitted: impl Fn(&str) -> bool,
    ) -> Result<NativeLaneOutcomeV1<NativeExactRecordV1>, QueryExecutionContractErrorV1> {
        self.translate(outcome, |batch| {
            let mut items = Vec::new();
            for candidate in &batch.candidates {
                let evidence = lane_evidence(batch, candidate)?;
                self.validate_binding(&evidence.binding)?;
                if !evidence
                    .matched_literals
                    .iter()
                    .any(|literal| literal.original_bytes == matched_literal.as_bytes())
                {
                    continue;
                }
                let Some(matched_kind) = evidence
                    .binding
                    .matched_term_kinds
                    .iter()
                    .copied()
                    .find(|kind| expected_kind.is_none_or(|expected| expected == *kind))
                else {
                    continue;
                };
                let occurrence = self.records.occurrence(&evidence.binding)?;
                self.validate_occurrence(&evidence.binding, &occurrence)?;
                if path_admitted(&occurrence.path) {
                    items.push(NativeExactRecordV1 {
                        occurrence,
                        matched_kind,
                        matched_literal: matched_literal.to_owned(),
                    });
                }
            }
            Ok(items)
        })
    }

    pub fn lexical(
        &self,
        outcome: RetrieverOutcome<RetrieverBatch<LexicalLaneEvidence>>,
        path_admitted: impl Fn(&str) -> bool,
    ) -> Result<NativeLaneOutcomeV1<NativeLexicalRecordV1>, QueryExecutionContractErrorV1> {
        self.translate(outcome, |batch| {
            let mut items = Vec::new();
            for candidate in &batch.candidates {
                let evidence = lane_evidence(batch, candidate)?;
                self.validate_binding(&evidence.binding)?;
                let occurrence = self.records.occurrence(&evidence.binding)?;
                self.validate_occurrence(&evidence.binding, &occurrence)?;
                if path_admitted(&occurrence.path) {
                    items.push(NativeLexicalRecordV1 {
                        occurrence,
                        score_micros: candidate.raw_score.0,
                        matched_phrases: evidence.matched_phrases.clone(),
                        matched_terms: evidence
                            .matched_whole_terms
                            .iter()
                            .chain(&evidence.matched_subtokens)
                            .cloned()
                            .collect(),
                    });
                }
            }
            Ok(items)
        })
    }

    pub fn graph(
        &self,
        outcome: RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>,
        path_admitted: impl Fn(&str) -> bool,
    ) -> Result<NativeLaneOutcomeV1<NativeGraphRecordV1>, QueryExecutionContractErrorV1> {
        self.translate(outcome, |batch| {
            let mut items = Vec::new();
            for candidate in &batch.candidates {
                let evidence = lane_evidence(batch, candidate)?;
                self.validate_binding(&evidence.binding)?;
                let Some(symbol) = evidence.binding.occurrence.symbol.as_ref() else {
                    continue;
                };
                let record = self
                    .records
                    .symbol(symbol, &evidence.binding.occurrence.file)?;
                if &record.occurrence != symbol {
                    return Err(QueryExecutionContractErrorV1::RecordIdentityMismatch);
                }
                if path_admitted(&record.path) {
                    items.push(NativeGraphRecordV1 {
                        symbol: record,
                        edge_kind: evidence.path.last().map(|edge| edge.edge_kind),
                        depth: evidence.path.len() as u32,
                    });
                }
            }
            Ok(items)
        })
    }

    pub fn semantic(
        &self,
        outcome: RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>,
        path_admitted: impl Fn(&str) -> bool,
    ) -> Result<NativeLaneOutcomeV1<NativeSemanticRecordV1>, QueryExecutionContractErrorV1> {
        self.translate(outcome, |batch| {
            let mut items = Vec::new();
            for candidate in &batch.candidates {
                let evidence = lane_evidence(batch, candidate)?;
                let occurrence = self.records.occurrence_by_chunk(&evidence.chunk_id)?;
                if occurrence.chunk.as_ref() != Some(&evidence.chunk_id) {
                    return Err(QueryExecutionContractErrorV1::RecordIdentityMismatch);
                }
                if path_admitted(&occurrence.path) {
                    items.push(NativeSemanticRecordV1 {
                        occurrence,
                        distance_micros: evidence.distance.micros(),
                        score: candidate.raw_score,
                    });
                }
            }
            Ok(items)
        })
    }

    #[hotpath::measure(label = "query.stream.translate")]
    fn translate<E, T>(
        &self,
        outcome: RetrieverOutcome<RetrieverBatch<E>>,
        translate_batch: impl Fn(&RetrieverBatch<E>) -> Result<Vec<T>, QueryExecutionContractErrorV1>,
    ) -> Result<NativeLaneOutcomeV1<T>, QueryExecutionContractErrorV1> {
        let translated = match outcome {
            RetrieverOutcome::Complete(batch) => {
                batch
                    .validate()
                    .map_err(|_| QueryExecutionContractErrorV1::InvalidLaneEvidence)?;
                Ok(NativeLaneOutcomeV1::Complete(
                    self.page(&batch, translate_batch(&batch)?),
                ))
            }
            RetrieverOutcome::Partial { value, reason } => {
                value
                    .validate()
                    .map_err(|_| QueryExecutionContractErrorV1::InvalidLaneEvidence)?;
                Ok(NativeLaneOutcomeV1::Partial {
                    page: self.page(&value, translate_batch(&value)?),
                    reason,
                })
            }
            RetrieverOutcome::Unavailable(reason) => Ok(NativeLaneOutcomeV1::Unavailable(reason)),
            RetrieverOutcome::Denied => Ok(NativeLaneOutcomeV1::Denied),
            RetrieverOutcome::Stale(freshness) => Ok(NativeLaneOutcomeV1::Stale(freshness)),
            RetrieverOutcome::BudgetExceeded(usage) => {
                Ok(NativeLaneOutcomeV1::BudgetExceeded(usage))
            }
            RetrieverOutcome::TimedOut(usage) => Ok(NativeLaneOutcomeV1::TimedOut(usage)),
            RetrieverOutcome::Cancelled => Ok(NativeLaneOutcomeV1::Cancelled),
        };
        if let Ok(ref outcome) = translated {
            match outcome {
                NativeLaneOutcomeV1::Complete(page) | NativeLaneOutcomeV1::Partial { page, .. } => {
                    hotpath::gauge!("query.stream.results").set(page.items.len());
                    hotpath::gauge!("query.stream.rows").set(page.coverage.examined);
                }
                NativeLaneOutcomeV1::Cancelled => {
                    hotpath::gauge!("query.cancel.count").inc(1u32);
                }
                NativeLaneOutcomeV1::Stale(_) => {
                    crate::hotpath_metrics::Residency::Rebuilding.record("query.stream.residency");
                }
                _ => {}
            }
        }
        translated
    }

    fn validate_binding(
        &self,
        binding: &CodeCandidateBindingV1,
    ) -> Result<(), QueryExecutionContractErrorV1> {
        if binding.occurrence.generation != self.generation {
            return Err(QueryExecutionContractErrorV1::GenerationMismatch);
        }
        Ok(())
    }

    fn validate_occurrence(
        &self,
        binding: &CodeCandidateBindingV1,
        occurrence: &NativeCodeOccurrenceV1,
    ) -> Result<(), QueryExecutionContractErrorV1> {
        if occurrence.file != binding.occurrence.file
            || occurrence.symbol != binding.occurrence.symbol
            || occurrence.chunk != binding.occurrence.chunk
        {
            return Err(QueryExecutionContractErrorV1::RecordIdentityMismatch);
        }
        Ok(())
    }

    fn page<E, T>(&self, batch: &RetrieverBatch<E>, items: Vec<T>) -> NativeLanePageV1<T> {
        NativeLanePageV1 {
            generation: self.generation.clone(),
            items,
            total_eligible: batch.coverage.eligible,
            coverage: batch.coverage,
        }
    }
}

/// The evidence a batch emitted for one candidate.
///
/// A batch that returns a candidate without its evidence is not translatable
/// at all, so every lane translation resolves it the same way.
fn lane_evidence<'batch, E>(
    batch: &'batch RetrieverBatch<E>,
    candidate: &CompactCandidate,
) -> Result<&'batch E, QueryExecutionContractErrorV1> {
    batch
        .evidence_by_occurrence
        .get(&candidate.source_occurrence_id)
        .ok_or(QueryExecutionContractErrorV1::InvalidLaneEvidence)
}

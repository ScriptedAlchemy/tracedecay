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
    // production port `LatestCompleteNativeRecordReadPortV1`
    // (src/daemon/code_index_scheduler/queries.rs) used to resolve every lookup
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

    fn translate<E, T>(
        &self,
        outcome: RetrieverOutcome<RetrieverBatch<E>>,
        translate_batch: impl Fn(&RetrieverBatch<E>) -> Result<Vec<T>, QueryExecutionContractErrorV1>,
    ) -> Result<NativeLaneOutcomeV1<T>, QueryExecutionContractErrorV1> {
        match outcome {
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
            RetrieverOutcome::Cancelled => Ok(NativeLaneOutcomeV1::Cancelled),
        }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt;

    use tracedecay_domain::{
        CompactCandidate, EdgeAuthorityV1, EvidenceRole, ExactAdmissionProof,
        ExactAdmissionRuleRevision, ExactFieldV1, FreshnessCompatibilityV1, RelationEdgeKindV1,
        RetrieverKind, SourceOccurrenceId, UtcMicros,
    };

    use super::*;
    use crate::retrieval::exact::{ExactLaneEvidence, ExactLiteralV1};
    use crate::retrieval::graph::GraphPathSegmentV1;
    use crate::retrieval::lexical::{LexicalFieldV1, LexicalLaneEvidence};
    use crate::retrieval::ports::CodeOccurrenceRefV1;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest<T>(byte: char) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: fmt::Debug,
    {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn freshness() -> SourceFreshness {
        SourceFreshness {
            source_namespace: id("namespace.query-execution"),
            source_instance: id("instance.query-execution"),
            source_watermark: Some(1),
            projection_watermark: Some(1),
            observed_at: UtcMicros(1),
            source_generation: Some(1),
            generation_lag: Some(0),
            compatibility: FreshnessCompatibilityV1::Current,
            policy_revision: id("policy.query-execution.v1"),
        }
    }

    fn candidate(kind: RetrieverKind) -> CompactCandidate {
        CompactCandidate {
            anchor_id: id("anchor.query-execution"),
            logical_evidence_id: id("logical.query-execution"),
            source_occurrence_id: id("source.query-execution"),
            file_occurrence_id: Some(id("file.query-execution")),
            source_namespace: id("namespace.query-execution"),
            repository_id: None,
            session_or_thread_id: None,
            logical_copy_cluster_id: None,
            logical_copy_evidence_anchor: None,
            evidence_role: EvidenceRole::Primary,
            retriever: kind,
            retriever_revision: id("retriever.query-execution.v1"),
            score_domain: id("score.query-execution.v1"),
            raw_score: FixedPointScore(42),
            ordinal_rank: 0,
            exact_admission_proof: None,
            retriever_evidence_anchor: id("evidence.query-execution"),
            freshness: freshness(),
        }
    }

    fn binding() -> CodeCandidateBindingV1 {
        CodeCandidateBindingV1 {
            candidate_anchor: id("anchor.query-execution"),
            occurrence: CodeOccurrenceRefV1 {
                generation: id("generation.query-execution"),
                file: id("file.query-execution"),
                symbol: Some(id("symbol.query-execution")),
                chunk: Some(id("chunk.query-execution")),
            },
            language_descriptor_revision: id("language.rust.v1"),
            matched_term_kinds: vec![ExactTechnicalTermKindV1::WholeSymbol],
            source_occurrence: id("source.query-execution"),
        }
    }

    fn batch<E>(candidate: CompactCandidate, evidence: E) -> RetrieverBatch<E> {
        RetrieverBatch {
            candidates: vec![candidate],
            evidence_by_occurrence: BTreeMap::from([(
                id::<SourceOccurrenceId>("source.query-execution"),
                evidence,
            )]),
            coverage: RetrieverCoverage {
                examined: 1,
                eligible: 1,
                excluded: 0,
                capped: 0,
                unknown: 0,
            },
            continuation: None,
        }
    }

    struct FixtureRecords {
        generation: CodeGenerationId,
    }

    impl FixtureRecords {
        fn occurrence() -> NativeCodeOccurrenceV1 {
            NativeCodeOccurrenceV1 {
                file: id("file.query-execution"),
                symbol: Some(id("symbol.query-execution")),
                chunk: Some(id("chunk.query-execution")),
                path: "src/lib.rs".to_owned(),
                span: SourceSpan {
                    start_byte: 10,
                    end_byte: 20,
                },
            }
        }
    }

    impl NativeRecordReadPortV1 for FixtureRecords {
        fn generation(&self) -> &CodeGenerationId {
            &self.generation
        }

        fn occurrence(
            &self,
            _: &CodeCandidateBindingV1,
        ) -> Result<NativeCodeOccurrenceV1, QueryExecutionContractErrorV1> {
            Ok(Self::occurrence())
        }

        fn occurrence_by_chunk(
            &self,
            _: &CodeSearchChunkId,
        ) -> Result<NativeCodeOccurrenceV1, QueryExecutionContractErrorV1> {
            Ok(Self::occurrence())
        }

        fn symbol(
            &self,
            symbol: &SymbolOccurrenceId,
            _: &FileOccurrenceId,
        ) -> Result<NativeSymbolRecordV1, QueryExecutionContractErrorV1> {
            Ok(NativeSymbolRecordV1 {
                occurrence: symbol.clone(),
                name: "callee".to_owned(),
                qualified_name: "fixture::callee".to_owned(),
                kind: "function".to_owned(),
                path: "src/lib.rs".to_owned(),
                span: SourceSpan {
                    start_byte: 10,
                    end_byte: 20,
                },
                signature: Some("pub fn callee()".to_owned()),
                is_async: false,
            })
        }
    }

    fn context() -> AdmittedGenerationContextV1<'static, FixtureRecords> {
        let records = Box::leak(Box::new(FixtureRecords {
            generation: id("generation.query-execution"),
        }));
        AdmittedGenerationContextV1::admit(records.generation.clone(), records)
            .expect("matching generation")
    }

    #[test]
    fn exact_phrase_and_graph_translation_match_daemon_shapes() {
        let context = context();
        let proof = ExactAdmissionProof {
            rule_revision: id::<ExactAdmissionRuleRevision>("exact-rules.v1"),
            field: ExactFieldV1::Identifier,
            original_bytes: b"callee".to_vec(),
            canonical_bytes: b"callee".to_vec(),
            normalization_steps: Vec::new(),
            scope_digest: digest('a'),
            authorization_revision: id("authorization.v1"),
            snapshot_digest: digest('b'),
        };
        let mut exact_candidate = candidate(RetrieverKind::ExactLiteral);
        exact_candidate.exact_admission_proof = Some(proof.clone());
        let exact = context
            .exact(
                RetrieverOutcome::Complete(batch(
                    exact_candidate,
                    ExactLaneEvidence {
                        binding: binding(),
                        matched_literals: vec![ExactLiteralV1 {
                            field: ExactFieldV1::Identifier,
                            original_bytes: b"callee".to_vec(),
                            canonical_bytes: b"callee".to_vec(),
                        }],
                        admission_proof: proof,
                    },
                )),
                "callee",
                Some(ExactTechnicalTermKindV1::WholeSymbol),
                |_| true,
            )
            .expect("exact translation");
        let NativeLaneOutcomeV1::Complete(exact) = exact else {
            panic!("exact lane completes");
        };
        assert_eq!(exact.items[0].matched_literal, "callee");
        assert_eq!(exact.items[0].occurrence.path, "src/lib.rs");

        let lexical = context
            .lexical(
                RetrieverOutcome::Complete(batch(
                    candidate(RetrieverKind::Lexical),
                    LexicalLaneEvidence {
                        binding: binding(),
                        field_scores_micros: vec![(LexicalFieldV1::SymbolName, 42)],
                        matched_whole_terms: vec!["callee".to_owned()],
                        matched_subtokens: vec!["call".to_owned()],
                        matched_phrases: vec!["callee".to_owned()],
                        typo_recovery_applied: false,
                        echo_penalty_applied: false,
                    },
                )),
                |_| true,
            )
            .expect("lexical translation");
        let NativeLaneOutcomeV1::Complete(lexical) = lexical else {
            panic!("lexical lane completes");
        };
        assert_eq!(lexical.items[0].score_micros, 42);
        assert_eq!(lexical.items[0].matched_terms, ["callee", "call"]);

        let graph = context
            .graph(
                RetrieverOutcome::Complete(batch(
                    candidate(RetrieverKind::Graph),
                    GraphLaneEvidence {
                        binding: binding(),
                        path: vec![GraphPathSegmentV1 {
                            from: id("symbol.caller"),
                            to: id("symbol.query-execution"),
                            edge_kind: RelationEdgeKindV1::Calls,
                            authority: EdgeAuthorityV1::SyntaxExact,
                            evidence_span: SourceSpan {
                                start_byte: 1,
                                end_byte: 2,
                            },
                        }],
                        weakest_authority: EdgeAuthorityV1::SyntaxExact,
                    },
                )),
                |_| true,
            )
            .expect("graph translation");
        let NativeLaneOutcomeV1::Complete(graph) = graph else {
            panic!("graph lane completes");
        };
        assert_eq!(graph.items[0].edge_kind, Some(RelationEdgeKindV1::Calls));
        assert_eq!(graph.items[0].depth, 1);
    }

    #[test]
    fn semantic_and_failure_lane_decisions_remain_typed() {
        let context = context();
        assert_eq!(
            context
                .semantic(RetrieverOutcome::Cancelled, |_| true)
                .expect("cancelled decision"),
            NativeLaneOutcomeV1::Cancelled
        );
        assert!(matches!(
            context
                .exact(RetrieverOutcome::Stale(freshness()), "callee", None, |_| {
                    true
                })
                .expect("stale decision"),
            NativeLaneOutcomeV1::Stale(_)
        ));
        assert_eq!(
            context
                .lexical(RetrieverOutcome::Denied, |_| true)
                .expect("denied decision"),
            NativeLaneOutcomeV1::Denied
        );
    }
}

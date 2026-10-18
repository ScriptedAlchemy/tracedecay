//! Production search-quality kernel: candidate types, packaged workload
//! inputs, and direct-report scoring for the exact/lexical/graph lanes.
//!
//! The evaluator that publishes a fixture corpus and compares live candidates
//! lives in `tracedecay-search-eval` and depends on this module.
//!
//! # Evidence, not activation authority
//!
//! Retiring the dense FastEmbed lane deleted `native_qualification` and the
//! paired held-out Student-t practical-effect gate with it, because the only
//! thing that gate governed was activating a lane that no longer exists. The
//! exact/lexical/graph lanes are unconditionally part of production retrieval:
//! there is nothing left for a measurement to switch on.
//!
//! Everything in this module is therefore evidence-only. A `Pass` report says
//! the checked-in labels were met on the packaged corpus; it does not qualify,
//! activate, promote, or accept a retrieval profile, and no caller may treat it
//! as doing so. Validation of the packaged workload, including natural-language
//! need provenance, attests that the measurement inputs are fit and sourced,
//! never that retrieval is qualified.
//!
//! Re-introducing a qualification gate is a schema change, not a comment
//! change: workload schema 1 carries no `methodology_version`, no practical-
//! effect bound, no `policy_freeze`, and no `decision_policy`, so it cannot
//! express a held-out methodology or a single-variant policy slice. See
//! `docs/development/search-quality-direct-evaluation.md`.

pub mod candidate_output;
pub mod evaluate;
pub mod packaged;
pub mod report;

pub use candidate_output::{
    CandidateOutputError, CandidateWorkloadV1, CorpusDocumentV1, EvaluationConcurrencyContractV1,
    EvaluationExecutionContractV1, GenerateCandidateOutputsResultV1, NeedProvenanceKindV1,
    NeedProvenanceV1, ProductionCandidateOutputV1, ResourceMeasurementPendingReasonV1,
    ResourceMeasurementStatusV1, WorkloadQueryV1, compute_corpus_digest,
    compute_profile_material_digest, compute_workload_digest, load_candidate_workload,
    validate_workload_for_tuning,
};
pub use evaluate::{
    DirectEvaluationStatusV1, QUERY_BASELINE_PROFILE, SearchEvalError, evaluate_generated_outputs,
};
pub use report::{
    DirectEvaluationReportV1, DirectProfileEvaluationV1, DirectQualityMetricsV1,
    DirectQueryEvaluationV1, DirectQueryQualityV1, DirectRatioMetricV1, DirectStratumQualityV1,
    DirectWorstStratumV1,
};

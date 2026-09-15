//! Production search-quality kernel: candidate types, packaged workload
//! inputs, and direct-report scoring for the exact/lexical/graph lanes.
//!
//! The evaluator that publishes a fixture corpus and compares live candidates
//! lives in `tracedecay-search-eval` and depends on this module.

pub mod candidate_output;
pub mod evaluate;
pub mod packaged;
pub mod report;

pub use candidate_output::{
    CandidateOutputError, CandidateWorkloadV1, CorpusDocumentV1, EvaluationConcurrencyContractV1,
    EvaluationExecutionContractV1, GenerateCandidateOutputsResultV1, NeedProvenanceKindV1,
    NeedProvenanceV1, ProductionCandidateOutputV1, ResourceMeasurementStatusV1, WorkloadQueryV1,
    compute_corpus_digest, compute_profile_material_digest, compute_workload_digest,
    load_candidate_workload, validate_workload_for_tuning,
};
pub use evaluate::{
    DirectEvaluationStatusV1, QUERY_BASELINE_PROFILE, SearchEvalError, evaluate_generated_outputs,
};
pub use report::{
    DirectEvaluationReportV1, DirectProfileEvaluationV1, DirectQualityMetricsV1,
    DirectQueryEvaluationV1, DirectQueryQualityV1, DirectRatioMetricV1, DirectStratumQualityV1,
    DirectWorstStratumV1,
};

//! Production search-quality kernel: candidate types, packaged-profile inputs,
//! native qualification, and direct-report scoring.
//!
//! The evaluator that publishes a fixture corpus and compares live candidates
//! lives in `tracedecay-search-eval` and depends on this module.

pub mod candidate_output;
pub mod evaluate;
pub mod native_qualification;
pub mod packaged;
pub mod report;
pub mod semantic_native;

pub use candidate_output::{
    CandidateOutputError, CandidateWorkloadV1, CorpusDocumentV1, DirectEvaluatedProfileMaterialV1,
    EvaluationConcurrencyContractV1, EvaluationExecutionContractV1,
    GenerateCandidateOutputsResultV1, OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
    ProductionCandidateNativeExecutionAuthorityV1, ProductionCandidateNativeGenerationResourcesV1,
    ProductionCandidateNativeQueryContextV1, ProductionCandidateNativeQueryInputsV1,
    ProductionCandidateNativeResourceContextV1, ProductionCandidateOutputV1,
    ProductionCandidateSemanticProjectionSourcesV1, ResourceMeasurementStatusV1, WorkloadQueryV1,
    compute_corpus_digest, compute_profile_material_digest, compute_workload_digest,
    direct_evaluated_profile_material, load_candidate_workload,
    load_direct_evaluated_profile_material, validate_workload_for_tuning,
};
pub use evaluate::{
    DirectActivationEvaluationV1, DirectEvaluationStatusV1, QUERY_BASELINE_PROFILE, RERANK_PROFILE,
    SEMANTIC_PROFILE, SearchEvalError, activation_profile_chain, evaluate_generated_outputs,
    evaluate_generated_outputs_against_corpus, load_authoritative_default_workload_metadata,
    load_default_evaluated_profile_material, nearest_rank,
};
pub use native_qualification::{
    NativeQualificationEvaluatorKeyV1, NativeQualificationExecutionResourceKeyV1,
    NativeQualificationExpectationsV1, NativeQualificationKeyV1, NativeQualificationModelKeyV1,
    NativeQualificationPlatformV1, NativeQualificationRuntimeKeyV1,
    NativeQualificationVectorGenerationRetentionV1, PackagedNativeActivationCandidateV1,
    PackagedNativeQualificationErrorV1, PackagedNativeQualificationV1,
    PortableNativeQualificationEvidenceV1, encode_daemon_native_qualification_blob,
    encode_packaged_native_qualification, load_packaged_native_qualification_from_bytes,
    packaged_native_qualification_bytes, qualified_default_activation_candidate,
    validate_packaged_native_activation_report, write_daemon_native_qualification,
    write_packaged_native_qualification,
};
pub use report::{
    DirectEvaluationReportV1, DirectProfileEvaluationV1, DirectQualityMetricsV1,
    DirectQueryEvaluationV1, DirectQueryQualityV1, DirectRatioMetricV1, DirectStratumQualityV1,
    DirectWorstStratumV1,
};

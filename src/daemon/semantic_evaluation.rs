use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracedecay_application::ResolvedScope;
use tracedecay_domain::CodeGenerationId;

use crate::application::semantic_runtime::{
    SemanticActivationCoordinationErrorV1, SemanticEvaluationAuthorityPublicationV1,
    SemanticEvaluationProfileCandidateV1, SemanticEvaluationPublicationSnapshotPortV1,
    SemanticEvaluationPublicationSnapshotV1, SemanticRuntimeFuture,
};
use crate::config::retrieval::RetrievalRuntimeCompatibilityV1;
use crate::search_eval::semantic_native::{
    SemanticNativePendingReasonV1, SemanticNativeResourceProvenanceV1,
    SemanticNativeResourceSampleV1, SemanticNativeStageResultV1,
};
use crate::search_eval::{
    CandidateOutputError, ProductionCandidateNativeExecutionAuthorityV1,
    ProductionCandidateNativeQueryContextV1, ProductionCandidateNativeQueryInputsV1,
    ProductionCandidateNativeResourceContextV1, evaluate_default_activation_candidate,
};

use super::code_index_scheduler::CodeIndexSchedulerRegistryV1;

static RESOURCE_MEASUREMENT_LOCK_V1: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub(super) struct DaemonSemanticEvaluationSnapshotAuthorityV1 {
    project_root: PathBuf,
    scope: ResolvedScope,
    scheduler: CodeIndexSchedulerRegistryV1,
    candidate: SemanticEvaluationProfileCandidateV1,
    prepared_native: Arc<
        Mutex<
            BTreeMap<
                CodeGenerationId,
                Arc<crate::application::semantic_runtime::PreparedSemanticEvaluationGenerationV1>,
            >,
        >,
    >,
}

impl DaemonSemanticEvaluationSnapshotAuthorityV1 {
    pub(super) fn new(
        project_root: PathBuf,
        scope: ResolvedScope,
        scheduler: CodeIndexSchedulerRegistryV1,
        candidate: SemanticEvaluationProfileCandidateV1,
    ) -> Self {
        Self {
            project_root,
            scope,
            scheduler,
            candidate,
            prepared_native: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl ProductionCandidateNativeExecutionAuthorityV1 for DaemonSemanticEvaluationSnapshotAuthorityV1 {
    fn with_query_inputs(
        &self,
        context: ProductionCandidateNativeQueryContextV1<'_>,
        evaluate: &mut dyn for<'inputs> FnMut(
            ProductionCandidateNativeQueryInputsV1<'inputs>,
        ) -> Result<(), CandidateOutputError>,
    ) -> Result<(), CandidateOutputError> {
        if context.profile.semantic_weight_ppm == 0 {
            return evaluate(ProductionCandidateNativeQueryInputsV1 {
                semantic: None,
                rerank: None,
            });
        }
        let required = self
            .candidate
            .compatibility
            .semantic
            .as_ref()
            .ok_or_else(|| {
                CandidateOutputError::Contract(
                    "semantic evaluator profile has no admitted production runtime".to_owned(),
                )
            })?;
        let mut prepared = self.prepared_native.lock().map_err(|_| {
            CandidateOutputError::Contract(
                "semantic evaluator generation cache is unavailable".to_owned(),
            )
        })?;
        if !prepared.contains_key(context.code_generation) {
            let runtime =
                crate::application::semantic_runtime::project_semantic_production_runtime(
                    &self.project_root,
                )
                .ok_or_else(|| {
                    CandidateOutputError::Contract(
                        "production semantic runtime is unavailable".to_owned(),
                    )
                })?;
            let generation = runtime
                .prepare_evaluation_generation(context.code)
                .map_err(|error| CandidateOutputError::Contract(format!("{error:?}")))?;
            if generation.projection() != &required.projection {
                return Err(CandidateOutputError::Contract(
                    "semantic evaluator artifact/projection does not match the candidate pins"
                        .to_owned(),
                ));
            }
            prepared.insert(context.code_generation.clone(), Arc::new(generation));
        }
        let generation = prepared.get(context.code_generation).ok_or_else(|| {
            CandidateOutputError::Contract(
                "semantic evaluator generation cache lost its prepared generation".to_owned(),
            )
        })?;
        if generation.projection() != &required.projection {
            return Err(CandidateOutputError::Contract(
                "cached semantic evaluator projection no longer matches the candidate pins"
                    .to_owned(),
            ));
        }
        let rerank_authority = self
            .candidate
            .compatibility
            .rerank
            .as_ref()
            .and_then(|pins| {
                crate::semantic_code::shared_lifecycle_owner()
                    .and_then(|owner| owner.mount_reranker(pins.clone()).ok())
            });
        generation.with_query_inputs(context, rerank_authority.as_ref(), evaluate)
    }

    fn measure_resources(
        &self,
        context: ProductionCandidateNativeResourceContextV1<'_>,
        execute_queries: &mut dyn FnMut() -> Result<Vec<u64>, CandidateOutputError>,
    ) -> Result<SemanticNativeStageResultV1<SemanticNativeResourceSampleV1>, CandidateOutputError>
    {
        let semantic_resources = self.candidate.compatibility.semantic.as_ref();
        if let Some(required) = semantic_resources {
            let mut prepared = self.prepared_native.lock().map_err(|_| {
                CandidateOutputError::Contract(
                    "semantic evaluator generation cache is unavailable".to_owned(),
                )
            })?;
            if !prepared.contains_key(context.code_generation) {
                let runtime =
                    crate::application::semantic_runtime::project_semantic_production_runtime(
                        &self.project_root,
                    )
                    .ok_or_else(|| {
                        CandidateOutputError::Contract(
                            "production semantic runtime is unavailable".to_owned(),
                        )
                    })?;
                let generation = runtime
                    .prepare_evaluation_generation(context.code)
                    .map_err(|error| CandidateOutputError::Contract(format!("{error:?}")))?;
                if generation.projection() != &required.projection {
                    return Err(CandidateOutputError::Contract(
                        "semantic resource projection does not match candidate pins".to_owned(),
                    ));
                }
                prepared.insert(context.code_generation.clone(), Arc::new(generation));
            }
        }
        let resource_window = LinuxProcessResourceWindowV1::begin();
        let latency_samples_us = execute_queries()?;
        let process_resources = resource_window.and_then(LinuxProcessResourceWindowV1::finish);
        let resources = if semantic_resources.is_some() {
            let prepared = self.prepared_native.lock().map_err(|_| {
                CandidateOutputError::Contract(
                    "semantic evaluator generation cache is unavailable".to_owned(),
                )
            })?;
            let prepared = prepared.get(context.code_generation).ok_or_else(|| {
                CandidateOutputError::Contract(
                    "semantic resource measurement has no prepared generation".to_owned(),
                )
            })?;
            let runtime =
                crate::application::semantic_runtime::project_semantic_production_runtime(
                    &self.project_root,
                )
                .ok_or_else(|| {
                    CandidateOutputError::Contract(
                        "production semantic runtime is unavailable".to_owned(),
                    )
                })?;
            let mut resources = runtime
                .measure_incremental_evaluation_projection(prepared, context.incremental_code)
                .map_err(|error| CandidateOutputError::Contract(format!("{error:?}")))?;
            resources.projection_cases = runtime
                .measure_evaluation_projection_cases(prepared, &context.semantic_projection_sources)
                .map_err(|error| CandidateOutputError::Contract(format!("{error:?}")))?;
            resources
        } else {
            return Ok(SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::ResourceMeasurementUnavailable,
            });
        };
        if resources.source_generation != *context.code_generation
            || resources.source_manifest_digest
                != context.code.projection().request().changes.manifest_digest
            || resources.incremental_source_generation
                != context.incremental_code.manifest().generation_id
            || resources.incremental_source_manifest_digest
                != context
                    .incremental_code
                    .projection()
                    .request()
                    .changes
                    .manifest_digest
            || (semantic_resources.is_some()
                && (resources.model_bytes == 0
                    || resources.vector_bytes == 0
                    || resources.projection_cases.len() != 7))
        {
            return Err(CandidateOutputError::Contract(
                "semantic resource measurement is not bound to the exact prepared generation"
                    .to_owned(),
            ));
        }
        let Some((cpu_time_us, peak_rss_bytes)) = process_resources else {
            return Ok(SemanticNativeStageResultV1::Pending {
                reason: SemanticNativePendingReasonV1::ResourceMeasurementUnavailable,
            });
        };
        Ok(SemanticNativeStageResultV1::Complete(
            SemanticNativeResourceSampleV1 {
                provenance: SemanticNativeResourceProvenanceV1 {
                    workload_digest: context.workload_digest.to_owned(),
                    corpus_digest: context.corpus_digest.to_owned(),
                    scale: context.scale.to_owned(),
                    code_generation_id: resources.source_generation.as_str().to_owned(),
                    code_source_manifest_digest: resources
                        .source_manifest_digest
                        .as_str()
                        .to_owned(),
                    incremental_code_generation_id: resources
                        .incremental_source_generation
                        .as_str()
                        .to_owned(),
                    incremental_code_source_manifest_digest: resources
                        .incremental_source_manifest_digest
                        .as_str()
                        .to_owned(),
                    incremental_before_content_digest: context
                        .incremental_before_content_digest
                        .to_owned(),
                    incremental_after_content_digest: context
                        .incremental_after_content_digest
                        .to_owned(),
                    vector_generation_id: resources
                        .vector_generation
                        .as_ref()
                        .map(|generation| generation.as_digest().as_str().to_owned()),
                    artifact_digest: resources
                        .artifact_digest
                        .as_ref()
                        .map(|digest| digest.as_str().to_owned()),
                    measurement_method: "linux-procfs-v1:cpu=/proc/self/stat(utime+stime,getconf-CLK_TCK);rss=/proc/self/status(VmHWM-process-lifetime-peak);query/clean-build/incremental/stages/projection-cases=std::time::Instant;projection-cases=prepare_semantic_evaluation_projection+DatabaseVectorEvaluationStoreV1(SQLite-CAS,receipts,model-calls,active-pointer,isolated-row-removed-after-run);hydration=canonical-late-hydration+authorized-fixture-filesystem-reads+receipt-count;model=catalog-verified-model-member-length;vector=sum-f32-bytes;index=exact-flat-zero;cache=session-pool-resident-bytes"
                        .to_owned(),
                },
                eligible_chunks: context.eligible_chunks,
                measured_queries: latency_samples_us.len() as u64,
                latency_samples_us,
                cpu_time_us: Some(cpu_time_us),
                peak_rss_bytes: Some(peak_rss_bytes),
                model_bytes: Some(resources.model_bytes),
                vector_bytes: Some(resources.vector_bytes),
                index_bytes: Some(resources.index_bytes),
                cache_bytes: Some(resources.cache_bytes),
                clean_projection_build_samples_us: vec![resources.clean_projection_build_micros],
                incremental_rebuild_samples_us: vec![resources.incremental_rebuild_micros],
                projection_cases: resources.projection_cases,
            },
        ))
    }
}

impl SemanticEvaluationPublicationSnapshotPortV1 for DaemonSemanticEvaluationSnapshotAuthorityV1 {
    fn current(
        &self,
    ) -> SemanticRuntimeFuture<
        '_,
        Result<SemanticEvaluationPublicationSnapshotV1, SemanticActivationCoordinationErrorV1>,
    > {
        Box::pin(async move {
            let code = self
                .scheduler
                .semantic_evaluation_snapshot_for_scope(&self.scope)
                .await
                .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
            let (
                semantic_source_generation,
                semantic_ceiling,
                vector_state_revision,
                vector_generation_id,
            ) = match self.candidate.compatibility.semantic.as_ref() {
                Some(required) => {
                    let runtime =
                        crate::application::semantic_runtime::project_semantic_production_runtime(
                            &self.project_root,
                        )
                        .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?;
                    let semantic = runtime
                        .inspect_compatible_current_generation_snapshot(
                            required,
                            &code.source_generation,
                            &code.source_manifest_digest,
                        )
                        .await
                        .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
                    (
                        Some(code.source_generation.clone()),
                        Some(semantic.executable.observed_ceiling),
                        Some(semantic.vector_state_revision),
                        Some(semantic.vector_generation_id),
                    )
                }
                None => (None, None, None, None),
            };
            let evaluated = crate::search_eval::load_direct_evaluated_profile_material(
                &self.project_root,
                None,
                &self.candidate.evaluated_profile_id,
            )
            .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)?;
            Ok(SemanticEvaluationPublicationSnapshotV1 {
                project_root: self.project_root.clone(),
                scope: self.scope.clone(),
                code_generation: code.source_generation,
                code_source_manifest_digest: code.source_manifest_digest,
                code_snapshot_digest: code.snapshot_digest,
                semantic_source_generation,
                vector_state_revision,
                vector_generation_id,
                runtime: RetrievalRuntimeCompatibilityV1 {
                    retrieval_ceiling:
                        super::code_index_scheduler::queries::maximum_retrieval_budget(),
                    semantic: self.candidate.compatibility.semantic.clone(),
                    semantic_ceiling,
                    rerank: self.candidate.compatibility.rerank.clone(),
                    rerank_ceiling: evaluated.rerank,
                },
            })
        })
    }

    fn evaluate_default_candidate<'a>(
        &'a self,
        repo_root: &'a std::path::Path,
        evaluated_profile_id: &'a str,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<
            crate::search_eval::DirectActivationEvaluationV1,
            SemanticActivationCoordinationErrorV1,
        >,
    > {
        let authority = self.clone();
        let repo_root = repo_root.to_path_buf();
        let evaluated_profile_id = evaluated_profile_id.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let _measurement = RESOURCE_MEASUREMENT_LOCK_V1
                    .lock()
                    .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?;
                evaluate_default_activation_candidate(&repo_root, &evaluated_profile_id, &authority)
                    .map_err(|_| SemanticActivationCoordinationErrorV1::Rejected)
            })
            .await
            .map_err(|_| SemanticActivationCoordinationErrorV1::Unavailable)?
        })
    }

    fn publish_if_current<'a>(
        &'a self,
        expected: &'a SemanticEvaluationPublicationSnapshotV1,
        publication: SemanticEvaluationAuthorityPublicationV1,
    ) -> SemanticRuntimeFuture<'a, Result<(), SemanticActivationCoordinationErrorV1>> {
        Box::pin(async move {
            let expected_code = super::code_index_scheduler::SemanticEvaluationCodeSnapshotV1 {
                source_generation: expected.code_generation.clone(),
                source_manifest_digest: expected.code_source_manifest_digest.clone(),
                snapshot_digest: expected.code_snapshot_digest.clone(),
            };
            let _code_lease = self
                .scheduler
                .acquire_semantic_evaluation_publication_lease(&self.scope, &expected_code)
                .await
                .ok_or(SemanticActivationCoordinationErrorV1::Conflict)?;
            let runtime = match (
                self.candidate.compatibility.semantic.as_ref(),
                expected.vector_state_revision,
                expected.vector_generation_id.as_ref(),
            ) {
                (Some(_), Some(revision), Some(generation)) => Some((
                    crate::application::semantic_runtime::project_semantic_production_runtime(
                        &self.project_root,
                    )
                    .ok_or(SemanticActivationCoordinationErrorV1::Unavailable)?,
                    revision,
                    generation,
                )),
                (None, None, None) => None,
                _ => return Err(SemanticActivationCoordinationErrorV1::Rejected),
            };
            let _vector_lease = match runtime.as_ref() {
                Some((runtime, revision, generation)) => Some(
                    runtime
                        .acquire_vector_publication_lease(*revision, generation)
                        .await
                        .map_err(|_| SemanticActivationCoordinationErrorV1::Conflict)?,
                ),
                None => None,
            };
            publication.commit(expected).await
        })
    }
}

struct LinuxProcessResourceWindowV1 {
    cpu_ticks: u64,
    ticks_per_second: u64,
}

impl LinuxProcessResourceWindowV1 {
    #[cfg(target_os = "linux")]
    fn begin() -> Option<Self> {
        let output = std::process::Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let ticks_per_second = std::str::from_utf8(&output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|ticks| *ticks != 0)?;
        Some(Self {
            cpu_ticks: read_linux_process_cpu_ticks()?,
            ticks_per_second,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn begin() -> Option<Self> {
        None
    }

    fn finish(self) -> Option<(u64, u64)> {
        let elapsed_ticks = read_linux_process_cpu_ticks()?.saturating_sub(self.cpu_ticks);
        let cpu_time_us = u64::try_from(
            u128::from(elapsed_ticks)
                .checked_mul(1_000_000)?
                .checked_div(u128::from(self.ticks_per_second))?,
        )
        .ok()?;
        Some((cpu_time_us, read_linux_process_lifetime_peak_rss_bytes()?))
    }
}

#[cfg(target_os = "linux")]
fn read_linux_process_cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let fields = fields.collect::<Vec<_>>();
    let user_ticks = fields.get(11)?.parse::<u64>().ok()?;
    let system_ticks = fields.get(12)?.parse::<u64>().ok()?;
    user_ticks.checked_add(system_ticks)
}

#[cfg(not(target_os = "linux"))]
fn read_linux_process_cpu_ticks() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn read_linux_process_lifetime_peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1_024)
}

#[cfg(not(target_os = "linux"))]
fn read_linux_process_lifetime_peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn checked_in_linux_quality_evaluation_records_process_resources() {
        let window = LinuxProcessResourceWindowV1::begin()
            .expect("Linux quality evaluation requires procfs and CLK_TCK");

        let (_cpu_time_us, peak_rss_bytes) = window
            .finish()
            .expect("Linux quality evaluation records CPU and peak RSS");

        assert!(peak_rss_bytes > 0);
    }
}

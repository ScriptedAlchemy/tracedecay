use tracedecay_application::native_integration::{
    NativeIntegrationAnalysisPort, NativeIntegrationAnalysisRevalidationV1,
    analyze_native_integration_generations, native_integration_generation_binding,
};
use tracedecay_code_index_runtime::code_index_scheduler::branch_generations::BranchGenerationReadControlV1;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexSchedulerRegistryV1, ExactGitTreeSourceV1, NativeCandidateGenerationIdentityV1,
    NativeCandidateGenerationSourcesV1,
};
use tracedecay_contracts::{
    CancellationSignal, Deadline, NativeIntegrationPortError, ResolvedScope,
};
use tracedecay_domain::{
    CommitId, GitOidV1, ManifestDigest, NativeIntegrationAnalysisReportV1,
    NativeIntegrationGenerationBindingV1, NativeIntegrationSelectionV1, TreeId,
};
use tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1;
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git_repository::{GitNativeCandidateTreeV1, GitNativePreflight};

const ANALYZER_REVISION: &str = "native-semantic-analysis-v1";

/// Production native semantic analyzer backed by the mounted canonical code
/// index scheduler and its durable retained-generation publication authority.
pub struct DaemonNativeIntegrationAnalysisV1 {
    schedulers: CodeIndexSchedulerRegistryV1,
    scope: ResolvedScope,
    runtime: tokio::runtime::Handle,
}

impl DaemonNativeIntegrationAnalysisV1 {
    pub fn new(
        schedulers: CodeIndexSchedulerRegistryV1,
        scope: ResolvedScope,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            schedulers,
            scope,
            runtime,
        }
    }
}

fn exact_source(
    reference: tracedecay_domain::RefId,
    revision: &GitOidV1,
    tree: &GitOidV1,
) -> Result<ExactGitTreeSourceV1, NativeIntegrationPortError> {
    Ok(ExactGitTreeSourceV1 {
        reference,
        revision: CommitId::new(revision.as_str().to_owned()).map_err(native_domain)?,
        tree: TreeId::new(tree.as_str().to_owned()).map_err(native_domain)?,
    })
}

fn identity(binding: &NativeIntegrationGenerationBindingV1) -> NativeCandidateGenerationIdentityV1 {
    NativeCandidateGenerationIdentityV1 {
        generation_id: binding.generation_id.clone(),
        project_id: binding.project_id.clone(),
        repository_id: binding.repository_id.clone(),
        worktree_id: binding.worktree_id.clone(),
        reference: binding.reference.clone(),
        snapshot_digest: binding.snapshot_digest.clone(),
        content_identity: binding.content_identity.clone(),
        source_revision: binding.source_revision.clone(),
        source_tree: binding.source_tree.clone(),
        seal_digest: binding.seal_digest.clone(),
    }
}

fn native_domain(error: impl std::fmt::Display) -> NativeIntegrationPortError {
    NativeIntegrationPortError::Native(error.to_string())
}

fn scheduler_error(reason: CodeIndexSearchUnavailableReasonV1) -> NativeIntegrationPortError {
    match reason {
        CodeIndexSearchUnavailableReasonV1::Cancelled => NativeIntegrationPortError::Cancelled,
        CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired => {
            NativeIntegrationPortError::ResetRequired
        }
        CodeIndexSearchUnavailableReasonV1::Internal => {
            NativeIntegrationPortError::Native(reason.as_str().to_owned())
        }
        _ => NativeIntegrationPortError::Unavailable,
    }
}

impl NativeIntegrationAnalysisPort for DaemonNativeIntegrationAnalysisV1 {
    fn analyze(
        &self,
        selection: &NativeIntegrationSelectionV1,
        native: &GitNativePreflight,
        candidate: &GitNativeCandidateTreeV1<'_>,
        deadline: &Deadline,
        cancellation_signal: &CancellationSignal,
        cancellation: &CancellationToken,
    ) -> Result<NativeIntegrationAnalysisReportV1, NativeIntegrationPortError> {
        if cancellation.is_cancelled() {
            return Err(NativeIntegrationPortError::Cancelled);
        }
        let source_ref = selection.source_ref().map_err(native_domain)?.clone();
        let destination_ref = selection.destination_ref().map_err(native_domain)?.clone();
        let candidate_tree = native
            .candidate_tree
            .as_ref()
            .ok_or(NativeIntegrationPortError::Unavailable)?;
        let sources = NativeCandidateGenerationSourcesV1 {
            merge_base: exact_source(
                source_ref.clone(),
                &native.merge_base,
                &native.merge_base_tree,
            )?,
            source: exact_source(source_ref, &native.source_tip, &native.source_tree)?,
            destination: exact_source(
                destination_ref.clone(),
                &native.destination_tip,
                &native.destination_tree,
            )?,
            candidate_reference: destination_ref,
            candidate_tree: TreeId::new(candidate_tree.as_str().to_owned())
                .map_err(native_domain)?,
        };
        let bindings = self
            .runtime
            .block_on(self.schedulers.with_native_candidate_generation_producer(
                &self.scope,
                BranchGenerationReadControlV1 {
                    deadline: Some(deadline.clone()),
                    cancellation: Some(cancellation_signal.clone()),
                },
                |scheduler, control| {
                    scheduler.publish_native_candidate_generations(&sources, candidate, control)
                },
            ))
            .map_err(scheduler_error)?;
        let findings = analyze_native_integration_generations(
            bindings.merge_base.generation(),
            bindings.source.generation(),
            bindings.destination.generation(),
            bindings.candidate.generation(),
            deadline,
            cancellation_signal,
        )?;
        NativeIntegrationAnalysisReportV1 {
            merge_base: native_integration_generation_binding(
                bindings.merge_base.generation(),
                native.merge_base_tree.clone(),
            )
            .map_err(native_domain)?,
            source: native_integration_generation_binding(
                bindings.source.generation(),
                native.source_tree.clone(),
            )
            .map_err(native_domain)?,
            destination: native_integration_generation_binding(
                bindings.destination.generation(),
                native.destination_tree.clone(),
            )
            .map_err(native_domain)?,
            candidate: native_integration_generation_binding(
                bindings.candidate.generation(),
                candidate_tree.clone(),
            )
            .map_err(native_domain)?,
            graph: findings.graph,
            tests: findings.tests,
            schema: findings.schema,
            migrations: findings.migrations,
            conflicts: findings.conflicts,
            analyzer_revision: ANALYZER_REVISION.to_owned(),
            digest: ManifestDigest::zero().map_err(native_domain)?,
        }
        .seal()
        .map_err(native_domain)
    }

    fn revalidate(
        &self,
        report: &NativeIntegrationAnalysisReportV1,
        deadline: &Deadline,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationAnalysisRevalidationV1, NativeIntegrationPortError> {
        if report.analyzer_revision != ANALYZER_REVISION {
            return Ok(NativeIntegrationAnalysisRevalidationV1::Stale);
        }
        let result =
            self.runtime
                .block_on(self.schedulers.with_native_candidate_generation_producer(
                    &self.scope,
                    BranchGenerationReadControlV1 {
                        deadline: Some(deadline.clone()),
                        cancellation: Some(cancellation.clone()),
                    },
                    |scheduler, _| {
                        let merge_base = scheduler
                            .load_native_candidate_generation(&identity(&report.merge_base))?;
                        let source = scheduler
                            .load_native_candidate_generation(&identity(&report.source))?;
                        let destination = scheduler
                            .load_native_candidate_generation(&identity(&report.destination))?;
                        let candidate = scheduler
                            .load_native_candidate_generation(&identity(&report.candidate))?;
                        Ok((merge_base, source, destination, candidate))
                    },
                ));
        let (merge_base, source, destination, candidate) = match result {
            Ok((Some(merge_base), Some(source), Some(destination), Some(candidate))) => {
                (merge_base, source, destination, candidate)
            }
            Ok(_) | Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable) => {
                return Ok(NativeIntegrationAnalysisRevalidationV1::Stale);
            }
            Err(_) => return Ok(NativeIntegrationAnalysisRevalidationV1::Unavailable),
        };
        let findings = analyze_native_integration_generations(
            merge_base.generation(),
            source.generation(),
            destination.generation(),
            candidate.generation(),
            deadline,
            cancellation,
        )?;
        let mut current = report.clone();
        current.graph = findings.graph;
        current.tests = findings.tests;
        current.schema = findings.schema;
        current.migrations = findings.migrations;
        current.conflicts = findings.conflicts;
        current.digest = ManifestDigest::zero().map_err(native_domain)?;
        current = current.seal().map_err(native_domain)?;
        Ok(if &current == report {
            NativeIntegrationAnalysisRevalidationV1::Current
        } else {
            NativeIntegrationAnalysisRevalidationV1::Stale
        })
    }
}

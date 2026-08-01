//! Production helpers that open the PR12 feedback-cycle runtime from project-open.
//!
//! These builders derive managed diagnostic admissions, policy context, and the
//! LSP trigger→execution bridge from the admitted project identity. They never
//! install Unavailable stub owners.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;

use tracedecay_application::diagnostics::{
    DiagnosticProviderDescriptor, DiagnosticProviderIdentity, DiagnosticProviderIdentityParts,
    ProviderCoverage, ProviderDocumentIdentity, ProviderFreshness, ProviderOrigin,
    ProviderProvenance, ProviderSourceIdentity, RevisionDigest,
};
use tracedecay_application::feedback::{
    FeedbackBudgetUsage, FeedbackCycleAdvisoryV1, FeedbackCycleControl,
    FeedbackCycleExecutionRequest, FeedbackPortFuture, FeedbackRuntimeStatePort,
    ProximityEvaluationRequestV1, feedback_surface_operation,
};
use tracedecay_application::{
    AdvisoryFindingContributorV1, AdvisoryFindingValidityWindowV1, ApplicationContractError,
    ApplicationOperation, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, PolicyDecisionRef, PolicyEvaluationContextV1,
    PolicyEvidenceAgreementV1, PolicyEvidenceFrontierV1, PolicyEvidenceHorizonV1, RequestContext,
    ResolvedScope, now_micros,
};
use tracedecay_domain::configuration::{
    AnalyzerExecutableId, AnalyzerExecutableReferenceV1, AnalyzerLanguageId,
    AnalyzerLanguageSelectionV1, AnalyzerPrivacyClassV1, AnalyzerResourceLimitsV1,
    AnalyzerRestartPolicyV1, AnalyzerSettingsV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
};
use tracedecay_domain::feedback::{
    FeedbackActorContextV1, FeedbackBudgetV1, FeedbackContentIdentityV1, FeedbackCycleId,
    FeedbackCycleRequestV1, FeedbackDurabilityV1, FeedbackEvaluationInputV1, FeedbackScopeV1,
    FeedbackTargetV1, FeedbackTriggerV1,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, CommitId, ComponentVersion, ContentDigest, FileOccurrenceId,
    LanguageDescriptorRevision, LanguageId, ManifestDigest, ProviderId, RetrievalAnchorId, ShardId,
    UtcMicros, VectorWatermark, canonical_sha256,
};
use tracedecay_lsp::{
    DiagnosticTrigger, FeedbackCycleRequest, FeedbackCycleRuntimePort, LspRuntimeFailure,
    LspRuntimeFuture,
};
use tracedecay_policy::TruthSourceStateV1;
use tracedecay_policy::analyzer::{
    AnalyzerAdmissionInputV1, AnalyzerAvailabilityV1, AnalyzerCandidateV1,
    AnalyzerExecutionLocationV1,
};
use tracedecay_tool_catalog::CapabilityId;

use super::cycle_runtime::Pr12FeedbackCycleRuntime;
use super::cycle_runtime::{Pr12FeedbackCycleInvocation, Pr12FeedbackCycleLspInput};
use crate::advisory::{
    ConcretePr13ProximityRuntimeOwnerV1, Pr13ProximityRuntimeOutcomeV1, ProximityThresholdPinV1,
    SharedCanonicalProximityEvidenceAuthorityV1, open_pr13_proximity_runtime,
};
use crate::configuration::ConfigurationCurrentStateV1;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::source_authorization::ProjectSourceAccessSnapshot;
use crate::tracedecay::TraceDecay;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::configuration::OwnedGlobalDbConfigurationControlStore;
use tracedecay_lsp::analyzer::broker::MountedLspProvider;

const POLICY_REVISION_V1: u64 = 1;
const MANAGED_CAPABILITY: &str = "capability.diagnostics.current";

/// Inputs required to open one production feedback-cycle registration.
pub struct ProductionFeedbackCycleOpenV1 {
    pub project_root: PathBuf,
    pub scope: ResolvedScope,
    pub access_configuration: ConfigurationCurrentStateV1,
    pub requester: ActorId,
    pub authorization: Arc<dyn ProductionFeedbackCycleAuthorizationPort>,
    pub graph: Arc<TraceDecay>,
    pub project_runtime_db: Arc<RegisteredGlobalDb>,
    pub runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
    pub document_identity: Arc<dyn ProductionFeedbackDocumentIdentityPort + Send + Sync>,
    pub code_index_identity:
        Arc<dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1>,
    pub test_attribution: Arc<
        dyn tracedecay_code_index::provider::GenerationTestAttributionJoinReadPort + Send + Sync,
    >,
    pub mounted_providers: Vec<MountedLspProvider>,
}

pub type ProductionFeedbackCycleAuthorizationFuture<'a> = Pin<
    Box<dyn Future<Output = Result<ProjectSourceAccessSnapshot, LspRuntimeFailure>> + Send + 'a>,
>;

/// Reloads current project-source authority for every feedback/LSP cycle.
/// Implementations must not reuse an expired project-open grant snapshot.
pub trait ProductionFeedbackCycleAuthorizationPort: Send + Sync {
    fn authorize(&self, observed_at: UtcMicros) -> ProductionFeedbackCycleAuthorizationFuture<'_>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionFeedbackDocumentIdentityV1 {
    pub generation_id: CodeGenerationId,
    pub generation_digest: ManifestDigest,
    pub file: FileOccurrenceId,
    pub content_digest: ContentDigest,
}

impl ProductionFeedbackDocumentIdentityV1 {
    fn file_digest(&self) -> Result<ManifestDigest, LspRuntimeFailure> {
        ManifestDigest::new(self.content_digest.as_str().to_owned())
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-file-digest"))
    }
}

pub type ProductionFeedbackDocumentIdentityFuture = Pin<
    Box<
        dyn Future<Output = Result<ProductionFeedbackDocumentIdentityV1, LspRuntimeFailure>> + Send,
    >,
>;

/// Current, mounted code-index authority for saved LSP document identity.
/// Implementations must reconcile freshness before returning a generation.
pub trait ProductionFeedbackDocumentIdentityPort {
    fn resolve(
        &self,
        project_root: PathBuf,
        document_uri: Option<String>,
    ) -> ProductionFeedbackDocumentIdentityFuture;
}

/// Resolved production cycle open parts for the daemon registrar.
pub struct ProductionFeedbackCyclePartsV1 {
    pub feedback_scope: FeedbackScopeV1,
    pub policy_digest: ManifestDigest,
    pub policy_context: PolicyEvaluationContextV1,
    pub evidence_horizon: PolicyEvidenceHorizonV1,
    pub evaluated_at: UtcMicros,
    pub provider_candidates: Vec<(DiagnosticProviderIdentity, AnalyzerAdmissionInputV1)>,
    pub affected_tests: Arc<dyn tracedecay_application::AffectedTestsRetrievalPort + Send + Sync>,
    pub operation: ApplicationOperation,
    pub graph_operation: ApplicationOperation,
    pub tests_operation: ApplicationOperation,
    pub lsp_input: Pr12FeedbackCycleLspInput,
    pub proximity: Arc<dyn ProductionFeedbackCycleProximityPortV1>,
    pub runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
}

/// Exact saved-generation proximity contribution mounted into the canonical
/// Plan 09 cycle. Implementations return no durable artifact; publication and
/// dedupe remain owned by `Pr12FeedbackCycleRuntime::run_once_with_advisory`.
pub trait ProductionFeedbackCycleProximityPortV1: Send + Sync {
    fn advisory<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
    ) -> FeedbackPortFuture<'a, Result<FeedbackCycleAdvisoryV1, LspRuntimeFailure>>;
}

type ProductionProximityOwnerV1 = ConcretePr13ProximityRuntimeOwnerV1<
    SharedCanonicalProximityEvidenceAuthorityV1,
    OwnedGlobalDbConfigurationControlStore,
>;

struct ProductionFeedbackCycleProximityV1 {
    project_root: PathBuf,
    document_identity: Arc<dyn ProductionFeedbackDocumentIdentityPort + Send + Sync>,
    owner: ProductionProximityOwnerV1,
    threshold_pin: ProximityThresholdPinV1,
}

impl ProductionFeedbackCycleProximityPortV1 for ProductionFeedbackCycleProximityV1 {
    fn advisory<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
    ) -> FeedbackPortFuture<'a, Result<FeedbackCycleAdvisoryV1, LspRuntimeFailure>> {
        Box::pin(async move {
            let current = self
                .document_identity
                .resolve(self.project_root.clone(), None)
                .await?;
            require_current_saved_identity(input, &current)?;
            let request = ProximityEvaluationRequestV1 {
                scope: input.request.scope.clone(),
                observed_at: input.observed_at,
            };
            match self
                .owner
                .evaluate_with_threshold_pin(context, &request, &self.threshold_pin)
                .await
            {
                Pr13ProximityRuntimeOutcomeV1::Completed(contributor) => {
                    let expires_at =
                        UtcMicros(input.observed_at.0.checked_add(1).ok_or_else(|| {
                            LspRuntimeFailure::new("feedback-cycle-proximity-validity")
                        })?);
                    let batch = contributor
                        .advisory_findings(AdvisoryFindingValidityWindowV1 {
                            valid_at: input.observed_at,
                            expires_at,
                        })
                        .map_err(|_| {
                            LspRuntimeFailure::new("feedback-cycle-proximity-contribution")
                        })?;
                    let advisory = FeedbackCycleAdvisoryV1 {
                        // Provider order is the PR13 canonical order:
                        // GitHub, CI, proximity. The LSP/Hook fallback has no
                        // authenticated remote provider target, so it records
                        // those providers as explicitly unavailable rather
                        // than silently omitting them.
                        provider_states: vec![
                            tracedecay_domain::feedback::ProviderEvaluationStateV1::Unavailable,
                            tracedecay_domain::feedback::ProviderEvaluationStateV1::Unavailable,
                            batch.provider_state,
                        ],
                        findings: batch.findings,
                    };
                    advisory
                        .validate()
                        .map_err(|_| LspRuntimeFailure::new("feedback-cycle-proximity-advisory"))?;
                    Ok(advisory)
                }
                Pr13ProximityRuntimeOutcomeV1::Denied => {
                    Err(LspRuntimeFailure::new("feedback-cycle-proximity-denied"))
                }
                Pr13ProximityRuntimeOutcomeV1::Unavailable => Ok(FeedbackCycleAdvisoryV1 {
                    provider_states: vec![
                        tracedecay_domain::feedback::ProviderEvaluationStateV1::Unavailable,
                        tracedecay_domain::feedback::ProviderEvaluationStateV1::Unavailable,
                        tracedecay_domain::feedback::ProviderEvaluationStateV1::Unavailable,
                    ],
                    findings: Vec::new(),
                }),
                Pr13ProximityRuntimeOutcomeV1::Cancelled => {
                    Err(LspRuntimeFailure::new("feedback-cycle-proximity-cancelled"))
                }
                Pr13ProximityRuntimeOutcomeV1::TimedOut => {
                    Err(LspRuntimeFailure::new("feedback-cycle-proximity-timed-out"))
                }
            }
        })
    }
}

fn require_current_saved_identity(
    input: &FeedbackEvaluationInputV1,
    current: &ProductionFeedbackDocumentIdentityV1,
) -> Result<(), LspRuntimeFailure> {
    input
        .validate()
        .map_err(|_| LspRuntimeFailure::new("feedback-cycle-proximity-input"))?;
    let FeedbackContentIdentityV1::SavedContent {
        generation_digest,
        file_digest,
    } = &input.request.content
    else {
        return Err(LspRuntimeFailure::new("feedback-cycle-proximity-overlay"));
    };
    let Some(generation_id) = input.target.generation_id.as_ref() else {
        return Err(LspRuntimeFailure::new(
            "feedback-cycle-proximity-generation",
        ));
    };
    if generation_id != &current.generation_id
        || generation_digest != &current.generation_digest
        || input.target.file != current.file
        || file_digest != &current.file_digest()?
    {
        return Err(LspRuntimeFailure::new(
            "feedback-cycle-proximity-generation-drift",
        ));
    }
    Ok(())
}

struct ProductionProximityFeedbackCycleRuntimeV1 {
    feedback_cycle: Arc<Pr12FeedbackCycleRuntime>,
    lsp_input: Pr12FeedbackCycleLspInput,
    proximity: Arc<dyn ProductionFeedbackCycleProximityPortV1>,
}

impl FeedbackCycleRuntimePort for ProductionProximityFeedbackCycleRuntimeV1 {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let feedback_cycle = Arc::clone(&self.feedback_cycle);
        let lsp_input = Arc::clone(&self.lsp_input);
        let proximity = Arc::clone(&self.proximity);
        Box::pin(async move {
            let invocation = lsp_input(request).await?;
            let advisory = proximity
                .advisory(&invocation.context, &invocation.request.input)
                .await?;
            feedback_cycle
                .run_once_with_advisory(&invocation.context, invocation.request, advisory)
                .await
                .map_err(|_| LspRuntimeFailure::new("feedback-cycle-proximity-execution"))?;
            Ok(())
        })
    }
}

/// LSP-facing production port for the same canonical cycle. It introduces no
/// publication path: the wrapped PR12 owner atomically records the combined
/// result through its existing store.
pub fn production_proximity_feedback_cycle_input(
    feedback_cycle: Arc<Pr12FeedbackCycleRuntime>,
    lsp_input: Pr12FeedbackCycleLspInput,
    proximity: Arc<dyn ProductionFeedbackCycleProximityPortV1>,
) -> Arc<dyn FeedbackCycleRuntimePort> {
    Arc::new(ProductionProximityFeedbackCycleRuntimeV1 {
        feedback_cycle,
        lsp_input,
        proximity,
    })
}

/// Build production cycle registration parts from the providers mounted by
/// the project diagnostics broker. An empty provider set remains a valid
/// cycle: provider-backed diagnostics are typed unavailable while the retained
/// project feedback/LSP owner continues to serve its other projections.
pub async fn resolve_production_feedback_cycle_parts(
    input: ProductionFeedbackCycleOpenV1,
) -> Result<ProductionFeedbackCyclePartsV1, ApplicationContractError> {
    let feedback_scope = feedback_scope_for_project(&input.project_root, &input.scope)?;
    let proximity_threshold_pin = ProximityThresholdPinV1::from_current_configuration(
        &input.access_configuration,
    )
    .ok_or(ApplicationContractError::Inconsistent {
        field: "project-open proximity threshold",
    })?;
    let proximity_evidence =
        crate::advisory::proximity_runtime::production_proximity_evidence_authority_v1(
            Arc::clone(&input.project_runtime_db),
            Arc::clone(&input.graph),
            feedback_scope.clone(),
            input.project_root.clone(),
            Arc::clone(&input.code_index_identity),
        )
        .ok_or(ApplicationContractError::Inconsistent {
            field: "project-open proximity authority",
        })?;
    let proximity_owner = open_pr13_proximity_runtime(
        feedback_scope.clone(),
        proximity_evidence,
        OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(Arc::clone(
            &input.project_runtime_db,
        )),
    )
    .ok_or(ApplicationContractError::Inconsistent {
        field: "project-open proximity runtime",
    })?;
    let proximity: Arc<dyn ProductionFeedbackCycleProximityPortV1> =
        Arc::new(ProductionFeedbackCycleProximityV1 {
            project_root: input.project_root.clone(),
            document_identity: Arc::clone(&input.document_identity),
            owner: proximity_owner,
            threshold_pin: proximity_threshold_pin.clone(),
        });
    let access_configuration_digest = input
        .access_configuration
        .snapshot
        .effective_behavior_digest
        .clone();
    let access_configuration_revision = input.access_configuration.revision_id.clone();
    let policy_digest = canonical_sha256(&(
        "tracedecay.project-open.policy.v1",
        &access_configuration_digest,
        POLICY_REVISION_V1,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open policy digest",
    })?;
    let evaluated_at = now_micros();
    let access = input
        .authorization
        .authorize(evaluated_at)
        .await
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open feedback authorization",
        })?;
    if access.configuration_revision != access_configuration_revision
        || access.configuration_digest != access_configuration_digest
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open feedback configuration",
        });
    }
    let request_context =
        authorized_daemon_request_context(&input.scope, &input.requester, access, evaluated_at)?;
    let policy_context = project_open_policy_context(
        request_context.clone(),
        input.access_configuration.revision_id,
        input.access_configuration.snapshot,
        policy_digest.clone(),
    )?;
    let provider_seed = input
        .document_identity
        .resolve(input.project_root.clone(), None)
        .await
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open provider code-index identity",
        })?;
    let provider_candidates = if input.mounted_providers.is_empty() {
        vec![unavailable_lsp_candidate(
            &input.scope,
            &access_configuration_digest,
            &policy_digest,
            evaluated_at,
            &provider_seed,
        )?]
    } else {
        input
            .mounted_providers
            .iter()
            .map(|provider| {
                managed_lsp_candidate(
                    provider,
                    AnalyzerAvailabilityV1::Available,
                    &input.scope,
                    &access_configuration_digest,
                    &policy_digest,
                    evaluated_at,
                    &provider_seed,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let operation = required_surface_operation("feedback_diagnostics")?;
    let graph_operation = required_surface_operation("feedback_impact")?;
    let tests_operation = required_surface_operation("affected_tests")?;
    let lsp_input = production_lsp_input(ProductionLspInputContext {
        feedback_scope: feedback_scope.clone(),
        scope: input.scope.clone(),
        requester: input.requester.clone(),
        authorization: input.authorization,
        threshold_pin: proximity_threshold_pin,
        policy_digest: policy_digest.clone(),
        providers: provider_candidates
            .iter()
            .map(|(identity, _)| identity.clone())
            .collect(),
        project_root: input.project_root,
        document_identity: input.document_identity,
    })?;
    Ok(ProductionFeedbackCyclePartsV1 {
        feedback_scope,
        policy_digest,
        policy_context,
        evidence_horizon: fresh_evidence_horizon()?,
        evaluated_at,
        provider_candidates,
        affected_tests: Arc::new(
            crate::primitives::TraceDecayAffectedTestsPortV1::with_generation_attribution(
                Arc::clone(&input.graph),
                provider_seed.generation_id.clone(),
                input.test_attribution,
            ),
        ),
        operation,
        graph_operation,
        tests_operation,
        lsp_input,
        proximity,
        runtime_state: input.runtime_state,
    })
}

fn project_open_policy_context(
    request_context: RequestContext,
    configuration_revision: ConfigurationRevisionId,
    configuration: ConfigurationSnapshotV1,
    policy_digest: ManifestDigest,
) -> Result<PolicyEvaluationContextV1, ApplicationContractError> {
    PolicyEvaluationContextV1::new(
        request_context,
        configuration_revision,
        configuration,
        POLICY_REVISION_V1,
        policy_digest,
    )
}

fn feedback_scope_for_project(
    project_root: &Path,
    scope: &ResolvedScope,
) -> Result<FeedbackScopeV1, ApplicationContractError> {
    let branch = tracedecay_runtime_core::branch::current_branch(project_root).ok_or(
        ApplicationContractError::Inconsistent {
            field: "project-open feedback branch",
        },
    )?;
    let branch_ref = format!("refs/heads/{branch}");
    let head = Command::new("git")
        .args(["-C", &project_root.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "project-open feedback head commit",
        })?;
    let feedback = FeedbackScopeV1 {
        project_id: scope.project_id.clone(),
        repository_id: scope.repository_id.clone(),
        worktree_id: scope.worktree_id.clone(),
        branch_ref,
        head_commit_id: CommitId::new(head).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open feedback head commit id",
            }
        })?,
    };
    feedback
        .validate()
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open feedback scope",
        })?;
    Ok(feedback)
}

fn managed_lsp_candidate(
    provider: &MountedLspProvider,
    availability: AnalyzerAvailabilityV1,
    scope: &ResolvedScope,
    configuration_digest: &ManifestDigest,
    policy_digest: &ManifestDigest,
    evaluated_at: UtcMicros,
    document: &ProductionFeedbackDocumentIdentityV1,
) -> Result<(DiagnosticProviderIdentity, AnalyzerAdmissionInputV1), ApplicationContractError> {
    let language = LanguageId::new(provider.language.clone()).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open analyzer language",
        }
    })?;
    let analyzer_language = AnalyzerLanguageId::new(provider.language.clone()).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open analyzer language id",
        }
    })?;
    let provider_digest = canonical_sha256(&(
        "tracedecay.project-open.mounted-lsp-provider.v1",
        &provider.language,
        &provider.command,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open analyzer provider digest",
    })?;
    let provider_suffix = provider_digest.as_str().trim_start_matches("sha256:");
    let executable = AnalyzerExecutableId::new(format!("analyzer.lsp.{provider_suffix}.mounted"))
        .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open analyzer executable",
    })?;
    let capability = CapabilityId::new(MANAGED_CAPABILITY.to_owned()).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open analyzer capability",
        }
    })?;
    let domain_capability = tracedecay_domain::CapabilityId::new(MANAGED_CAPABILITY.to_owned())
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "project-open analyzer capability",
        })?;
    let policy = PolicyDecisionRef::new(
        "policy.decision.project-open.analyzer",
        POLICY_REVISION_V1,
        policy_digest.clone(),
        ComponentVersion::new("policy.evaluator.analyzer.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open analyzer evaluator revision",
            }
        })?,
    )?;
    let identity = DiagnosticProviderIdentity::new(DiagnosticProviderIdentityParts {
        scope: scope.clone(),
        source: ProviderSourceIdentity::CleanGeneration {
            generation: document.generation_id.clone(),
        },
        document: ProviderDocumentIdentity {
            file: document.file.clone(),
            content_digest: document.content_digest.clone(),
            document_version: None,
        },
        producer: DiagnosticProviderDescriptor {
            provider: ProviderId::new(format!("provider.lsp.{provider_suffix}")).map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "project-open analyzer provider",
                }
            })?,
            analyzer_revision: ComponentVersion::new(format!("analyzer.lsp.{provider_suffix}.v1"))
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "project-open analyzer revision",
                })?,
            language: language.clone(),
            language_descriptor_revision: LanguageDescriptorRevision::new(format!(
                "language.lsp.{provider_suffix}.v1"
            ))
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "project-open language descriptor",
            })?,
        },
        requested_capability: capability.clone(),
        freshness: ProviderFreshness::current(evaluated_at),
        coverage: ProviderCoverage::complete(1, 1),
        provenance: ProviderProvenance {
            origin: ProviderOrigin::ConfiguredAnalyzer,
            anchor: Some(
                RetrievalAnchorId::new(format!("anchor.provider.lsp.{provider_suffix}")).map_err(
                    |_| ApplicationContractError::Inconsistent {
                        field: "project-open analyzer anchor",
                    },
                )?,
            ),
        },
        configuration: RevisionDigest {
            revision: ComponentVersion::new("configuration.project-open.v1").map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "project-open configuration revision label",
                }
            })?,
            digest: configuration_digest.clone(),
        },
        policy: policy.clone(),
    })?;
    let admission_input = AnalyzerAdmissionInputV1 {
        settings: AnalyzerSettingsV1 {
            schema_version: AnalyzerSettingsV1::SCHEMA_VERSION,
            selections: vec![AnalyzerLanguageSelectionV1 {
                language_id: analyzer_language.clone(),
                enabled: true,
                executable: AnalyzerExecutableReferenceV1::BuiltIn {
                    executable_id: executable.clone(),
                },
                arguments: Vec::new(),
                initialization_options: BTreeMap::new(),
                settings: BTreeMap::new(),
                environment_allowlist: BTreeSet::new(),
                privacy_class: AnalyzerPrivacyClassV1::NonSensitive,
                resource_limits: AnalyzerResourceLimitsV1 {
                    maximum_memory_mib: 256,
                    startup_timeout_millis: 5_000,
                    request_timeout_millis: 5_000,
                },
                restart_policy: AnalyzerRestartPolicyV1::RestartOnConfigurationChange,
            }],
        },
        language_id: analyzer_language,
        requested_capability: domain_capability.clone(),
        candidates: vec![AnalyzerCandidateV1 {
            executable_id: executable,
            approved_external_digest: None,
            language_id: AnalyzerLanguageId::new(provider.language.clone()).map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "project-open candidate language",
                }
            })?,
            capability_id: domain_capability,
            availability,
            execution_location: AnalyzerExecutionLocationV1::Local,
            scope_authorized: true,
            available_memory_mib: 2_048,
            catalog_digest: configuration_digest.clone(),
        }],
        privacy_constraints: BTreeSet::new(),
        configuration_digest: configuration_digest.clone(),
        policy_revision: POLICY_REVISION_V1,
        policy_digest: policy_digest.clone(),
        evaluated_at,
    };
    let _ = language;
    let _ = capability;
    let _ = policy;
    Ok((identity, admission_input))
}

fn unavailable_lsp_candidate(
    scope: &ResolvedScope,
    configuration_digest: &ManifestDigest,
    policy_digest: &ManifestDigest,
    evaluated_at: UtcMicros,
    document: &ProductionFeedbackDocumentIdentityV1,
) -> Result<(DiagnosticProviderIdentity, AnalyzerAdmissionInputV1), ApplicationContractError> {
    managed_lsp_candidate(
        &MountedLspProvider {
            language: "unavailable".to_owned(),
            command: "unavailable".to_owned(),
        },
        AnalyzerAvailabilityV1::Unavailable,
        scope,
        configuration_digest,
        policy_digest,
        evaluated_at,
        document,
    )
}

struct ProductionLspInputContext {
    feedback_scope: FeedbackScopeV1,
    scope: ResolvedScope,
    requester: ActorId,
    authorization: Arc<dyn ProductionFeedbackCycleAuthorizationPort>,
    threshold_pin: ProximityThresholdPinV1,
    policy_digest: ManifestDigest,
    providers: Vec<DiagnosticProviderIdentity>,
    project_root: PathBuf,
    document_identity: Arc<dyn ProductionFeedbackDocumentIdentityPort + Send + Sync>,
}

fn production_lsp_input(
    input: ProductionLspInputContext,
) -> Result<Pr12FeedbackCycleLspInput, ApplicationContractError> {
    let ProductionLspInputContext {
        feedback_scope,
        scope,
        requester,
        authorization,
        threshold_pin,
        policy_digest,
        providers,
        project_root,
        document_identity,
    } = input;
    let root_uri = url::Url::from_directory_path(
        project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.clone()),
    )
    .map_err(|()| ApplicationContractError::Inconsistent {
        field: "project-open feedback root URI",
    })?;
    let root_path =
        root_uri
            .to_file_path()
            .map_err(|()| ApplicationContractError::Inconsistent {
                field: "project-open feedback root URI",
            })?;
    Ok(Arc::new(move |request: FeedbackCycleRequest| {
        let feedback_scope = feedback_scope.clone();
        let scope = scope.clone();
        let requester = requester.clone();
        let authorization = Arc::clone(&authorization);
        let threshold_pin = threshold_pin.clone();
        let policy_digest = policy_digest.clone();
        let providers = providers.clone();
        let project_root = project_root.clone();
        let root_path = root_path.clone();
        let document_identity = Arc::clone(&document_identity);
        Box::pin(async move {
            if url::Url::parse(&request.root_uri)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .as_ref()
                != Some(&root_path)
            {
                return Err(LspRuntimeFailure::new("feedback-cycle-root-mismatch"));
            }
            let trigger = match request.trigger {
                DiagnosticTrigger::DocumentSave => FeedbackTriggerV1::DocumentSave,
                DiagnosticTrigger::ExplicitDocumentDiagnostics => {
                    FeedbackTriggerV1::ExplicitDiagnostics
                }
            };
            let observed_at = now_micros();
            let access = authorization.authorize(observed_at).await?;
            if access.configuration_revision != threshold_pin.configuration_revision
                || access.configuration_digest != threshold_pin.configuration_digest
            {
                return Err(LspRuntimeFailure::new("feedback-cycle-configuration-drift"));
            }
            let context =
                authorized_daemon_request_context(&scope, &requester, access, observed_at)
                    .map_err(|_| LspRuntimeFailure::new("feedback-cycle-request-context"))?;
            let document = document_identity
                .resolve(project_root, Some(request.document_uri))
                .await?;
            let file_digest = document.file_digest()?;
            let generation_digest = document.generation_digest.clone();
            let providers = providers
                .iter()
                .map(|provider| provider_for_document(provider, &document, observed_at))
                .collect::<Result<Vec<_>, _>>()?;
            let cycle_digest = canonical_sha256(&(
                "tracedecay.project-open.feedback-cycle.v2",
                &feedback_scope,
                &document.generation_id,
                &document.file,
                &document.content_digest,
                trigger,
            ))
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-id"))?;
            let cycle_id = FeedbackCycleId::new(format!(
                "cycle.project-open.{}",
                cycle_digest.as_str().trim_start_matches("sha256:")
            ))
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-id"))?;
            let cycle_request = FeedbackCycleRequestV1::new(
                cycle_id,
                feedback_scope,
                FeedbackContentIdentityV1::SavedContent {
                    generation_digest: generation_digest.clone(),
                    file_digest: file_digest.clone(),
                },
                trigger,
                policy_digest,
                threshold_pin.configuration_digest,
                FeedbackBudgetV1::bounded(1_000, 1_000, 10_000, 10_000),
            )
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-request"))?;
            if cycle_request.durability() != FeedbackDurabilityV1::Durable {
                return Err(LspRuntimeFailure::new("feedback-cycle-non-durable"));
            }
            let input = FeedbackEvaluationInputV1 {
                request: cycle_request,
                target: FeedbackTargetV1 {
                    file: document.file,
                    span: None,
                    symbol: None,
                    generation_id: Some(document.generation_id),
                },
                actor: FeedbackActorContextV1::default(),
                observed_at,
            };
            let execution = FeedbackCycleExecutionRequest {
                input,
                providers,
                maximum_returned_findings: 64,
                usage: FeedbackBudgetUsage {
                    completed_at: now_micros(),
                    tokens_consumed: 0,
                    cost_microunits: 0,
                },
                control: FeedbackCycleControl::Continue,
            };
            Pr12FeedbackCycleInvocation::new(context, execution)
                .map_err(|_| LspRuntimeFailure::new("feedback-cycle-invocation"))
        })
    }))
}

fn provider_for_document(
    provider: &DiagnosticProviderIdentity,
    document: &ProductionFeedbackDocumentIdentityV1,
    observed_at: UtcMicros,
) -> Result<DiagnosticProviderIdentity, LspRuntimeFailure> {
    DiagnosticProviderIdentity::new(DiagnosticProviderIdentityParts {
        scope: provider.scope.clone(),
        source: ProviderSourceIdentity::CleanGeneration {
            generation: document.generation_id.clone(),
        },
        document: ProviderDocumentIdentity {
            file: document.file.clone(),
            content_digest: document.content_digest.clone(),
            document_version: None,
        },
        producer: provider.producer.clone(),
        requested_capability: provider.requested_capability.clone(),
        freshness: ProviderFreshness {
            state: provider.freshness.state,
            observed_at,
        },
        coverage: provider.coverage.clone(),
        provenance: provider.provenance.clone(),
        configuration: provider.configuration.clone(),
        policy: provider.policy.clone(),
    })
    .map_err(|_| LspRuntimeFailure::new("feedback-cycle-provider-identity"))
}

fn required_surface_operation(
    name: &str,
) -> Result<ApplicationOperation, ApplicationContractError> {
    feedback_surface_operation(name)?.ok_or(ApplicationContractError::Inconsistent {
        field: "project-open feedback surface operation",
    })
}

fn fresh_evidence_horizon() -> Result<PolicyEvidenceHorizonV1, ApplicationContractError> {
    Ok(PolicyEvidenceHorizonV1 {
        local_session: PolicyEvidenceFrontierV1 {
            watermark: VectorWatermark {
                components: BTreeMap::from([(
                    ShardId::new("local-session").map_err(|_| {
                        ApplicationContractError::Inconsistent {
                            field: "project-open local-session shard",
                        }
                    })?,
                    1,
                )]),
            },
            state: TruthSourceStateV1::Fresh,
        },
        live_git: PolicyEvidenceFrontierV1 {
            watermark: VectorWatermark {
                components: BTreeMap::from([(
                    ShardId::new("live-git").map_err(|_| {
                        ApplicationContractError::Inconsistent {
                            field: "project-open live-git shard",
                        }
                    })?,
                    1,
                )]),
            },
            state: TruthSourceStateV1::Fresh,
        },
        agreement: PolicyEvidenceAgreementV1::Agree,
    })
}

fn daemon_request_context(
    scope: &ResolvedScope,
    requester: &ActorId,
    grant_expires_at: UtcMicros,
    observed_at: UtcMicros,
) -> Result<RequestContext, ApplicationContractError> {
    let capability = CapabilityId::new(MANAGED_CAPABILITY.to_owned())?;
    let use_case = tracedecay_tool_catalog::UseCaseId::new(
        "use-case.application.feedback.diagnostics".to_owned(),
    )?;
    let mut capabilities = BTreeSet::new();
    capabilities.insert(capability.clone());
    let mut use_cases = BTreeSet::new();
    use_cases.insert(use_case.clone());
    for (capability_id, use_case_id) in [
        (
            "capability.application.feedback.diagnostics",
            "use-case.application.feedback.diagnostics",
        ),
        (
            "capability.application.feedback.impact",
            "use-case.application.feedback.impact",
        ),
        (
            "capability.application.feedback.affected-tests",
            "use-case.application.feedback.affected-tests",
        ),
        (
            tracedecay_application::feedback::GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            tracedecay_application::feedback::GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        ),
        (
            tracedecay_application::feedback::CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
            tracedecay_application::feedback::CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
        ),
        (
            tracedecay_application::feedback::PROXIMITY_CAPABILITY_ID_V1,
            tracedecay_application::feedback::PROXIMITY_USE_CASE_ID_V1,
        ),
    ] {
        capabilities.insert(CapabilityId::new(capability_id.to_owned())?);
        use_cases.insert(tracedecay_tool_catalog::UseCaseId::new(
            use_case_id.to_owned(),
        )?);
    }
    let grant_digest = canonical_sha256(&(
        "tracedecay.project-open.grant.v2",
        requester,
        scope,
        observed_at,
        grant_expires_at,
        &capabilities,
        &use_cases,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open grant digest",
    })?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.tracedecay-daemon.project-open.cycle.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))?,
        1,
        grant_digest,
        ActorId::new("actor.tracedecay-daemon.project-open".to_owned())?,
        observed_at,
        grant_expires_at,
        scope.clone(),
        capabilities,
        use_cases,
        DisclosureClass::Evidence,
    )?;
    let request_id = mint_global_request_id(GlobalRequestSurface::ProjectOpenFeedbackCycle)
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "feedback-cycle request identity",
        })?;
    RequestContext::new(
        requester.clone(),
        scope.clone(),
        grant,
        request_id.clone(),
        Deadline::new(grant_expires_at)?,
        CancellationContext::active(format!("cancel.project-open.cycle.{}", request_id.as_str()))?,
    )
}

fn authorized_daemon_request_context(
    expected_scope: &ResolvedScope,
    expected_requester: &ActorId,
    access: ProjectSourceAccessSnapshot,
    observed_at: UtcMicros,
) -> Result<RequestContext, ApplicationContractError> {
    if access.scope != *expected_scope
        || access.requester != *expected_requester
        || observed_at >= access.grant_expires_at
    {
        return Err(ApplicationContractError::Inconsistent {
            field: "feedback-cycle current authorization",
        });
    }
    for capability in [
        MANAGED_CAPABILITY,
        "capability.application.feedback.diagnostics",
        "capability.application.feedback.impact",
        "capability.application.feedback.affected-tests",
        tracedecay_application::feedback::GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        tracedecay_application::feedback::CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        tracedecay_application::feedback::PROXIMITY_CAPABILITY_ID_V1,
    ] {
        let capability = CapabilityId::new(capability.to_owned())?;
        if !access.effective_capabilities.contains(&capability) {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback-cycle current authorization capability",
            });
        }
    }
    daemon_request_context(
        expected_scope,
        expected_requester,
        access.grant_expires_at,
        observed_at,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use tracedecay_application::diagnostics::AnalyzerAdmittedDiagnosticProviderV1;
    use tracedecay_application::policy::PolicyEvaluatorCompositionV1;
    use tracedecay_domain::configuration::{
        AuthorityRef, ScopeSourceBinding, SourceBindingId, SourceKindV1,
    };
    use tracedecay_domain::{LocatorDigest, ProjectId, RepositoryId, WorktreeId};

    #[derive(Clone)]
    struct Identity(ProductionFeedbackDocumentIdentityV1);

    impl ProductionFeedbackDocumentIdentityPort for Identity {
        fn resolve(
            &self,
            _project_root: PathBuf,
            _document_uri: Option<String>,
        ) -> ProductionFeedbackDocumentIdentityFuture {
            let identity = self.0.clone();
            Box::pin(async move { Ok(identity) })
        }
    }

    struct Authorization {
        access: ProjectSourceAccessSnapshot,
        lifetime_micros: i64,
        calls: Arc<AtomicUsize>,
    }

    impl ProductionFeedbackCycleAuthorizationPort for Authorization {
        fn authorize(
            &self,
            observed_at: UtcMicros,
        ) -> ProductionFeedbackCycleAuthorizationFuture<'_> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut access = self.access.clone();
            access.grant_expires_at = UtcMicros(observed_at.0.saturating_add(self.lifetime_micros));
            Box::pin(async move { Ok(access) })
        }
    }

    fn digest(label: &str) -> ManifestDigest {
        canonical_sha256(&("cycle-production-test", label)).expect("digest")
    }

    fn scope() -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new("project.cycle-production").expect("project"),
            RepositoryId::new("repository.cycle-production").expect("repository"),
            WorktreeId::new("worktree.cycle-production").expect("worktree"),
            Some(tracedecay_domain::RefId::new("refs/heads/main").expect("reference")),
        )
        .expect("scope")
    }

    fn feedback_scope(scope: &ResolvedScope) -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: scope.project_id.clone(),
            repository_id: scope.repository_id.clone(),
            worktree_id: scope.worktree_id.clone(),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: CommitId::new("0123456789abcdef0123456789abcdef01234567")
                .expect("commit"),
        }
    }

    fn document_identity() -> ProductionFeedbackDocumentIdentityV1 {
        ProductionFeedbackDocumentIdentityV1 {
            generation_id: CodeGenerationId::new("generation.test.current").expect("generation"),
            generation_digest: digest("generation"),
            file: FileOccurrenceId::new("file.test.src-lib").expect("file"),
            content_digest: ContentDigest::new(digest("file").as_str().to_owned())
                .expect("content"),
        }
    }

    fn mounted_provider(language: &str) -> MountedLspProvider {
        MountedLspProvider {
            language: language.to_owned(),
            command: format!("{language}-language-server"),
        }
    }

    fn authorization(
        scope: &ResolvedScope,
        configuration_revision: &ConfigurationRevisionId,
        configuration_digest: &ManifestDigest,
        lifetime_micros: i64,
    ) -> (
        Arc<dyn ProductionFeedbackCycleAuthorizationPort>,
        Arc<AtomicUsize>,
    ) {
        let locator = LocatorDigest::new(digest("locator").as_str().to_owned()).expect("locator");
        let binding = ScopeSourceBinding::new(
            SourceBindingId::new("binding.cycle-production").expect("binding"),
            SourceKindV1::Cursor,
            locator,
            AuthorityRef::Project(scope.project_id.clone()),
        )
        .expect("source binding");
        let effective_capabilities = [
            MANAGED_CAPABILITY,
            "capability.application.feedback.diagnostics",
            "capability.application.feedback.impact",
            "capability.application.feedback.affected-tests",
            tracedecay_application::feedback::GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            tracedecay_application::feedback::CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
            tracedecay_application::feedback::PROXIMITY_CAPABILITY_ID_V1,
        ]
        .into_iter()
        .map(|capability| CapabilityId::new(capability.to_owned()).expect("capability"))
        .collect();
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Authorization {
                access: ProjectSourceAccessSnapshot {
                    scope: scope.clone(),
                    requester: ActorId::new("actor.cycle-production").expect("actor"),
                    binding,
                    configuration_revision: configuration_revision.clone(),
                    configuration_digest: configuration_digest.clone(),
                    configuration_provenance_digest: digest("configuration-provenance"),
                    effective_capabilities,
                    grant_expires_at: UtcMicros(0),
                },
                lifetime_micros,
                calls: Arc::clone(&calls),
            }),
            calls,
        )
    }

    fn threshold_pin(
        configuration_revision: &ConfigurationRevisionId,
        configuration_digest: &ManifestDigest,
    ) -> ProximityThresholdPinV1 {
        ProximityThresholdPinV1::new(
            configuration_revision.clone(),
            configuration_digest.clone(),
            5_000,
        )
        .expect("threshold pin")
    }

    #[test]
    fn non_rust_provider_admits_against_the_authoritative_configuration_snapshot() {
        let scope = scope();
        let snapshot = crate::config::resolver::resolve_configuration(
            &crate::config::registry::ConfigurationRegistry::core().expect("registry"),
            &[],
        )
        .expect("configuration resolution")
        .snapshot;
        let configuration_digest = snapshot.effective_behavior_digest.clone();
        let policy_digest = canonical_sha256(&(
            "tracedecay.project-open.policy.v1",
            &configuration_digest,
            POLICY_REVISION_V1,
        ))
        .expect("policy digest");
        let context = project_open_policy_context(
            daemon_request_context(
                &scope,
                &ActorId::new("actor.cycle-production").expect("actor"),
                UtcMicros(3),
                UtcMicros(1),
            )
            .expect("request context"),
            ConfigurationRevisionId::new("configuration.test.current").expect("revision"),
            snapshot,
            policy_digest.clone(),
        )
        .expect("policy context");
        let (identity, input) = managed_lsp_candidate(
            &mounted_provider("python"),
            AnalyzerAvailabilityV1::Available,
            &scope,
            &configuration_digest,
            &policy_digest,
            UtcMicros(1),
            &document_identity(),
        )
        .expect("provider");
        assert_eq!(identity.producer.language.as_str(), "python");

        AnalyzerAdmittedDiagnosticProviderV1::evaluate_current_plan20_snapshot(
            &PolicyEvaluatorCompositionV1::from_application_catalog().expect("policy"),
            &context,
            identity,
            input,
        )
        .expect("the production registration path must admit its current snapshot");
    }

    #[tokio::test]
    async fn production_lsp_input_builds_a_provider_identity_for_the_requested_document() {
        let scope = scope();
        let configuration = digest("configuration");
        let policy = digest("policy");
        let document = document_identity();
        let (provider, _) = managed_lsp_candidate(
            &mounted_provider("typescript"),
            AnalyzerAvailabilityV1::Available,
            &scope,
            &configuration,
            &policy,
            UtcMicros(1),
            &document,
        )
        .expect("provider");
        let configuration_revision =
            ConfigurationRevisionId::new("configuration.test.current").expect("revision");
        let (authorization, _) =
            authorization(&scope, &configuration_revision, &configuration, 1_000_000);
        let input = production_lsp_input(ProductionLspInputContext {
            feedback_scope: feedback_scope(&scope),
            scope,
            requester: ActorId::new("actor.cycle-production").expect("actor"),
            authorization,
            threshold_pin: threshold_pin(&configuration_revision, &configuration),
            policy_digest: policy,
            providers: vec![provider],
            project_root: PathBuf::from("/workspace"),
            document_identity: Arc::new(Identity(document)),
        })
        .expect("input");

        let invocation = input(FeedbackCycleRequest {
            root_uri: "file:///workspace".to_owned(),
            trigger: DiagnosticTrigger::DocumentSave,
            document_uri: "file:///workspace/src/lib.rs".to_owned(),
        })
        .await;

        assert!(
            invocation.is_ok(),
            "a saved document must resolve to an exact provider/input identity"
        );
    }

    #[tokio::test]
    async fn production_lsp_input_reauthorizes_each_cycle_and_rejects_expiry() {
        let scope = scope();
        let configuration = digest("configuration");
        let configuration_revision =
            ConfigurationRevisionId::new("configuration.test.current").expect("revision");
        let policy = digest("policy");
        let document = document_identity();
        let (provider, _) = managed_lsp_candidate(
            &mounted_provider("typescript"),
            AnalyzerAvailabilityV1::Available,
            &scope,
            &configuration,
            &policy,
            UtcMicros(1),
            &document,
        )
        .expect("provider");
        let request = || FeedbackCycleRequest {
            root_uri: "file:///workspace/".to_owned(),
            trigger: DiagnosticTrigger::DocumentSave,
            document_uri: "file:///workspace/src/lib.rs".to_owned(),
        };
        let (current, calls) = authorization(&scope, &configuration_revision, &configuration, 1);
        let input = production_lsp_input(ProductionLspInputContext {
            feedback_scope: feedback_scope(&scope),
            scope: scope.clone(),
            requester: ActorId::new("actor.cycle-production").expect("actor"),
            authorization: current,
            threshold_pin: threshold_pin(&configuration_revision, &configuration),
            policy_digest: policy.clone(),
            providers: vec![provider.clone()],
            project_root: PathBuf::from("/workspace"),
            document_identity: Arc::new(Identity(document.clone())),
        })
        .expect("input");

        assert!(input(request()).await.is_ok());
        assert!(input(request()).await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let (expired, expired_calls) =
            authorization(&scope, &configuration_revision, &configuration, 0);
        let input = production_lsp_input(ProductionLspInputContext {
            feedback_scope: feedback_scope(&scope),
            scope,
            requester: ActorId::new("actor.cycle-production").expect("actor"),
            authorization: expired,
            threshold_pin: threshold_pin(&configuration_revision, &configuration),
            policy_digest: policy,
            providers: vec![provider],
            project_root: PathBuf::from("/workspace"),
            document_identity: Arc::new(Identity(document)),
        })
        .expect("input");

        assert!(input(request()).await.is_err());
        assert_eq!(expired_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn production_lsp_input_rejects_in_flight_configuration_change() {
        let scope = scope();
        let authorized_configuration = digest("configuration.authorized");
        let drifted_configuration = digest("configuration.drifted");
        let configuration_revision =
            ConfigurationRevisionId::new("configuration.test.current").expect("revision");
        let policy = digest("policy");
        let document = document_identity();
        let (provider, _) = managed_lsp_candidate(
            &mounted_provider("typescript"),
            AnalyzerAvailabilityV1::Available,
            &scope,
            &authorized_configuration,
            &policy,
            UtcMicros(1),
            &document,
        )
        .expect("provider");
        let (authorization, _) = authorization(
            &scope,
            &configuration_revision,
            &drifted_configuration,
            1_000_000,
        );
        let input = production_lsp_input(ProductionLspInputContext {
            feedback_scope: feedback_scope(&scope),
            scope,
            requester: ActorId::new("actor.cycle-production").expect("actor"),
            authorization,
            threshold_pin: threshold_pin(&configuration_revision, &authorized_configuration),
            policy_digest: policy,
            providers: vec![provider],
            project_root: PathBuf::from("/workspace"),
            document_identity: Arc::new(Identity(document)),
        })
        .expect("input");

        assert!(
            input(FeedbackCycleRequest {
                root_uri: "file:///workspace/".to_owned(),
                trigger: DiagnosticTrigger::DocumentSave,
                document_uri: "file:///workspace/src/lib.rs".to_owned(),
            })
            .await
            .is_err(),
            "a cycle must not publish under a configuration identity that lost authorization"
        );
    }

    #[tokio::test]
    async fn refreshed_authorization_cannot_widen_the_managed_diagnostics_capability() {
        let scope = scope();
        let configuration_digest = digest("configuration");
        let configuration_revision =
            ConfigurationRevisionId::new("configuration.test.current").expect("revision");
        let (authorization, _) = authorization(
            &scope,
            &configuration_revision,
            &configuration_digest,
            1_000_000,
        );
        let observed_at = UtcMicros(1);
        let mut access = authorization.authorize(observed_at).await.expect("access");
        access
            .effective_capabilities
            .remove(&CapabilityId::new(MANAGED_CAPABILITY.to_owned()).expect("capability"));

        assert!(
            authorized_daemon_request_context(
                &scope,
                &ActorId::new("actor.cycle-production").expect("actor"),
                access,
                observed_at,
            )
            .is_err(),
            "a derived request grant must not add capability.diagnostics.current"
        );
    }

    #[test]
    fn renewed_cycle_grants_have_distinct_immutable_identity() {
        let scope = scope();
        let requester = ActorId::new("actor.cycle-production").expect("actor");
        let first = daemon_request_context(&scope, &requester, UtcMicros(10), UtcMicros(1))
            .expect("first context");
        let renewed = daemon_request_context(&scope, &requester, UtcMicros(20), UtcMicros(11))
            .expect("renewed context");

        assert_ne!(first.grant().grant_id, renewed.grant().grant_id);
        assert_ne!(first.grant().digest, renewed.grant().digest);
        for capability in [
            tracedecay_application::feedback::GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            tracedecay_application::feedback::CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        ] {
            assert!(
                renewed
                    .grant()
                    .allowed_capabilities
                    .contains(&CapabilityId::new(capability.to_owned()).expect("capability"))
            );
        }
    }

    #[tokio::test]
    async fn proximity_mount_accepts_only_the_exact_current_saved_identity() {
        let scope = scope();
        let configuration = digest("configuration");
        let policy = digest("policy");
        let document = document_identity();
        let (provider, _) = managed_lsp_candidate(
            &mounted_provider("typescript"),
            AnalyzerAvailabilityV1::Available,
            &scope,
            &configuration,
            &policy,
            UtcMicros(1),
            &document,
        )
        .expect("provider");
        let configuration_revision =
            ConfigurationRevisionId::new("configuration.test.current").expect("revision");
        let (authorization, _) =
            authorization(&scope, &configuration_revision, &configuration, 1_000_000);
        let input = production_lsp_input(ProductionLspInputContext {
            feedback_scope: feedback_scope(&scope),
            scope,
            requester: ActorId::new("actor.cycle-production").expect("actor"),
            authorization,
            threshold_pin: threshold_pin(&configuration_revision, &configuration),
            policy_digest: policy,
            providers: vec![provider],
            project_root: PathBuf::from("/workspace"),
            document_identity: Arc::new(Identity(document.clone())),
        })
        .expect("input");
        let invocation = input(FeedbackCycleRequest {
            root_uri: "file:///workspace/".to_owned(),
            trigger: DiagnosticTrigger::DocumentSave,
            document_uri: "file:///workspace/src/lib.rs".to_owned(),
        })
        .await
        .expect("invocation");

        assert!(require_current_saved_identity(&invocation.request.input, &document).is_ok());

        let mut drifted = document;
        drifted.generation_id =
            CodeGenerationId::new("generation.test.replaced").expect("generation");
        assert!(require_current_saved_identity(&invocation.request.input, &drifted).is_err());
    }

    #[tokio::test]
    async fn proximity_mount_rejects_dirty_overlay_identity() {
        let scope = scope();
        let configuration = digest("configuration");
        let policy = digest("policy");
        let document = document_identity();
        let (provider, _) = managed_lsp_candidate(
            &mounted_provider("typescript"),
            AnalyzerAvailabilityV1::Available,
            &scope,
            &configuration,
            &policy,
            UtcMicros(1),
            &document,
        )
        .expect("provider");
        let configuration_revision =
            ConfigurationRevisionId::new("configuration.test.current").expect("revision");
        let (authorization, _) =
            authorization(&scope, &configuration_revision, &configuration, 1_000_000);
        let input = production_lsp_input(ProductionLspInputContext {
            feedback_scope: feedback_scope(&scope),
            scope,
            requester: ActorId::new("actor.cycle-production").expect("actor"),
            authorization,
            threshold_pin: threshold_pin(&configuration_revision, &configuration),
            policy_digest: policy,
            providers: vec![provider],
            project_root: PathBuf::from("/workspace"),
            document_identity: Arc::new(Identity(document.clone())),
        })
        .expect("input");
        let mut invocation = input(FeedbackCycleRequest {
            root_uri: "file:///workspace/".to_owned(),
            trigger: DiagnosticTrigger::DocumentSave,
            document_uri: "file:///workspace/src/lib.rs".to_owned(),
        })
        .await
        .expect("invocation");
        invocation.request.input.request.content = FeedbackContentIdentityV1::EphemeralOverlay {
            session_id: tracedecay_domain::SessionId::new("session.overlay").expect("session"),
            owner_client_id: tracedecay_domain::HostInstanceId::new("host.overlay").expect("host"),
            agent_id: None,
            document_version: 1,
            overlay_digest: digest("overlay"),
        };
        invocation.request.input.target.generation_id = None;

        assert!(
            require_current_saved_identity(&invocation.request.input, &document).is_err(),
            "session-local overlays must never enter the durable proximity cycle"
        );
    }

    #[test]
    fn daemon_cycle_grant_authorizes_the_exact_proximity_scope() {
        let scope = scope();
        let feedback_scope = feedback_scope(&scope);
        let observed_at = now_micros();
        let context = daemon_request_context(
            &scope,
            &ActorId::new("actor.cycle-production").expect("actor"),
            UtcMicros(observed_at.0.saturating_add(60_000_000)),
            observed_at,
        )
        .expect("request context");

        assert!(
            crate::advisory::context_allows_feedback_operation(
                &context,
                &feedback_scope,
                tracedecay_application::feedback::PROXIMITY_CAPABILITY_ID_V1,
                tracedecay_application::feedback::PROXIMITY_USE_CASE_ID_V1,
            ),
            "the daemon grant must authorize only the already-admitted proximity scope"
        );
        let mut unauthorized_scope = feedback_scope.clone();
        unauthorized_scope.worktree_id =
            WorktreeId::new("worktree.cycle-production.other").expect("worktree");
        assert!(
            !crate::advisory::context_allows_feedback_operation(
                &context,
                &unauthorized_scope,
                tracedecay_application::feedback::PROXIMITY_CAPABILITY_ID_V1,
                tracedecay_application::feedback::PROXIMITY_USE_CASE_ID_V1,
            ),
            "cross-worktree proximity must fail closed"
        );
        let mut unauthorized_repository_scope = feedback_scope;
        unauthorized_repository_scope.repository_id =
            RepositoryId::new("repository.cycle-production.other").expect("repository");
        assert!(
            !crate::advisory::context_allows_feedback_operation(
                &context,
                &unauthorized_repository_scope,
                tracedecay_application::feedback::PROXIMITY_CAPABILITY_ID_V1,
                tracedecay_application::feedback::PROXIMITY_USE_CASE_ID_V1,
            ),
            "cross-repository proximity must fail closed"
        );
    }

    #[test]
    fn missing_mounted_provider_contributes_typed_unavailable() {
        let scope = scope();
        let snapshot = crate::config::resolver::resolve_configuration(
            &crate::config::registry::ConfigurationRegistry::core().expect("registry"),
            &[],
        )
        .expect("configuration resolution")
        .snapshot;
        let configuration = snapshot.effective_behavior_digest.clone();
        let policy = canonical_sha256(&(
            "tracedecay.project-open.policy.v1",
            &configuration,
            POLICY_REVISION_V1,
        ))
        .expect("policy digest");
        let (identity, input) = unavailable_lsp_candidate(
            &scope,
            &configuration,
            &policy,
            UtcMicros(1),
            &document_identity(),
        )
        .expect("unavailable provider");
        let context = project_open_policy_context(
            daemon_request_context(
                &scope,
                &ActorId::new("actor.cycle-production").expect("actor"),
                UtcMicros(3),
                UtcMicros(1),
            )
            .expect("request context"),
            ConfigurationRevisionId::new("configuration.test.current").expect("revision"),
            snapshot,
            policy,
        )
        .expect("policy context");

        let admitted = AnalyzerAdmittedDiagnosticProviderV1::evaluate_current_plan20_snapshot(
            &PolicyEvaluatorCompositionV1::from_application_catalog().expect("policy"),
            &context,
            identity,
            input,
        )
        .expect("typed unavailable provider");

        assert_eq!(
            admitted.state(),
            tracedecay_application::diagnostics::DiagnosticProviderState::Unavailable
        );
    }
}

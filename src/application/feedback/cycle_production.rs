//! Production helpers that open the PR12 feedback-cycle runtime from project-open.
//!
//! These builders derive managed diagnostic admissions, policy context, and the
//! LSP trigger→execution bridge from the admitted project identity. They never
//! install Unavailable stub owners.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use tracedecay_application::diagnostics::{
    DiagnosticProviderDescriptor, DiagnosticProviderIdentity, DiagnosticProviderIdentityParts,
    ProviderCoverage, ProviderDocumentIdentity, ProviderFreshness, ProviderOrigin,
    ProviderProvenance, ProviderSourceIdentity, RevisionDigest,
};
use tracedecay_application::feedback::{
    FeedbackBudgetUsage, FeedbackCycleControl, FeedbackCycleExecutionRequest,
    FeedbackRuntimeStatePort, feedback_surface_operation,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationOperation, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, PolicyDecisionRef,
    PolicyEvaluationContextV1, PolicyEvidenceAgreementV1, PolicyEvidenceFrontierV1,
    PolicyEvidenceHorizonV1, RequestContext, RequestId, ResolvedScope,
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
use tracedecay_policy::TruthSourceStateV1;
use tracedecay_policy::analyzer::{
    AnalyzerAdmissionInputV1, AnalyzerAvailabilityV1, AnalyzerCandidateV1,
    AnalyzerExecutionLocationV1,
};
use tracedecay_tool_catalog::CapabilityId;

use super::cycle_runtime::{Pr12FeedbackCycleInvocation, Pr12FeedbackCycleLspInput};
use crate::daemon::lsp_gateway::{DiagnosticTrigger, FeedbackCycleRequest, LspRuntimeFailure};
use crate::tracedecay::TraceDecay;

const POLICY_REVISION_V1: u64 = 1;
const MANAGED_ANALYZER_LANGUAGE: &str = "rust";
const MANAGED_ANALYZER_EXECUTABLE: &str = "analyzer.rust-analyzer.builtin";
const MANAGED_CAPABILITY: &str = "capability.diagnostics.current";

/// Inputs required to open one production feedback-cycle registration.
pub struct ProductionFeedbackCycleOpenV1 {
    pub project_root: PathBuf,
    pub scope: ResolvedScope,
    pub access_configuration_digest: ManifestDigest,
    pub access_configuration_revision: ConfigurationRevisionId,
    pub requester: ActorId,
    pub grant_expires_at: UtcMicros,
    pub graph: Arc<TraceDecay>,
    pub runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
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
    pub runtime_state: Arc<dyn FeedbackRuntimeStatePort + Send + Sync>,
}

/// Build production cycle registration parts when a managed analyzer can admit.
pub fn resolve_production_feedback_cycle_parts(
    input: ProductionFeedbackCycleOpenV1,
) -> Result<ProductionFeedbackCyclePartsV1, ApplicationContractError> {
    if !project_admits_managed_rust_analyzer(&input.project_root) {
        return Err(ApplicationContractError::Inconsistent {
            field: "project-open managed diagnostic provider",
        });
    }
    let feedback_scope = feedback_scope_for_project(&input.project_root, &input.scope)?;
    let policy_digest = canonical_sha256(&(
        "tracedecay.project-open.policy.v1",
        &input.access_configuration_digest,
        POLICY_REVISION_V1,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open policy digest",
    })?;
    let evaluated_at = now_micros();
    let request_context = daemon_request_context(
        &input.scope,
        &input.requester,
        input.grant_expires_at,
        evaluated_at,
    )?;
    let policy_context = PolicyEvaluationContextV1::new(
        request_context.clone(),
        input.access_configuration_revision.clone(),
        ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open configuration snapshot",
            }
        })?,
        POLICY_REVISION_V1,
        policy_digest.clone(),
    )?;
    let provider_candidates = vec![managed_rust_analyzer_candidate(
        &input.scope,
        &input.access_configuration_digest,
        &policy_digest,
        evaluated_at,
    )?];
    let operation = required_surface_operation("feedback_diagnostics")?;
    let graph_operation = required_surface_operation("feedback_impact")?;
    let tests_operation = required_surface_operation("affected_tests")?;
    let lsp_input = production_lsp_input(
        feedback_scope.clone(),
        input.scope.clone(),
        input.requester.clone(),
        input.grant_expires_at,
        input.access_configuration_digest.clone(),
        policy_digest.clone(),
        provider_candidates
            .iter()
            .map(|(identity, _)| identity.clone())
            .collect(),
    )?;
    Ok(ProductionFeedbackCyclePartsV1 {
        feedback_scope,
        policy_digest,
        policy_context,
        evidence_horizon: fresh_evidence_horizon()?,
        evaluated_at,
        provider_candidates,
        affected_tests: Arc::new(
            crate::application::primitives::TraceDecayAffectedTestsPortV1::new(Arc::clone(
                &input.graph,
            )),
        ),
        operation,
        graph_operation,
        tests_operation,
        lsp_input,
        runtime_state: input.runtime_state,
    })
}

fn project_admits_managed_rust_analyzer(project_root: &Path) -> bool {
    project_root.join("Cargo.toml").is_file()
        && Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
}

fn feedback_scope_for_project(
    project_root: &Path,
    scope: &ResolvedScope,
) -> Result<FeedbackScopeV1, ApplicationContractError> {
    let branch = crate::branch::current_branch(project_root).ok_or(
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

fn managed_rust_analyzer_candidate(
    scope: &ResolvedScope,
    configuration_digest: &ManifestDigest,
    policy_digest: &ManifestDigest,
    evaluated_at: UtcMicros,
) -> Result<(DiagnosticProviderIdentity, AnalyzerAdmissionInputV1), ApplicationContractError> {
    let language = LanguageId::new(MANAGED_ANALYZER_LANGUAGE).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open analyzer language",
        }
    })?;
    let analyzer_language = AnalyzerLanguageId::new(MANAGED_ANALYZER_LANGUAGE).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open analyzer language id",
        }
    })?;
    let executable = AnalyzerExecutableId::new(MANAGED_ANALYZER_EXECUTABLE).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open analyzer executable",
        }
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
    let generation = CodeGenerationId::new(format!(
        "generation.project-open.{}",
        configuration_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "project-open provider generation",
    })?;
    let file = FileOccurrenceId::new("file.project-open.managed-root").map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "project-open provider file",
        }
    })?;
    let content_digest =
        ContentDigest::new(configuration_digest.as_str().to_owned()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "project-open provider content digest",
            }
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
            generation: generation.clone(),
        },
        document: ProviderDocumentIdentity {
            file,
            content_digest,
            document_version: None,
        },
        producer: DiagnosticProviderDescriptor {
            provider: ProviderId::new("provider.rust-analyzer").map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "project-open analyzer provider",
                }
            })?,
            analyzer_revision: ComponentVersion::new("analyzer.rust-analyzer.v1").map_err(
                |_| ApplicationContractError::Inconsistent {
                    field: "project-open analyzer revision",
                },
            )?,
            language: language.clone(),
            language_descriptor_revision: LanguageDescriptorRevision::new(
                "language.rust.project-open.v1",
            )
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
                RetrievalAnchorId::new("anchor.provider.rust-analyzer.project-open").map_err(
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
            language_id: AnalyzerLanguageId::new(MANAGED_ANALYZER_LANGUAGE).map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "project-open candidate language",
                }
            })?,
            capability_id: domain_capability,
            availability: AnalyzerAvailabilityV1::Available,
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
    let _ = generation;
    let _ = language;
    let _ = capability;
    let _ = policy;
    Ok((identity, admission_input))
}

fn production_lsp_input(
    feedback_scope: FeedbackScopeV1,
    scope: ResolvedScope,
    requester: ActorId,
    grant_expires_at: UtcMicros,
    configuration_digest: ManifestDigest,
    policy_digest: ManifestDigest,
    providers: Vec<DiagnosticProviderIdentity>,
) -> Result<Pr12FeedbackCycleLspInput, ApplicationContractError> {
    Ok(Arc::new(move |request: FeedbackCycleRequest| {
        let feedback_scope = feedback_scope.clone();
        let scope = scope.clone();
        let requester = requester.clone();
        let configuration_digest = configuration_digest.clone();
        let policy_digest = policy_digest.clone();
        let providers = providers.clone();
        Box::pin(async move {
            let trigger = match request.trigger {
                DiagnosticTrigger::DocumentSave => FeedbackTriggerV1::DocumentSave,
                DiagnosticTrigger::ExplicitDocumentDiagnostics => {
                    FeedbackTriggerV1::ExplicitDiagnostics
                }
            };
            let observed_at = now_micros();
            let context = daemon_request_context(&scope, &requester, grant_expires_at, observed_at)
                .map_err(|_| LspRuntimeFailure::new("feedback-cycle-request-context"))?;
            let file_digest = canonical_sha256(&(
                "tracedecay.project-open.feedback-file.v1",
                &request.document_uri,
            ))
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-file-digest"))?;
            let generation_digest = configuration_digest.clone();
            let cycle_id = FeedbackCycleId::new(format!(
                "cycle.project-open.{}",
                file_digest.as_str().trim_start_matches("sha256:")
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
                configuration_digest,
                FeedbackBudgetV1::bounded(1_000, 1_000, 10_000, 10_000),
            )
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-request"))?;
            if cycle_request.durability() != FeedbackDurabilityV1::Durable {
                return Err(LspRuntimeFailure::new("feedback-cycle-non-durable"));
            }
            let generation_id = CodeGenerationId::new(format!(
                "generation.project-open.{}",
                generation_digest.as_str().trim_start_matches("sha256:")
            ))
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-generation"))?;
            let file = FileOccurrenceId::new(format!(
                "file.project-open.{}",
                file_digest.as_str().trim_start_matches("sha256:")
            ))
            .map_err(|_| LspRuntimeFailure::new("feedback-cycle-file"))?;
            let input = FeedbackEvaluationInputV1 {
                request: cycle_request,
                target: FeedbackTargetV1 {
                    file,
                    span: None,
                    symbol: None,
                    generation_id: Some(generation_id),
                },
                actor: FeedbackActorContextV1::default(),
                observed_at,
            };
            let execution = FeedbackCycleExecutionRequest {
                input,
                providers,
                maximum_returned_findings: 64,
                usage: FeedbackBudgetUsage {
                    completed_at: observed_at,
                    tokens_consumed: 1,
                    cost_microunits: 1,
                },
                control: FeedbackCycleControl::Continue,
            };
            Pr12FeedbackCycleInvocation::new(context, execution)
                .map_err(|_| LspRuntimeFailure::new("feedback-cycle-invocation"))
        })
    }))
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
    ] {
        capabilities.insert(CapabilityId::new(capability_id.to_owned())?);
        use_cases.insert(tracedecay_tool_catalog::UseCaseId::new(
            use_case_id.to_owned(),
        )?);
    }
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.tracedecay-daemon.project-open.cycle".to_owned())?,
        1,
        canonical_sha256(&("tracedecay.project-open.grant.v1", requester, scope)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "project-open grant digest",
            },
        )?,
        ActorId::new("actor.tracedecay-daemon.project-open".to_owned())?,
        observed_at,
        grant_expires_at,
        scope.clone(),
        capabilities,
        use_cases,
        DisclosureClass::Evidence,
    )?;
    RequestContext::new(
        requester.clone(),
        scope.clone(),
        grant,
        RequestId::new(format!("request.project-open.cycle.{}", observed_at.0))?,
        Deadline::new(grant_expires_at)?,
        CancellationContext::active(format!("cancel.project-open.cycle.{}", observed_at.0))?,
    )
}

fn now_micros() -> UtcMicros {
    use std::time::{SystemTime, UNIX_EPOCH};
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
    )
}

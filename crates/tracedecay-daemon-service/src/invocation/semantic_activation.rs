//! Composed semantic activation journey: evaluate, publish, then activate.
//!
//! One typed daemon operation carries an operator from an installed model to
//! an active semantic profile. The daemon composes the installed-model
//! material and the configuration compare-and-swap itself, so no caller ever
//! authors artifact digests or install paths over the wire. Every stage
//! failure is the typed problem of the authority that refused it: evaluation
//! problems come from the semantic evaluation route, activation problems from
//! the configuration mutation route (a lost compare-and-swap stays a typed
//! `Conflict` with `RetryDirective::AfterRevalidate`).

use super::*;

use tracedecay_configuration::ConfigurationCurrentStateV1;
use tracedecay_domain::configuration::{ConfigurationValueV1, SettingKey};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_usecases::semantic_runtime::SemanticConfigurationPinV1;

use tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY;
#[cfg(test)]
use tracedecay_semantic_contracts::SemanticModelRemediationV1;
use tracedecay_semantic_contracts::{
    SemanticConfig, SemanticModelLifecycleStateV1, SemanticModelLifecycleStatusV1,
    SemanticProfileSelection,
};

/// Installed-model material required to author a semantic profile selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct InstalledSemanticModelMaterialV1 {
    pub(super) artifact_digest: String,
    pub(super) install_path: PathBuf,
}

/// The composed configuration write for one activation, plus its receipt
/// metadata that travels back on the wire.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ComposedSemanticActivationV1 {
    pub(super) config: SemanticConfig,
    pub(super) rollback_profile_id: Option<String>,
}

impl DaemonInvocationService {
    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "daemon.service.semantic.activate", future = true)]
    pub(super) async fn execute_semantic_activation(
        &self,
        project_root: Option<&Path>,
        request_id: String,
        evaluated_profile_id: String,
        set_rollback: bool,
        observed_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
        request_cancellation: CancellationToken,
    ) -> DaemonInvocationResponse {
        let Some(project_root_path) = project_root.map(Path::to_path_buf) else {
            hotpath::gauge!("daemon.service.semantic.activate.unavailable.project_root_total")
                .inc(1_u64);
            tracing::warn!(
                event = "semantic_activation_admission",
                outcome = "unavailable",
                reason = "project_root",
                "semantic activation has no routed project root"
            );
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        // Admission stays the outermost gate, exactly as on the evaluation
        // route: an unregistered or quiesced project refuses before any
        // lifecycle read or evaluation work.
        let Some(registered) = self.configuration_runtime(project_root).await else {
            hotpath::gauge!(
                "daemon.service.semantic.activate.unavailable.configuration_runtime_total"
            )
            .inc(1_u64);
            tracing::warn!(
                event = "semantic_activation_admission",
                outcome = "unavailable",
                reason = "configuration_runtime",
                "semantic activation configuration runtime is not registered"
            );
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        };
        // Preflight the installed material before the multi-minute native
        // evaluation so a missing model is a fast typed refusal, not a
        // late one.
        let material = match semantic_activation_material(
            tracedecay_usecases::semantic_runtime::project_or_shared_lifecycle_status(
                &project_root_path,
            )
            .as_ref(),
        ) {
            Ok(material) => material,
            Err(problem) => return application_problem(request_id, problem),
        };

        let evaluation = self
            .execute_semantic_evaluation(
                project_root,
                request_id.clone(),
                evaluated_profile_id.clone(),
                observed_at,
                deadline.clone(),
                cancellation.clone(),
                request_cancellation.clone(),
            )
            .await;
        let (scope, profile_digest, report_digest) = match evaluation.outcome {
            DaemonInvocationOutcome::SemanticEvaluatedProfilePublished {
                scope,
                profile_digest,
                report_digest,
                ..
            } => (scope, profile_digest, report_digest),
            outcome => {
                // Evaluation refusals and problems are already the typed
                // truth of that stage; activation never ran and no
                // configuration effect was admitted.
                return DaemonInvocationResponse {
                    protocol: evaluation.protocol,
                    revision: evaluation.revision,
                    request_id: evaluation.request_id,
                    outcome,
                };
            }
        };
        let current = match registered.runtime.client().current().await {
            Ok(current) => current,
            Err(error) => return application_problem(request_id, configuration_problem(error)),
        };
        let composed = compose_activated_semantic_config(
            &current.config.semantic,
            &evaluated_profile_id,
            &profile_digest,
            &material,
            set_rollback,
        );
        let value = match serde_json::to_string(&composed.config) {
            Ok(value) => value,
            Err(_) => {
                return application_problem(
                    request_id,
                    configuration_problem(ConfigurationError::Unavailable),
                );
            }
        };
        let key = match SettingKey::new(SEMANTIC_RUNTIME_SETTING_KEY) {
            Ok(key) => key,
            Err(_) => {
                return application_problem(
                    request_id,
                    configuration_problem(ConfigurationError::Unavailable),
                );
            }
        };
        let expected_revision = current.revision_id.clone();
        let idempotency_key = match ConfigurationIdempotencyKey::new(format!(
            "configuration.idempotency.semantic-activation.{expected_revision}"
        )) {
            Ok(idempotency_key) => idempotency_key,
            Err(_) => {
                return application_problem(
                    request_id,
                    configuration_problem(ConfigurationError::Unavailable),
                );
            }
        };
        let mutation = DirectConfigurationMutation::Set {
            layer: ConfigurationLayerIdV1::Project {
                project_id: registered.runtime.configuration_target().project_id.clone(),
            },
            key,
            value: Box::new(ConfigurationValueV1::Text(value)),
        };
        let activation_observed_at = current_micros();
        let receipt = match issue_direct_configuration_mutation_authority(
            &registered,
            &request_id,
            idempotency_key,
            &mutation,
            expected_revision.clone(),
            deadline.expires_at,
            activation_observed_at,
        ) {
            Ok(mutation_authority) => {
                match apply_configuration_or_semantic_transition(
                    &registered,
                    mutation_authority,
                    mutation,
                    expected_revision,
                    activation_observed_at,
                )
                .await
                {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        return application_problem(request_id, configuration_problem(error));
                    }
                }
            }
            Err(error) => return application_problem(request_id, configuration_problem(error)),
        };

        // Observe the runtime immediately after the activation revision so a
        // caller can distinguish "activation recorded, runtime converging"
        // from "ready" without a second round trip.
        let pin = registered
            .runtime
            .client()
            .current()
            .await
            .ok()
            .and_then(|post| {
                SemanticConfigurationPinV1::from_current(&ConfigurationCurrentStateV1 {
                    revision_id: post.revision_id,
                    snapshot: post.snapshot,
                })
                .ok()
            });
        let runtime_state =
            tracedecay_usecases::semantic_runtime::resolve_project_semantic_runtime_status(
                Some(&project_root_path),
                pin,
            )
            .state;
        // The invocation wire carries the observed state as serialized JSON
        // (the protocol crate does not depend on the usecases state enum).
        let runtime_state = match serde_json::to_value(&runtime_state) {
            Ok(value) => value,
            Err(error) => {
                return application_problem(
                    request_id,
                    ApplicationProblem::unavailable(SafeDiagnostic {
                        code: "semantic_activation.state_serialization_failed".to_owned(),
                        message: format!(
                            "the observed runtime state could not be serialized: {error}"
                        ),
                    }),
                );
            }
        };

        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::SemanticProfileActivated {
                scope,
                profile_digest,
                report_digest,
                configuration_revision: receipt.result_revision_id,
                rollback_profile_id: composed.rollback_profile_id,
                runtime_state,
            },
        )
    }
}

/// Map the model lifecycle onto the material an activation needs, or the
/// typed refusal naming the state that blocks it.
pub(super) fn semantic_activation_material(
    lifecycle: Option<&SemanticModelLifecycleStatusV1>,
) -> Result<InstalledSemanticModelMaterialV1, ApplicationProblem> {
    let state = lifecycle.and_then(|status| status.state.as_ref());
    match state {
        Some(
            SemanticModelLifecycleStateV1::Installed {
                artifact_digest,
                install_path,
                ..
            }
            | SemanticModelLifecycleStateV1::Ready {
                artifact_digest,
                install_path,
                ..
            },
        ) => Ok(InstalledSemanticModelMaterialV1 {
            artifact_digest: artifact_digest.clone(),
            install_path: install_path.clone(),
        }),
        Some(state) => Err(semantic_activation_model_problem(&format!(
            "the selected semantic model is not installed (state: {}); wait for \
             acquisition to finish or re-run `tracedecay tool runtime` to watch it",
            lifecycle_state_label(state)
        ))),
        None => Err(semantic_activation_model_problem(
            "no semantic model is selected or installed; select a model in the \
             `semantic.runtime.v1` configuration (auto_download downloads it in \
             the background) before activating a profile",
        )),
    }
}

fn lifecycle_state_label(state: &SemanticModelLifecycleStateV1) -> &'static str {
    match state {
        SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. } => "selected_not_downloaded",
        SemanticModelLifecycleStateV1::Downloading { .. } => "downloading",
        SemanticModelLifecycleStateV1::Verifying { .. } => "verifying",
        SemanticModelLifecycleStateV1::Installed { .. } => "installed",
        SemanticModelLifecycleStateV1::Loading { .. } => "loading",
        SemanticModelLifecycleStateV1::Indexing { .. } => "indexing",
        SemanticModelLifecycleStateV1::Ready { .. } => "ready",
        SemanticModelLifecycleStateV1::Failed { .. } => "failed",
    }
}

fn semantic_activation_model_problem(message: &str) -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "semantic_activation.model_not_installed".to_owned(),
        message: message.to_owned(),
    })
}

/// Compose the activated semantic configuration from the current one.
///
/// Everything except the profile selection is preserved. The rollback slot
/// only ever holds a selection that differs from the new active one, because
/// the configuration contract rejects `active_profile == rollback_profile`.
pub(super) fn compose_activated_semantic_config(
    current: &SemanticConfig,
    evaluated_profile_id: &str,
    profile_digest: &ManifestDigest,
    material: &InstalledSemanticModelMaterialV1,
    set_rollback: bool,
) -> ComposedSemanticActivationV1 {
    let active = SemanticProfileSelection {
        profile_id: evaluated_profile_id.to_owned(),
        accepted_profile_digest: profile_digest.clone(),
        artifact_digest: material.artifact_digest.clone(),
        artifact_path: material.install_path.clone(),
    };
    let rollback = if set_rollback {
        current
            .active_profile
            .clone()
            .filter(|prior| prior != &active)
    } else {
        current
            .rollback_profile
            .clone()
            .filter(|prior| prior != &active)
    };
    let rollback_profile_id = rollback.as_ref().map(|profile| profile.profile_id.clone());
    ComposedSemanticActivationV1 {
        config: SemanticConfig {
            selected_model: current.selected_model.clone(),
            auto_download: current.auto_download,
            active_profile: Some(active),
            rollback_profile: rollback,
            resources: current.resources,
            document_composition: current.document_composition,
        },
        rollback_profile_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_path(root: &Path) -> PathBuf {
        root.join("models").join("jina")
    }

    fn digest(seed: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64)))
            .expect("valid manifest digest")
    }

    fn material(root: &Path) -> InstalledSemanticModelMaterialV1 {
        InstalledSemanticModelMaterialV1 {
            artifact_digest: "a".repeat(64),
            install_path: model_path(root),
        }
    }

    fn selection(root: &Path, profile_id: &str, seed: char) -> SemanticProfileSelection {
        SemanticProfileSelection {
            profile_id: profile_id.to_owned(),
            accepted_profile_digest: digest(seed),
            artifact_digest: "a".repeat(64),
            artifact_path: model_path(root),
        }
    }

    fn installed_status(root: &Path) -> SemanticModelLifecycleStatusV1 {
        SemanticModelLifecycleStatusV1 {
            selected_model: Some("JinaEmbeddingsV2BaseCode".to_owned()),
            auto_download: false,
            catalog_model_ids: vec!["JinaEmbeddingsV2BaseCode".to_owned()],
            state: Some(SemanticModelLifecycleStateV1::Installed {
                model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
                revision: "rev".to_owned(),
                artifact_digest: "a".repeat(64),
                install_path: model_path(root),
            }),
            remediation: SemanticModelRemediationV1 {
                retry: false,
                remove: false,
                rollback: false,
            },
            semantics_omitted: false,
        }
    }

    #[test]
    fn activation_material_comes_from_the_installed_lifecycle_state() {
        let root = tempfile::tempdir().expect("semantic model fixture root");
        let status = installed_status(root.path());
        let material =
            semantic_activation_material(Some(&status)).expect("installed model material");
        assert_eq!(material.artifact_digest, "a".repeat(64));
        assert_eq!(material.install_path, model_path(root.path()));
    }

    #[test]
    fn activation_refuses_typed_when_the_model_is_still_downloading() {
        let root = tempfile::tempdir().expect("semantic model fixture root");
        let mut status = installed_status(root.path());
        status.state = Some(SemanticModelLifecycleStateV1::Downloading {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            revision: "rev".to_owned(),
            artifact_digest: "a".repeat(64),
            bytes_received: 1,
            bytes_total: 2,
        });
        let problem =
            semantic_activation_material(Some(&status)).expect_err("downloading must refuse");
        assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
        let diagnostic = problem.diagnostic().expect("typed refusal diagnostic");
        assert_eq!(diagnostic.code, "semantic_activation.model_not_installed");
        assert!(
            diagnostic.message.contains("downloading"),
            "refusal must name the blocking lifecycle state: {}",
            diagnostic.message
        );
        assert_eq!(problem.retry(), RetryDirective::AfterDelay);
    }

    #[test]
    fn activation_refuses_typed_when_no_model_is_selected() {
        let problem = semantic_activation_material(None).expect_err("missing model must refuse");
        assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
        assert!(
            problem
                .diagnostic()
                .expect("typed refusal diagnostic")
                .message
                .contains("auto_download"),
            "refusal must tell the operator how to install a model"
        );
    }

    #[test]
    fn composed_activation_preserves_configuration_and_records_prior_rollback() {
        let root = tempfile::tempdir().expect("semantic model fixture root");
        let current = SemanticConfig {
            selected_model: Some("JinaEmbeddingsV2BaseCode".to_owned()),
            auto_download: false,
            active_profile: Some(selection(root.path(), "hybrid-conservative", 'b')),
            rollback_profile: None,
            resources: tracedecay_semantic_contracts::SemanticResourceCeilings::default(),
            document_composition: tracedecay_domain::EmbeddingDocumentCompositionV1::SanitizedText,
        };
        let composed = compose_activated_semantic_config(
            &current,
            "hybrid-conservative",
            &digest('c'),
            &material(root.path()),
            true,
        );
        assert_eq!(
            composed.config.selected_model.as_deref(),
            Some("JinaEmbeddingsV2BaseCode")
        );
        assert!(!composed.config.auto_download);
        let active = composed.config.active_profile.as_ref().expect("active");
        assert_eq!(active.accepted_profile_digest, digest('c'));
        assert_eq!(
            composed.config.rollback_profile,
            Some(selection(root.path(), "hybrid-conservative", 'b')),
            "the prior active selection becomes the rollback target"
        );
        assert_eq!(
            composed.rollback_profile_id.as_deref(),
            Some("hybrid-conservative")
        );
        composed
            .config
            .validate()
            .expect("composed configuration is valid");
    }

    #[test]
    fn reactivating_the_same_profile_never_records_itself_as_rollback() {
        let root = tempfile::tempdir().expect("semantic model fixture root");
        let current = SemanticConfig {
            selected_model: Some("JinaEmbeddingsV2BaseCode".to_owned()),
            auto_download: false,
            active_profile: Some(selection(root.path(), "hybrid-conservative", 'c')),
            rollback_profile: None,
            resources: tracedecay_semantic_contracts::SemanticResourceCeilings::default(),
            document_composition: tracedecay_domain::EmbeddingDocumentCompositionV1::SanitizedText,
        };
        let composed = compose_activated_semantic_config(
            &current,
            "hybrid-conservative",
            &digest('c'),
            &material(root.path()),
            true,
        );
        assert_eq!(composed.config.rollback_profile, None);
        assert_eq!(composed.rollback_profile_id, None);
        composed
            .config
            .validate()
            .expect("composed configuration is valid");
    }

    #[test]
    fn first_activation_without_rollback_intent_keeps_a_differing_existing_rollback() {
        let root = tempfile::tempdir().expect("semantic model fixture root");
        let current = SemanticConfig {
            selected_model: Some("JinaEmbeddingsV2BaseCode".to_owned()),
            auto_download: true,
            active_profile: None,
            rollback_profile: Some(selection(root.path(), "hybrid-aggressive", 'd')),
            resources: tracedecay_semantic_contracts::SemanticResourceCeilings::default(),
            document_composition: tracedecay_domain::EmbeddingDocumentCompositionV1::SanitizedText,
        };
        let composed = compose_activated_semantic_config(
            &current,
            "hybrid-conservative",
            &digest('c'),
            &material(root.path()),
            false,
        );
        assert_eq!(
            composed.config.rollback_profile,
            Some(selection(root.path(), "hybrid-aggressive", 'd'))
        );
        composed
            .config
            .validate()
            .expect("composed configuration is valid");
    }
}

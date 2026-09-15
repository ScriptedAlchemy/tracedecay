//! Project-open source-edit authorization and mutation ownership.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tracedecay_application::source_authorization::{
    ProjectSourceAccessSnapshot, ProjectSourceAccessSnapshotPort,
};
use tracedecay_contracts::request_identity::{PreviewIdentityDomain, derive_preview_identity};
use tracedecay_contracts::{
    ApplicationOperation, CancellationContext, CancellationSignal, Deadline, EffectId,
    IdempotencyKey, RequestContext, RequestId, ResolvedScope, SourceEditAuthorizationAdmissionV1,
    SourceEditAuthorizationFuture, SourceEditAuthorizationPort, SourceEditKind,
    SourceEditReconciliationDispositionV1, SourceEditRequest, now_micros,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::CatalogSnapshotV1;

use tracedecay_source_edit::{SourceEditEffectControlV1, SourceEditRuntime};

const POLICY_REVISION_V1: u64 = 1;
const SOURCE_EDIT_PRIVACY_KEY_EPOCH_V1: u64 = 1;

#[derive(Clone)]
pub struct ProjectSourceEditAuthorizationV1 {
    project_root: PathBuf,
    scope: ResolvedScope,
    configuration: Arc<tracedecay_configuration::ProjectConfigurationRuntime>,
    catalog: Arc<CatalogSnapshotV1>,
    source_access: Arc<dyn ProjectSourceAccessSnapshotPort>,
}

struct CurrentSourceEditAuthorityV1 {
    receipt: tracedecay_contracts::AuthorityReceipt,
    proof: tracedecay_contracts::SourceEditEffectProofV1,
}

impl ProjectSourceEditAuthorizationV1 {
    pub fn new(
        project_root: PathBuf,
        scope: ResolvedScope,
        configuration: Arc<tracedecay_configuration::ProjectConfigurationRuntime>,
        catalog: Arc<CatalogSnapshotV1>,
        source_access: Arc<dyn ProjectSourceAccessSnapshotPort>,
    ) -> Self {
        Self {
            project_root,
            scope,
            configuration,
            catalog,
            source_access,
        }
    }

    #[hotpath::skip]
    async fn current_access(
        &self,
        observed_at: UtcMicros,
    ) -> std::result::Result<ProjectSourceAccessSnapshot, tracedecay_contracts::ApplicationProblem>
    {
        let current = self
            .configuration
            .client()
            .current()
            .await
            .map_err(|_| concealed_source_edit_problem())?;
        self.source_access
            .source_access_at(&self.scope, &self.project_root, &current, observed_at)
            .map_err(|_| concealed_source_edit_problem())
    }

    #[hotpath::skip]
    async fn current_authority(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        observed_at: UtcMicros,
    ) -> std::result::Result<CurrentSourceEditAuthorityV1, tracedecay_contracts::ApplicationProblem>
    {
        let access = self.current_access(observed_at).await?;
        if context.admission_at(observed_at) != tracedecay_contracts::RequestAdmission::Admitted
            || !access.allows(context, operation, observed_at)
        {
            return Err(concealed_source_edit_problem());
        }
        let manifest = self
            .catalog
            .capability(operation.capability_id())
            .ok_or_else(concealed_source_edit_problem)?;
        let catalog_digest =
            tracedecay_domain::ManifestDigest::new(self.catalog.digest().to_string())
                .map_err(|_| concealed_source_edit_problem())?;
        let privacy_domain_id = tracedecay_domain::PrivacyDomainId::new(format!(
            "privacy.local-source-edit.{}",
            access.scope.project_id.as_str()
        ))
        .map_err(|_| concealed_source_edit_problem())?;
        let privacy_digest = canonical_sha256(&(
            "tracedecay.daemon.source-edit-privacy.v1",
            &privacy_domain_id,
            SOURCE_EDIT_PRIVACY_KEY_EPOCH_V1,
            manifest.privacy(),
            manifest.denied_disclosure(),
            manifest.scope(),
            &access.binding,
            &access.configuration_provenance_digest,
        ))
        .map_err(|_| concealed_source_edit_problem())?;
        let policy_digest = canonical_sha256(&(
            "tracedecay.daemon.source-edit-policy.v1",
            &access.scope,
            &access.requester,
            &access.binding,
            &access.configuration_digest,
            &access.configuration_provenance_digest,
            operation.capability_id(),
            operation.use_case_id(),
            &catalog_digest,
            &privacy_digest,
        ))
        .map_err(|_| concealed_source_edit_problem())?;
        let policy = tracedecay_contracts::PolicyDecisionRef::new(
            "policy.daemon.source-edit.v1",
            POLICY_REVISION_V1,
            policy_digest,
            tracedecay_domain::ComponentVersion::new("tracedecay.daemon.source-edit-policy.v1")
                .map_err(|_| concealed_source_edit_problem())?,
        )
        .map_err(|_| concealed_source_edit_problem())?;
        let receipt =
            tracedecay_contracts::AuthorityReceipt::from_context(context, policy, observed_at)
                .map_err(|_| concealed_source_edit_problem())?;
        let proof = tracedecay_contracts::SourceEditEffectProofV1 {
            policy_digest: receipt.policy.digest.clone(),
            configuration_revision_id: access.configuration_revision,
            configuration_digest: access.configuration_digest,
            catalog_revision: manifest.routing().revision(),
            catalog_digest,
            privacy_domain_id,
            privacy_key_epoch: SOURCE_EDIT_PRIVACY_KEY_EPOCH_V1,
            privacy_digest,
            external_proof: None,
        };
        proof
            .validate_for(&receipt)
            .map_err(|_| concealed_source_edit_problem())?;
        Ok(CurrentSourceEditAuthorityV1 { receipt, proof })
    }
}

impl SourceEditAuthorizationPort for ProjectSourceEditAuthorizationV1 {
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move {
            self.current_authority(context, operation, observed_at)
                .await
                .and_then(|current| {
                    SourceEditAuthorizationAdmissionV1::new(
                        current.receipt,
                        current.proof,
                        context.scope(),
                    )
                    .map_err(|_| concealed_source_edit_problem())
                })
        })
    }

    fn recheck_effect<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        admission: &'a SourceEditAuthorizationAdmissionV1,
        observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a> {
        Box::pin(async move {
            let current = self
                .current_authority(context, operation, observed_at)
                .await?;
            if current.receipt.grant_id != admission.receipt.grant_id
                || current.receipt.grant_revision != admission.receipt.grant_revision
                || current.receipt.grant_digest != admission.receipt.grant_digest
                || current.receipt.authorized_scope_digest
                    != admission.receipt.authorized_scope_digest
                || current.receipt.disclosure != admission.receipt.disclosure
                || current.receipt.policy != admission.receipt.policy
                || current.proof != admission.proof
            {
                return Err(concealed_source_edit_problem());
            }
            SourceEditAuthorizationAdmissionV1::new(current.receipt, current.proof, context.scope())
                .map_err(|_| concealed_source_edit_problem())
        })
    }
}

fn concealed_source_edit_problem() -> tracedecay_contracts::ApplicationProblem {
    tracedecay_contracts::ApplicationProblem::not_found_or_not_authorized(
        tracedecay_contracts::RetryDirective::Never,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceEditMutationState {
    Warming,
    Ready,
    Failed,
}

#[derive(Debug)]
pub struct SourceEditMutationGate {
    state: AtomicU8,
}

impl SourceEditMutationGate {
    const WARMING: u8 = 0;
    const READY: u8 = 1;
    const FAILED: u8 = 2;

    pub fn warming() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(Self::WARMING),
        })
    }

    fn state(&self) -> SourceEditMutationState {
        match self.state.load(Ordering::Acquire) {
            Self::READY => SourceEditMutationState::Ready,
            Self::FAILED => SourceEditMutationState::Failed,
            _ => SourceEditMutationState::Warming,
        }
    }

    pub fn mark_ready(&self) {
        self.state.store(Self::READY, Ordering::Release);
    }

    pub fn mark_failed(&self) {
        self.state.store(Self::FAILED, Ordering::Release);
    }

    pub fn authorize_mutation(&self, lane: &str) -> Result<()> {
        match self.state() {
            SourceEditMutationState::Ready => Ok(()),
            SourceEditMutationState::Warming => Err(TraceDecayError::Config {
                message: format!("daemon-owned source edit {lane} authority is warming"),
            }),
            SourceEditMutationState::Failed => Err(TraceDecayError::Config {
                message: format!(
                    "daemon-owned source edit {lane} authority failed to publish; reopen the project"
                ),
            }),
        }
    }
}

pub struct ProjectSourceEditOwnerV1 {
    runtime: Arc<SourceEditRuntime>,
    code_graph: Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort>,
    authorization: ProjectSourceEditAuthorizationV1,
    mutation: Arc<SourceEditMutationGate>,
}

impl ProjectSourceEditOwnerV1 {
    pub fn new(
        runtime: Arc<SourceEditRuntime>,
        code_graph: Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort>,
        authorization: ProjectSourceEditAuthorizationV1,
        mutation: Arc<SourceEditMutationGate>,
    ) -> Self {
        Self {
            runtime,
            code_graph,
            authorization,
            mutation,
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "daemon.project.source_edit", future = true)]
    pub async fn execute(
        &self,
        edit: SourceEditRequest,
        idempotency_key: Option<IdempotencyKey>,
        expected_state: Option<ManifestDigest>,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<tracedecay_contracts::source_edit::SourceEditSurfaceResultV1> {
        if !edit.dry_run() {
            self.mutation.authorize_mutation("mutation")?;
        }
        let observed_at = now_micros();
        let operation = tracedecay_contracts::source_edit_operation(edit.kind())
            .map_err(source_edit_contract_error)?;
        let access = self
            .authorization
            .current_access(observed_at)
            .await
            .map_err(|_| source_edit_authority_error())?;
        let context = source_edit_request_context(
            &access,
            request_id,
            &operation,
            observed_at,
            deadline,
            cancellation.context(),
        )?;
        let effect_control = SourceEditEffectControlV1::for_request(&context, cancellation);
        let current = self
            .authorization
            .current_authority(&context, &operation, observed_at)
            .await
            .map_err(|_| source_edit_authority_error())?;
        let dry_run = edit.dry_run();
        let idempotency_key = match idempotency_key {
            Some(key) => key,
            None if dry_run => {
                let preview_identity = derive_preview_identity(
                    PreviewIdentityDomain::SourceEdit,
                    context.request_id(),
                    &edit,
                )
                .map_err(|error| TraceDecayError::Config {
                    message: format!("source edit preview identity failed: {error}"),
                })?;
                IdempotencyKey::new(format!("preview.{preview_identity}"))
                    .map_err(source_edit_contract_error)?
            }
            None => {
                return Err(TraceDecayError::Config {
                    message: "source edit apply requires an idempotency key".to_owned(),
                });
            }
        };
        let expected_state = match expected_state {
            Some(state) => state,
            None if dry_run => canonical_sha256(&(
                "tracedecay.source-edit-preview-unbound-state.v1",
                context.request_id(),
                &edit,
            ))
            .map_err(|error| TraceDecayError::Config {
                message: format!("source edit preview state identity failed: {error}"),
            })?,
            None => {
                return Err(TraceDecayError::Config {
                    message: "source edit apply requires an expected state".to_owned(),
                });
            }
        };
        let request = tracedecay_contracts::SourceEditEffectRequestV1 {
            context,
            authority: current.receipt.clone(),
            edit,
            idempotency_key,
            expected_state,
            proof: current.proof,
            observed_at,
        };
        tracedecay_source_edit::execute_source_edit_with_control(
            self.runtime.as_ref(),
            self.code_graph.as_ref(),
            &operation,
            request,
            &self.authorization,
            &effect_control,
        )
        .await
        .and_then(source_edit_surface_result)
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "daemon.project.source_edit_rollback", future = true)]
    pub async fn rollback(
        &self,
        effect_id: EffectId,
        original_idempotency_key: IdempotencyKey,
        idempotency_key: IdempotencyKey,
        original_input_digest: ManifestDigest,
        expected_state: ManifestDigest,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<tracedecay_contracts::source_edit::SourceEditSurfaceResultV1> {
        self.mutation.authorize_mutation("rollback")?;
        let observed_at = now_micros();
        let operation = tracedecay_contracts::source_edit_rollback_operation()
            .map_err(source_edit_contract_error)?;
        let access = self
            .authorization
            .current_access(observed_at)
            .await
            .map_err(|_| source_edit_authority_error())?;
        let context = source_edit_request_context(
            &access,
            request_id,
            &operation,
            observed_at,
            deadline,
            cancellation.context(),
        )?;
        let effect_control = SourceEditEffectControlV1::for_request(&context, cancellation);
        let current = self
            .authorization
            .current_authority(&context, &operation, observed_at)
            .await
            .map_err(|_| source_edit_authority_error())?;
        let request = tracedecay_contracts::SourceEditRollbackRequestV1 {
            context,
            authority: current.receipt.clone(),
            effect_id,
            original_idempotency_key,
            idempotency_key,
            original_input_digest,
            expected_state,
            proof: current.proof,
            observed_at,
        };
        tracedecay_source_edit::execute_source_edit_rollback_with_control(
            self.runtime.as_ref(),
            &operation,
            request,
            &self.authorization,
            &effect_control,
        )
        .await
        .and_then(source_edit_surface_result)
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "daemon.project.source_edit_reconciliation", future = true)]
    pub async fn reconcile(
        &self,
        kind: SourceEditKind,
        effect_id: EffectId,
        idempotency_key: IdempotencyKey,
        attempt_idempotency_key: IdempotencyKey,
        input_digest: ManifestDigest,
        disposition: SourceEditReconciliationDispositionV1,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<tracedecay_contracts::source_edit::SourceEditSurfaceResultV1> {
        self.mutation.authorize_mutation("reconciliation")?;
        let observed_at = now_micros();
        let operation = tracedecay_contracts::source_edit_reconciliation_operation()
            .map_err(source_edit_contract_error)?;
        let access = self
            .authorization
            .current_access(observed_at)
            .await
            .map_err(|_| source_edit_authority_error())?;
        let context = source_edit_request_context(
            &access,
            request_id,
            &operation,
            observed_at,
            deadline,
            cancellation.context(),
        )?;
        let effect_control = SourceEditEffectControlV1::for_request(&context, cancellation);
        let current = self
            .authorization
            .current_authority(&context, &operation, observed_at)
            .await
            .map_err(|_| source_edit_authority_error())?;
        let request = tracedecay_contracts::SourceEditReconciliationRequestV1 {
            context,
            authority: current.receipt.clone(),
            kind,
            effect_id,
            idempotency_key,
            attempt_idempotency_key,
            input_digest,
            disposition,
            proof: current.proof,
            observed_at,
        };
        tracedecay_source_edit::reconcile_source_edit_effect_unknown_with_control(
            self.runtime.as_ref(),
            request,
            &self.authorization,
            &effect_control,
        )
        .await
        .and_then(source_edit_surface_result)
    }
}

fn source_edit_request_context(
    access: &ProjectSourceAccessSnapshot,
    request_id: RequestId,
    operation: &ApplicationOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<RequestContext> {
    if cancellation.is_cancelled() || deadline.is_elapsed_at(observed_at) {
        return Err(source_edit_authority_error());
    }
    let expires_at = UtcMicros(deadline.expires_at.0.min(access.grant_expires_at.0));
    if expires_at.0 <= observed_at.0 {
        return Err(source_edit_authority_error());
    }
    let grant_digest = canonical_sha256(&(
        "tracedecay.daemon.source-edit-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        operation.capability_id(),
        operation.use_case_id(),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("source edit route grant unavailable: {error}"),
    })?;
    let grant = tracedecay_contracts::CapabilityGrantSnapshot::new(
        tracedecay_contracts::CapabilityGrantId::new(format!(
            "grant.daemon.source-edit.{}",
            grant_digest.as_str().trim_start_matches("sha256:")
        ))
        .map_err(source_edit_contract_error)?,
        POLICY_REVISION_V1,
        grant_digest,
        access.requester.clone(),
        observed_at,
        expires_at,
        access.scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        tracedecay_contracts::DisclosureClass::Sensitive,
    )
    .map_err(source_edit_contract_error)?;
    RequestContext::new(
        access.requester.clone(),
        access.scope.clone(),
        grant,
        request_id,
        Deadline::new(expires_at).map_err(source_edit_contract_error)?,
        cancellation,
    )
    .map_err(source_edit_contract_error)
}

fn source_edit_contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("source edit invocation contract is invalid: {error}"),
    }
}

fn source_edit_authority_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: "source edit was not found or is not authorized".to_owned(),
    }
}

fn source_edit_surface_result(
    result: tracedecay_source_edit::SourceEditApplicationResult,
) -> Result<tracedecay_contracts::source_edit::SourceEditSurfaceResultV1> {
    let replayed = result.replayed;
    let mut value = result.value();
    let object = value
        .as_object_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: "source edit result did not serialize to its canonical object contract"
                .to_owned(),
        })?;
    object.insert("replayed".to_owned(), serde_json::Value::Bool(replayed));
    serde_json::from_value(value).map_err(|error| TraceDecayError::Config {
        message: format!("source edit result violated its canonical surface contract: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::SourceEditMutationGate;

    #[test]
    fn mutation_gate_distinguishes_warming_ready_and_failed_publication() {
        let gate = SourceEditMutationGate::warming();
        assert!(
            gate.authorize_mutation("mutation")
                .expect_err("warming mutation must be refused")
                .to_string()
                .contains("warming")
        );

        gate.mark_ready();
        gate.authorize_mutation("mutation")
            .expect("published mutation authority");

        gate.mark_failed();
        assert!(
            gate.authorize_mutation("mutation")
                .expect_err("failed publication must be terminal")
                .to_string()
                .contains("failed to publish")
        );
    }
}

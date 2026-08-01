//! Canonical application authority for Doctor remediation references.
//!
//! Doctor remains diagnostic-only. This authority validates owner-supplied
//! operations, enforces current legal actions and confirmation, persists
//! intents and receipts, resumes interrupted effects, and independently
//! re-observes terminal effects. Transports only decode and render it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::doctor::{
    DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1,
    DoctorRemediationRegistryV1,
};
use tracedecay_application::{
    EffectReceipt, IdempotencyKey, OperationReceipt, PreviewId, RequestId,
};
use tracedecay_domain::ManifestDigest;

use crate::agents::host_bundle_v2::{HostBundleComponentV1, HostKindV1};
use crate::operation_stream::OperationId;
use crate::application_surface::{
    ConfigurationProtectedApplySurfaceRequest, ConfigurationProtectedPreviewSurfaceRequest,
};
use crate::request_identity::{
    derive_doctor_remediation_apply_operation, derive_doctor_remediation_preview_operation,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoctorRemediationDispatchCommandV1 {
    Preview {
        operation: DoctorOwningOperationRefV1,
        target: DoctorRemediationTargetV1,
    },
    Apply {
        operation: DoctorOwningOperationRefV1,
        target: DoctorRemediationTargetV1,
        preview_id: Option<PreviewId>,
        idempotency_key: IdempotencyKey,
    },
    Resume {
        operation: DoctorOwningOperationRefV1,
        target: DoctorRemediationTargetV1,
        preview_id: Option<PreviewId>,
        idempotency_key: IdempotencyKey,
    },
    Status {
        operation_id: OperationId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "owner_operation", content = "target", rename_all = "snake_case")]
// Configuration protected variants carry surface requests; boxing would change the wire shape.
#[allow(clippy::large_enum_variant)]
pub enum DoctorRemediationTargetV1 {
    StorageRetentionCollect,
    StorageCollectOrphanStore,
    StorageBranchGc,
    StorageQuarantineAndCollectDebris,
    ConfigurationProtectedPreview(ConfigurationProtectedPreviewSurfaceRequest),
    ConfigurationProtectedApply(ConfigurationProtectedApplySurfaceRequest),
    ConfigurationPinAuthority,
    RuntimeRecoverDaemon,
    HostRepairIntegration {
        host: HostKindV1,
        components: Vec<HostBundleComponentV1>,
    },
    CodeIndexRemount,
}

impl DoctorRemediationTargetV1 {
    pub fn for_operation(operation: &DoctorOwningOperationRefV1) -> Option<Self> {
        use tracedecay_application::doctor::operations;
        match operation.as_str() {
            operations::STORAGE_RETENTION_COLLECT => Some(Self::StorageRetentionCollect),
            operations::STORAGE_COLLECT_ORPHAN_STORE => Some(Self::StorageCollectOrphanStore),
            operations::STORAGE_BRANCH_GC => Some(Self::StorageBranchGc),
            operations::STORAGE_QUARANTINE_AND_COLLECT_DEBRIS => {
                Some(Self::StorageQuarantineAndCollectDebris)
            }
            operations::CONFIGURATION_PIN_AUTHORITY => Some(Self::ConfigurationPinAuthority),
            operations::RUNTIME_RECOVER_DAEMON => Some(Self::RuntimeRecoverDaemon),
            operations::CODE_INDEX_REMOUNT => Some(Self::CodeIndexRemount),
            operations::CONFIGURATION_PROTECTED_APPLY | operations::HOST_REPAIR_INTEGRATION => None,
            _ => None,
        }
    }

    fn operation(&self) -> &'static str {
        use tracedecay_application::doctor::operations;
        match self {
            Self::StorageRetentionCollect => operations::STORAGE_RETENTION_COLLECT,
            Self::StorageCollectOrphanStore => operations::STORAGE_COLLECT_ORPHAN_STORE,
            Self::StorageBranchGc => operations::STORAGE_BRANCH_GC,
            Self::StorageQuarantineAndCollectDebris => {
                operations::STORAGE_QUARANTINE_AND_COLLECT_DEBRIS
            }
            Self::ConfigurationProtectedPreview(_) | Self::ConfigurationProtectedApply(_) => {
                operations::CONFIGURATION_PROTECTED_APPLY
            }
            Self::ConfigurationPinAuthority => operations::CONFIGURATION_PIN_AUTHORITY,
            Self::RuntimeRecoverDaemon => operations::RUNTIME_RECOVER_DAEMON,
            Self::HostRepairIntegration { .. } => operations::HOST_REPAIR_INTEGRATION,
            Self::CodeIndexRemount => operations::CODE_INDEX_REMOUNT,
        }
    }

    fn validate_for(
        &self,
        operation: &DoctorOwningOperationRefV1,
        kind: DoctorRemediationKindV1,
    ) -> Result<(), DoctorRemediationDispatchErrorV1> {
        let phase_matches = match (self, kind) {
            (Self::ConfigurationProtectedPreview(_), DoctorRemediationKindV1::Preview)
            | (Self::ConfigurationProtectedApply(_), DoctorRemediationKindV1::Action) => true,
            (Self::ConfigurationProtectedPreview(_) | Self::ConfigurationProtectedApply(_), _) => {
                false
            }
            (Self::RuntimeRecoverDaemon, DoctorRemediationKindV1::Preview) => false,
            (Self::HostRepairIntegration { components, .. }, _) => {
                !components.is_empty()
                    && components.len() <= 4
                    && !components
                        .iter()
                        .enumerate()
                        .any(|(index, current)| components[index + 1..].contains(current))
            }
            _ => true,
        };
        (phase_matches && operation.as_str() == self.operation())
            .then_some(())
            .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
    }

    pub fn digest(&self) -> Result<ManifestDigest, DoctorRemediationDispatchErrorV1> {
        tracedecay_domain::canonical_sha256(&("tracedecay.doctor-remediation-target.v1", self))
            .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRemediationOperationPhaseV1 {
    Previewed,
    Running,
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    EffectUnknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct DoctorRemediationOperationV1 {
    pub operation_id: OperationId,
    pub owning_operation: DoctorOwningOperationRefV1,
    pub phase: DoctorRemediationOperationPhaseV1,
    pub preview_id: Option<PreviewId>,
    pub execution: Option<OperationReceipt>,
    pub effect_receipt: Option<EffectReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_effect_receipt: Option<EffectReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_result_digest: Option<ManifestDigest>,
    #[serde(default)]
    pub verification: DoctorRemediationVerificationV1,
}

/// Independent post-effect observation. Owner execution cannot set this state;
/// only the separately admitted observation callback can.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
#[derive(Default)]
pub enum DoctorRemediationVerificationV1 {
    /// A mutating owner effect has not yet been independently observed.
    #[default]
    Pending,
    /// A preview or a cancellation before any effect requires no recovery check.
    NotRequired,
    /// Current owner state proves the finding is recovered.
    Verified { observation_digest: ManifestDigest },
    /// Current owner state is usable but evidence or recovery remains incomplete.
    Partial {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation_digest: Option<ManifestDigest>,
    },
    /// Current owner state proves the remediation did not recover the finding.
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation_digest: Option<ManifestDigest>,
    },
    /// Current authority denied the independent observation.
    Denied,
    /// Current owner state could not be observed.
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DurableDoctorRemediationRecordV1 {
    schema_version: u16,
    kind: DoctorRemediationKindV1,
    target: DoctorRemediationTargetV1,
    target_digest: ManifestDigest,
    idempotency_key: Option<IdempotencyKey>,
    operation: DoctorRemediationOperationV1,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRemediationDispatchErrorV1 {
    Unsupported,
    Denied,
    InvalidReference,
    ConfirmationRequired,
    OwnerUnavailable,
}

pub type DoctorRemediationDispatchFuture = Pin<
    Box<
        dyn Future<Output = Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1>>
            + Send
            + 'static,
    >,
>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRemediationLegalActionV1 {
    RequestPreview,
    RequestApply,
}

pub type DoctorRemediationLegalActionsFuture =
    Pin<Box<dyn Future<Output = Vec<DoctorRemediationLegalActionV1>> + Send + 'static>>;
pub type LegalActions = Arc<
    dyn Fn(DoctorRemediationRefV1) -> DoctorRemediationLegalActionsFuture + Send + Sync + 'static,
>;
pub type Dispatch = Arc<
    dyn Fn(DoctorRemediationDispatchCommandV1) -> DoctorRemediationDispatchFuture
        + Send
        + Sync
        + 'static,
>;
pub type DoctorRemediationObservationFuture = Pin<
    Box<
        dyn Future<
                Output = Result<DoctorRemediationVerificationV1, DoctorRemediationDispatchErrorV1>,
            > + Send
            + 'static,
    >,
>;
pub type Observation = Arc<
    dyn Fn(DoctorRemediationOperationV1) -> DoctorRemediationObservationFuture
        + Send
        + Sync
        + 'static,
>;

#[derive(Clone)]
pub struct DoctorRemediationAuthorityV1 {
    // Both callbacks are owner supplied. They must re-check current authority;
    // construction-time admission alone never authorizes a later request.
    legal_actions: LegalActions,
    dispatch: Dispatch,
    observation: Observation,
    durable_receipt_root: Option<std::path::PathBuf>,
    dispatch_gate: Arc<tokio::sync::Mutex<()>>,
}

impl DoctorRemediationAuthorityV1 {
    pub fn new(legal_actions: LegalActions, dispatch: Dispatch, observation: Observation) -> Self {
        Self {
            legal_actions,
            dispatch,
            observation,
            durable_receipt_root: None,
            dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn new_durable(
        durable_receipt_root: std::path::PathBuf,
        legal_actions: LegalActions,
        dispatch: Dispatch,
        observation: Observation,
    ) -> Self {
        let dispatch_gate = shared_durable_dispatch_gate(&durable_receipt_root);
        Self {
            legal_actions,
            dispatch,
            observation,
            durable_receipt_root: Some(durable_receipt_root),
            dispatch_gate,
        }
    }

    pub async fn legal_actions(
        &self,
        reference: &DoctorRemediationRefV1,
    ) -> Vec<DoctorRemediationLegalActionV1> {
        let preview_available = DoctorRemediationRegistryV1::default_registry()
            .resolve(reference)
            .is_ok_and(tracedecay_application::DoctorRemediationDescriptorV1::preview_available);
        (self.legal_actions)(reference.clone())
            .await
            .into_iter()
            .filter(|kind| {
                matches!(
                    (reference.kind(), *kind, preview_available),
                    (
                        DoctorRemediationKindV1::Preview | DoctorRemediationKindV1::Action,
                        DoctorRemediationLegalActionV1::RequestPreview,
                        true
                    ) | (
                        DoctorRemediationKindV1::Action,
                        DoctorRemediationLegalActionV1::RequestApply,
                        _
                    )
                )
            })
            .collect()
    }

    pub async fn preview(
        &self,
        command: DoctorRemediationDispatchCommandV1,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        let DoctorRemediationDispatchCommandV1::Preview { operation, target } = &command else {
            return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
        };
        target.validate_for(operation, DoctorRemediationKindV1::Preview)?;
        let reference =
            DoctorRemediationRefV1::new(operation.clone(), DoctorRemediationKindV1::Preview);
        DoctorRemediationRegistryV1::default_registry()
            .resolve(&reference)
            .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
        if !self
            .legal_actions(&reference)
            .await
            .contains(&DoctorRemediationLegalActionV1::RequestPreview)
        {
            return Err(DoctorRemediationDispatchErrorV1::Denied);
        }
        self.dispatch_command(command).await
    }

    pub async fn apply(
        &self,
        command: DoctorRemediationDispatchCommandV1,
        confirmed: bool,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        if !confirmed {
            return Err(DoctorRemediationDispatchErrorV1::ConfirmationRequired);
        }
        let DoctorRemediationDispatchCommandV1::Apply {
            operation, target, ..
        } = &command
        else {
            return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
        };
        target.validate_for(operation, DoctorRemediationKindV1::Action)?;
        let reference =
            DoctorRemediationRefV1::new(operation.clone(), DoctorRemediationKindV1::Action);
        DoctorRemediationRegistryV1::default_registry()
            .resolve(&reference)
            .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
        if !self
            .legal_actions(&reference)
            .await
            .contains(&DoctorRemediationLegalActionV1::RequestApply)
        {
            return Err(DoctorRemediationDispatchErrorV1::Denied);
        }
        self.dispatch_command(command).await
    }

    pub async fn status(
        &self,
        operation_id: OperationId,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        self.dispatch_command(DoctorRemediationDispatchCommandV1::Status { operation_id })
            .await
    }

    async fn dispatch_command(
        &self,
        command: DoctorRemediationDispatchCommandV1,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        let _dispatch_guard = self.dispatch_gate.lock().await;
        if let (Some(root), DoctorRemediationDispatchCommandV1::Status { operation_id }) =
            (&self.durable_receipt_root, &command)
            && let Some(record) = read_durable_operation(root, operation_id)?
        {
            validate_persisted_outcome(record.operation.clone())?;
            if record.operation.phase == DoctorRemediationOperationPhaseV1::Running {
                let reference = DoctorRemediationRefV1::new(
                    record.operation.owning_operation.clone(),
                    record.kind,
                );
                if self.legal_actions(&reference).await.is_empty() {
                    return Err(DoctorRemediationDispatchErrorV1::Denied);
                }
                let recovery = match record.kind {
                    DoctorRemediationKindV1::Preview => {
                        DoctorRemediationDispatchCommandV1::Preview {
                            operation: record.operation.owning_operation.clone(),
                            target: record.target.clone(),
                        }
                    }
                    DoctorRemediationKindV1::Action => DoctorRemediationDispatchCommandV1::Resume {
                        operation: record.operation.owning_operation.clone(),
                        target: record.target.clone(),
                        preview_id: record.operation.preview_id.clone(),
                        idempotency_key: record
                            .idempotency_key
                            .clone()
                            .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)?,
                    },
                };
                let outcome = (self.dispatch)(recovery.clone()).await?;
                validate_command_outcome(&recovery, &outcome)?;
                if outcome.operation_id != record.operation.operation_id {
                    return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
                }
                let outcome = self.finish_owner_outcome(outcome).await?;
                let recovered = DurableDoctorRemediationRecordV1 {
                    operation: outcome.clone(),
                    ..record
                };
                write_durable_record(root, &recovered)?;
                if recovered.idempotency_key.is_some() {
                    write_idempotency_record(
                        root,
                        &recovered.operation.owning_operation,
                        &recovered,
                    )?;
                }
                return Ok(outcome);
            }
            return self.reobserve_record(root, record).await;
        }
        let (kind, operation, target, idempotency_key) = match &command {
            DoctorRemediationDispatchCommandV1::Preview { operation, target } => (
                DoctorRemediationKindV1::Preview,
                operation.clone(),
                target.clone(),
                None,
            ),
            DoctorRemediationDispatchCommandV1::Apply {
                operation,
                target,
                idempotency_key,
                ..
            } => (
                DoctorRemediationKindV1::Action,
                operation.clone(),
                target.clone(),
                Some(idempotency_key.clone()),
            ),
            DoctorRemediationDispatchCommandV1::Status { .. } => {
                let outcome = (self.dispatch)(command.clone()).await?;
                validate_command_outcome(&command, &outcome)?;
                return self.finish_owner_outcome(outcome).await;
            }
            DoctorRemediationDispatchCommandV1::Resume { .. } => {
                return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
            }
        };
        target.validate_for(&operation, kind)?;
        let target_digest = target.digest()?;
        let expected_operation_id = operation_id_for_command(&command)?;
        let mut owner_command = command.clone();
        if let Some(root) = &self.durable_receipt_root {
            if let Some(key) = &idempotency_key
                && let Some(record) = read_idempotency_record(root, &operation, key)?
            {
                if record.target_digest != target_digest || record.kind != kind {
                    return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
                }
                if record.operation.phase != DoctorRemediationOperationPhaseV1::Running {
                    return self.reobserve_record(root, record).await;
                }
                owner_command = DoctorRemediationDispatchCommandV1::Resume {
                    operation: record.operation.owning_operation,
                    target: record.target,
                    preview_id: record.operation.preview_id,
                    idempotency_key: record
                        .idempotency_key
                        .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)?,
                };
            }
            let intent = DurableDoctorRemediationRecordV1 {
                schema_version: 1,
                kind,
                target: target.clone(),
                target_digest: target_digest.clone(),
                idempotency_key: idempotency_key.clone(),
                operation: DoctorRemediationOperationV1 {
                    operation_id: expected_operation_id.clone(),
                    owning_operation: operation.clone(),
                    phase: DoctorRemediationOperationPhaseV1::Running,
                    preview_id: match &command {
                        DoctorRemediationDispatchCommandV1::Apply { preview_id, .. } => {
                            preview_id.clone()
                        }
                        _ => None,
                    },
                    execution: None,
                    effect_receipt: None,
                    owner_effect_receipt: None,
                    owner_result_digest: None,
                    verification: DoctorRemediationVerificationV1::Pending,
                },
            };
            write_durable_record(root, &intent)?;
            if idempotency_key.is_some() {
                write_idempotency_record(root, &operation, &intent)?;
            }
        }
        let outcome = (self.dispatch)(owner_command).await?;
        validate_command_outcome(&command, &outcome)?;
        let outcome = self.finish_owner_outcome(outcome).await?;
        if let Some(root) = &self.durable_receipt_root {
            if outcome.operation_id != expected_operation_id {
                return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
            }
            let record = DurableDoctorRemediationRecordV1 {
                schema_version: 1,
                kind,
                target,
                target_digest,
                idempotency_key: idempotency_key.clone(),
                operation: outcome.clone(),
            };
            write_durable_record(root, &record)?;
            if idempotency_key.is_some() {
                write_idempotency_record(root, &operation, &record)?;
            }
        }
        Ok(outcome)
    }

    async fn finish_owner_outcome(
        &self,
        outcome: DoctorRemediationOperationV1,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        validate_owner_outcome(outcome.clone())?;
        if outcome.phase == DoctorRemediationOperationPhaseV1::EffectUnknown {
            return Ok(outcome);
        }
        self.reobserve_if_needed(outcome).await
    }

    async fn reobserve_if_needed(
        &self,
        mut outcome: DoctorRemediationOperationV1,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        if !requires_reobservation(&outcome) {
            return Ok(outcome);
        }
        outcome.verification = match (self.observation)(outcome.clone()).await {
            Ok(
                verification @ (DoctorRemediationVerificationV1::Verified { .. }
                | DoctorRemediationVerificationV1::Partial { .. }
                | DoctorRemediationVerificationV1::Failed { .. }
                | DoctorRemediationVerificationV1::Denied
                | DoctorRemediationVerificationV1::Unavailable),
            ) => verification,
            Ok(
                DoctorRemediationVerificationV1::Pending
                | DoctorRemediationVerificationV1::NotRequired,
            ) => return Err(DoctorRemediationDispatchErrorV1::InvalidReference),
            Err(DoctorRemediationDispatchErrorV1::Denied) => {
                DoctorRemediationVerificationV1::Denied
            }
            Err(
                DoctorRemediationDispatchErrorV1::OwnerUnavailable
                | DoctorRemediationDispatchErrorV1::Unsupported,
            ) => DoctorRemediationVerificationV1::Unavailable,
            Err(error) => return Err(error),
        };
        Ok(outcome)
    }

    async fn reobserve_record(
        &self,
        root: &std::path::Path,
        record: DurableDoctorRemediationRecordV1,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        let operation = self.reobserve_if_needed(record.operation.clone()).await?;
        if operation != record.operation {
            let observed = DurableDoctorRemediationRecordV1 {
                operation: operation.clone(),
                ..record
            };
            write_durable_record(root, &observed)?;
            if observed.idempotency_key.is_some() {
                write_idempotency_record(root, &observed.operation.owning_operation, &observed)?;
            }
        }
        Ok(operation)
    }
}

fn requires_reobservation(outcome: &DoctorRemediationOperationV1) -> bool {
    matches!(
        outcome.phase,
        DoctorRemediationOperationPhaseV1::Completed
            | DoctorRemediationOperationPhaseV1::Failed
            | DoctorRemediationOperationPhaseV1::Partial
            | DoctorRemediationOperationPhaseV1::EffectUnknown
    ) && !matches!(
        outcome.verification,
        DoctorRemediationVerificationV1::Verified { .. }
    )
}

fn validate_command_outcome(
    command: &DoctorRemediationDispatchCommandV1,
    outcome: &DoctorRemediationOperationV1,
) -> Result<(), DoctorRemediationDispatchErrorV1> {
    let expected_operation_id = operation_id_for_command(command)?;
    let (expected_operation, expected_preview, expected_key, is_preview) = match command {
        DoctorRemediationDispatchCommandV1::Preview { operation, .. } => {
            (operation, None, None, true)
        }
        DoctorRemediationDispatchCommandV1::Apply {
            operation,
            preview_id,
            idempotency_key,
            ..
        }
        | DoctorRemediationDispatchCommandV1::Resume {
            operation,
            preview_id,
            idempotency_key,
            ..
        } => (operation, preview_id.as_ref(), Some(idempotency_key), false),
        DoctorRemediationDispatchCommandV1::Status { operation_id } => {
            return (outcome.operation_id == *operation_id)
                .then_some(())
                .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference);
        }
    };
    (outcome.operation_id == expected_operation_id
        && outcome.owning_operation == *expected_operation
        && if is_preview {
            outcome.preview_id.is_some()
        } else {
            outcome.preview_id.as_ref() == expected_preview
        }
        && outcome
            .effect_receipt
            .as_ref()
            .is_none_or(|receipt| Some(&receipt.idempotency_key) == expected_key))
    .then_some(())
    .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
}

fn shared_durable_dispatch_gate(root: &std::path::Path) -> Arc<tokio::sync::Mutex<()>> {
    static GATES: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<std::path::PathBuf, std::sync::Weak<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();
    let gates = GATES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut gates = gates
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(root).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    gates.insert(root.to_path_buf(), Arc::downgrade(&gate));
    gate
}

pub fn operation_id_for_command(
    command: &DoctorRemediationDispatchCommandV1,
) -> Result<OperationId, DoctorRemediationDispatchErrorV1> {
    let digest = match command {
        DoctorRemediationDispatchCommandV1::Preview { operation, target } => {
            derive_doctor_remediation_preview_operation(operation, &target.digest()?)
        }
        DoctorRemediationDispatchCommandV1::Apply {
            operation,
            target,
            idempotency_key,
            ..
        }
        | DoctorRemediationDispatchCommandV1::Resume {
            operation,
            target,
            idempotency_key,
            ..
        } => {
            derive_doctor_remediation_apply_operation(operation, &target.digest()?, idempotency_key)
        }
        DoctorRemediationDispatchCommandV1::Status { operation_id } => {
            return Ok(operation_id.clone());
        }
    }
    .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    RequestId::new(format!("request.doctor-remediation.{}", digest.as_str()))
        .map(OperationId::from_request)
        .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)
}

fn durable_operation_path(
    root: &std::path::Path,
    operation_id: &OperationId,
) -> Result<std::path::PathBuf, DoctorRemediationDispatchErrorV1> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.doctor-remediation-operation.v1",
        operation_id,
    ))
    .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    Ok(root.join(format!("{}.json", digest.as_str())))
}

fn read_durable_operation(
    root: &std::path::Path,
    operation_id: &OperationId,
) -> Result<Option<DurableDoctorRemediationRecordV1>, DoctorRemediationDispatchErrorV1> {
    let path = durable_operation_path(root, operation_id)?;
    read_record_path(&path)?.map_or(Ok(None), |record| {
        (record.schema_version == 1 && record.operation.operation_id == *operation_id)
            .then_some(Some(record))
            .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
    })
}

fn write_durable_record(
    root: &std::path::Path,
    record: &DurableDoctorRemediationRecordV1,
) -> Result<(), DoctorRemediationDispatchErrorV1> {
    let path = durable_operation_path(root, &record.operation.operation_id)?;
    write_record_path(&path, record)
}

fn idempotency_record_path(
    root: &std::path::Path,
    operation: &DoctorOwningOperationRefV1,
    key: &IdempotencyKey,
) -> Result<std::path::PathBuf, DoctorRemediationDispatchErrorV1> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.doctor-remediation-idempotency.v1",
        operation,
        key,
    ))
    .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    Ok(root
        .join("idempotency")
        .join(format!("{}.json", digest.as_str())))
}

fn read_idempotency_record(
    root: &std::path::Path,
    operation: &DoctorOwningOperationRefV1,
    key: &IdempotencyKey,
) -> Result<Option<DurableDoctorRemediationRecordV1>, DoctorRemediationDispatchErrorV1> {
    let path = idempotency_record_path(root, operation, key)?;
    read_record_path(&path)
}

fn write_idempotency_record(
    root: &std::path::Path,
    operation: &DoctorOwningOperationRefV1,
    record: &DurableDoctorRemediationRecordV1,
) -> Result<(), DoctorRemediationDispatchErrorV1> {
    let key = record
        .idempotency_key
        .as_ref()
        .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)?;
    write_record_path(&idempotency_record_path(root, operation, key)?, record)
}

fn read_record_path(
    path: &std::path::Path,
) -> Result<Option<DurableDoctorRemediationRecordV1>, DoctorRemediationDispatchErrorV1> {
    let parent = path
        .parent()
        .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)?;
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(parent)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let _lock =
        tracedecay_runtime_core::storage::acquire_sidecar_lock_blocking(&tracedecay_runtime_core::storage::append_lock_path(path))
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(DoctorRemediationDispatchErrorV1::InvalidReference)
        }
        Ok(_) => {
            let bytes = std::fs::read(path)
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            let mut record: DurableDoctorRemediationRecordV1 = serde_json::from_slice(&bytes)
                .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
            if record.operation.verification == DoctorRemediationVerificationV1::Pending
                && matches!(
                    record.operation.phase,
                    DoctorRemediationOperationPhaseV1::Previewed
                        | DoctorRemediationOperationPhaseV1::Cancelled
                        | DoctorRemediationOperationPhaseV1::TimedOut
                )
            {
                record.operation.verification = DoctorRemediationVerificationV1::NotRequired;
            }
            Ok(Some(record))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable),
    }
}

fn write_record_path(
    path: &std::path::Path,
    record: &DurableDoctorRemediationRecordV1,
) -> Result<(), DoctorRemediationDispatchErrorV1> {
    let parent = path
        .parent()
        .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)?;
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(parent)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let _lock =
        tracedecay_runtime_core::storage::acquire_sidecar_lock_blocking(&tracedecay_runtime_core::storage::append_lock_path(path))
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let bytes = serde_json::to_vec(record)
        .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    let temp_path = path.with_extension(format!("json.tmp-{}", std::process::id()));
    tracedecay_runtime_core::storage::PrivateStoreIo::write_file_atomically(path, &temp_path, &bytes)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)
}

fn validate_owner_outcome(
    outcome: DoctorRemediationOperationV1,
) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
    validate_outcome(outcome, true)
}

fn validate_persisted_outcome(
    outcome: DoctorRemediationOperationV1,
) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
    validate_outcome(outcome, false)
}

fn validate_outcome(
    outcome: DoctorRemediationOperationV1,
    owner_boundary: bool,
) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
    let reference_kind = if outcome.phase == DoctorRemediationOperationPhaseV1::Previewed {
        DoctorRemediationKindV1::Preview
    } else {
        DoctorRemediationKindV1::Action
    };
    if DoctorRemediationRegistryV1::default_registry()
        .resolve(&DoctorRemediationRefV1::new(
            outcome.owning_operation.clone(),
            reference_kind,
        ))
        .is_err()
    {
        return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
    }
    let invalid_effect_binding = outcome.effect_receipt.as_ref().is_some_and(|receipt| {
        receipt.operation.as_str() != outcome.owning_operation.as_str()
            || &receipt.request_id != outcome.operation_id.request_id()
            || outcome
                .execution
                .as_ref()
                .is_none_or(|execution| execution.termination != receipt.outcome.into())
    });
    if outcome
        .execution
        .as_ref()
        .is_some_and(|receipt| receipt.validate().is_err())
        || outcome
            .effect_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.validate().is_err())
        || outcome
            .owner_effect_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.validate().is_err())
        || invalid_effect_binding
    {
        return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
    }
    let expected_termination = match outcome.phase {
        DoctorRemediationOperationPhaseV1::Previewed
        | DoctorRemediationOperationPhaseV1::Completed => {
            Some(tracedecay_application::OperationTermination::Completed)
        }
        DoctorRemediationOperationPhaseV1::Cancelled => {
            Some(tracedecay_application::OperationTermination::Cancelled)
        }
        DoctorRemediationOperationPhaseV1::TimedOut => {
            Some(tracedecay_application::OperationTermination::TimedOut)
        }
        DoctorRemediationOperationPhaseV1::Failed => {
            Some(tracedecay_application::OperationTermination::Failed)
        }
        DoctorRemediationOperationPhaseV1::Partial => {
            Some(tracedecay_application::OperationTermination::Partial)
        }
        DoctorRemediationOperationPhaseV1::EffectUnknown => {
            Some(tracedecay_application::OperationTermination::EffectUnknown)
        }
        DoctorRemediationOperationPhaseV1::Running => None,
    };
    let owner_set_verification = !matches!(
        outcome.verification,
        DoctorRemediationVerificationV1::Pending | DoctorRemediationVerificationV1::NotRequired
    );
    let invalid_initial_verification = match outcome.phase {
        DoctorRemediationOperationPhaseV1::Previewed
        | DoctorRemediationOperationPhaseV1::Cancelled
        | DoctorRemediationOperationPhaseV1::TimedOut => {
            outcome.verification != DoctorRemediationVerificationV1::NotRequired
        }
        _ => outcome.verification != DoctorRemediationVerificationV1::Pending,
    };
    if (invalid_initial_verification || owner_set_verification) && owner_boundary
        || expected_termination.is_some_and(|expected| {
            outcome
                .execution
                .as_ref()
                .is_none_or(|execution| execution.termination != expected)
        })
        || (outcome.phase == DoctorRemediationOperationPhaseV1::Previewed
            && (outcome.preview_id.is_none() || outcome.effect_receipt.is_some()))
        || matches!(
            outcome.phase,
            DoctorRemediationOperationPhaseV1::Completed
                | DoctorRemediationOperationPhaseV1::EffectUnknown
        ) && outcome.effect_receipt.is_none()
    {
        return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
    }
    Ok(outcome)
}

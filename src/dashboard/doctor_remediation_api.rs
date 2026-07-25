//! Admitted owner-operation routing for Doctor remediation references.
//!
//! Doctor remains diagnostic-only. These endpoints delegate preview, apply,
//! and durable status reads to an optional owner-supplied dispatcher. No
//! dispatcher means unsupported; the dashboard never derives or executes a
//! repair from a finding.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use tracedecay_application::doctor::{
    DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1,
    DoctorRemediationRegistryV1,
};
use tracedecay_application::{
    EffectReceipt, IdempotencyKey, OperationReceipt, PreviewId, RequestId,
};
use tracedecay_domain::ManifestDigest;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, scope_from_state,
};
use crate::agents::host_bundle_v2::{HostBundleComponentV1, HostKindV1};
use crate::application::operation_stream::OperationId;
use crate::application_surface::{
    ConfigurationProtectedApplySurfaceRequest, ConfigurationProtectedPreviewSurfaceRequest,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DoctorRemediationDispatchCommandV1 {
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "owner_operation", content = "target", rename_all = "snake_case")]
pub(crate) enum DoctorRemediationTargetV1 {
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

    pub(crate) fn digest(&self) -> Result<ManifestDigest, DoctorRemediationDispatchErrorV1> {
        tracedecay_domain::canonical_sha256(&("tracedecay.doctor-remediation-target.v1", self))
            .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DoctorRemediationOperationPhaseV1 {
    Previewed,
    Running,
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    EffectUnknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DoctorRemediationOperationV1 {
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

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DoctorRemediationDispatchErrorV1 {
    Unsupported,
    Denied,
    InvalidReference,
    ConfirmationRequired,
    OwnerUnavailable,
}

pub(crate) type DoctorRemediationDispatchFuture = Pin<
    Box<
        dyn Future<Output = Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1>>
            + Send
            + 'static,
    >,
>;

pub(crate) type DoctorRemediationLegalActionsFuture =
    Pin<Box<dyn Future<Output = Vec<DashboardLegalActionKindV1>> + Send + 'static>>;
pub(crate) type LegalActions = Arc<
    dyn Fn(DoctorRemediationRefV1) -> DoctorRemediationLegalActionsFuture + Send + Sync + 'static,
>;
pub(crate) type Dispatch = Arc<
    dyn Fn(DoctorRemediationDispatchCommandV1) -> DoctorRemediationDispatchFuture
        + Send
        + Sync
        + 'static,
>;

#[derive(Clone)]
pub(crate) struct DoctorRemediationDispatcherV1 {
    // Both callbacks are owner supplied. They must re-check current authority;
    // construction-time admission alone never authorizes a later request.
    legal_actions: LegalActions,
    dispatch: Dispatch,
    durable_receipt_root: Option<std::path::PathBuf>,
    dispatch_gate: Arc<tokio::sync::Mutex<()>>,
}

impl DoctorRemediationDispatcherV1 {
    pub(crate) fn new(legal_actions: LegalActions, dispatch: Dispatch) -> Self {
        Self {
            legal_actions,
            dispatch,
            durable_receipt_root: None,
            dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn new_durable(
        durable_receipt_root: std::path::PathBuf,
        legal_actions: LegalActions,
        dispatch: Dispatch,
    ) -> Self {
        let dispatch_gate = shared_durable_dispatch_gate(&durable_receipt_root);
        Self {
            legal_actions,
            dispatch,
            durable_receipt_root: Some(durable_receipt_root),
            dispatch_gate,
        }
    }

    pub(crate) async fn legal_actions(
        &self,
        reference: &DoctorRemediationRefV1,
    ) -> Vec<DashboardLegalActionKindV1> {
        let preview_available = DoctorRemediationRegistryV1::default_registry()
            .resolve(reference)
            .is_ok_and(|descriptor| descriptor.preview_available());
        (self.legal_actions)(reference.clone())
            .await
            .into_iter()
            .filter(|kind| {
                matches!(
                    (reference.kind(), *kind, preview_available),
                    (
                        DoctorRemediationKindV1::Preview,
                        DashboardLegalActionKindV1::RequestDryRun,
                        true
                    ) | (
                        DoctorRemediationKindV1::Action,
                        DashboardLegalActionKindV1::RequestApply,
                        _
                    ) | (
                        DoctorRemediationKindV1::Action,
                        DashboardLegalActionKindV1::RequestDryRun,
                        true
                    )
                )
            })
            .collect()
    }

    async fn dispatch(
        &self,
        command: DoctorRemediationDispatchCommandV1,
    ) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
        let _dispatch_guard = self.dispatch_gate.lock().await;
        if let (Some(root), DoctorRemediationDispatchCommandV1::Status { operation_id }) =
            (&self.durable_receipt_root, &command)
            && let Some(record) = read_durable_operation(root, operation_id)?
        {
            validate_outcome(record.operation.clone())?;
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
                let outcome = (self.dispatch)(recovery).await?;
                if outcome.operation_id != record.operation.operation_id {
                    return Err(DoctorRemediationDispatchErrorV1::InvalidReference);
                }
                validate_outcome(outcome.clone())?;
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
            return Ok(record.operation);
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
                return (self.dispatch)(command).await;
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
                    return Ok(record.operation);
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
                },
            };
            write_durable_record(root, &intent)?;
            if idempotency_key.is_some() {
                write_idempotency_record(root, &operation, &intent)?;
            }
        }
        let outcome = (self.dispatch)(owner_command).await?;
        validate_outcome(outcome.clone())?;
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
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(root).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    gates.insert(root.to_path_buf(), Arc::downgrade(&gate));
    gate
}

pub(crate) fn operation_id_for_command(
    command: &DoctorRemediationDispatchCommandV1,
) -> Result<OperationId, DoctorRemediationDispatchErrorV1> {
    let digest = match command {
        DoctorRemediationDispatchCommandV1::Preview { operation, target } => {
            tracedecay_domain::canonical_sha256(&(
                "tracedecay.doctor-remediation-preview-operation.v1",
                operation,
                target.digest()?,
            ))
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
        } => tracedecay_domain::canonical_sha256(&(
            "tracedecay.doctor-remediation-apply-operation.v1",
            operation,
            target.digest()?,
            idempotency_key,
        )),
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
    crate::storage::PrivateStoreIo::create_dir_all(parent)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let _lock =
        crate::storage::acquire_sidecar_lock_blocking(&crate::storage::append_lock_path(path))
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(DoctorRemediationDispatchErrorV1::InvalidReference)
        }
        Ok(_) => std::fs::read(path)
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)
            })
            .map(Some),
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
    crate::storage::PrivateStoreIo::create_dir_all(parent)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let _lock =
        crate::storage::acquire_sidecar_lock_blocking(&crate::storage::append_lock_path(path))
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let bytes = serde_json::to_vec(record)
        .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    let temp_path = path.with_extension(format!("json.tmp-{}", std::process::id()));
    crate::storage::PrivateStoreIo::write_file_atomically(path, &temp_path, &bytes)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorRemediationPreviewRequestV1 {
    operation: DoctorOwningOperationRefV1,
    target: DoctorRemediationTargetV1,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorRemediationApplyRequestV1 {
    operation: DoctorOwningOperationRefV1,
    target: DoctorRemediationTargetV1,
    preview_id: Option<PreviewId>,
    idempotency_key: IdempotencyKey,
    confirmed: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DoctorRemediationPayloadV1 {
    Operation {
        operation: DoctorRemediationOperationV1,
    },
    Unavailable {
        reason: DoctorRemediationDispatchErrorV1,
    },
}

pub(crate) async fn preview(
    State(state): State<DashboardState>,
    Json(request): Json<DoctorRemediationPreviewRequestV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    if !reference_is_registered(&request.operation, DoctorRemediationKindV1::Preview)
        || request
            .target
            .validate_for(&request.operation, DoctorRemediationKindV1::Preview)
            .is_err()
    {
        return response(
            &state,
            Err(DoctorRemediationDispatchErrorV1::InvalidReference),
        );
    }
    let Some(dispatcher) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    let reference =
        DoctorRemediationRefV1::new(request.operation.clone(), DoctorRemediationKindV1::Preview);
    if !dispatcher
        .legal_actions(&reference)
        .await
        .contains(&DashboardLegalActionKindV1::RequestDryRun)
    {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Denied));
    }
    let expected_operation = request.operation.clone();
    let result = dispatcher
        .dispatch(DoctorRemediationDispatchCommandV1::Preview {
            operation: request.operation,
            target: request.target,
        })
        .await
        .and_then(|outcome| {
            (outcome.owning_operation == expected_operation)
                .then_some(outcome)
                .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
        })
        .and_then(validate_outcome);
    response(&state, result)
}

pub(crate) async fn apply(
    State(state): State<DashboardState>,
    Json(request): Json<DoctorRemediationApplyRequestV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    if !request.confirmed {
        return response(
            &state,
            Err(DoctorRemediationDispatchErrorV1::ConfirmationRequired),
        );
    }
    if !reference_is_registered(&request.operation, DoctorRemediationKindV1::Action)
        || request
            .target
            .validate_for(&request.operation, DoctorRemediationKindV1::Action)
            .is_err()
    {
        return response(
            &state,
            Err(DoctorRemediationDispatchErrorV1::InvalidReference),
        );
    }
    let Some(dispatcher) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    let reference =
        DoctorRemediationRefV1::new(request.operation.clone(), DoctorRemediationKindV1::Action);
    if !dispatcher
        .legal_actions(&reference)
        .await
        .contains(&DashboardLegalActionKindV1::RequestApply)
    {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Denied));
    }
    let expected_operation = request.operation.clone();
    let expected_preview_id = request.preview_id.clone();
    let expected_idempotency_key = request.idempotency_key.clone();
    let result = dispatcher
        .dispatch(DoctorRemediationDispatchCommandV1::Apply {
            operation: request.operation,
            target: request.target,
            preview_id: request.preview_id,
            idempotency_key: request.idempotency_key,
        })
        .await
        .and_then(|outcome| {
            (outcome.owning_operation == expected_operation
                && outcome.preview_id == expected_preview_id
                && outcome
                    .effect_receipt
                    .as_ref()
                    .is_none_or(|receipt| receipt.idempotency_key == expected_idempotency_key))
            .then_some(outcome)
            .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
        })
        .and_then(validate_outcome);
    response(&state, result)
}

pub(crate) async fn status(
    State(state): State<DashboardState>,
    Path(operation_id): Path<String>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    let Ok(request_id) = RequestId::new(operation_id) else {
        return response(
            &state,
            Err(DoctorRemediationDispatchErrorV1::InvalidReference),
        );
    };
    let Some(dispatcher) = state.doctor_remediation_dispatcher.as_ref() else {
        return response(&state, Err(DoctorRemediationDispatchErrorV1::Unsupported));
    };
    let operation_id = OperationId::from_request(request_id);
    let result = dispatcher
        .dispatch(DoctorRemediationDispatchCommandV1::Status {
            operation_id: operation_id.clone(),
        })
        .await
        .and_then(|outcome| {
            (outcome.operation_id == operation_id)
                .then_some(outcome)
                .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)
        })
        .and_then(validate_outcome);
    response(&state, result)
}

fn reference_is_registered(
    operation: &DoctorOwningOperationRefV1,
    kind: DoctorRemediationKindV1,
) -> bool {
    DoctorRemediationRegistryV1::default_registry()
        .resolve(&DoctorRemediationRefV1::new(operation.clone(), kind))
        .is_ok()
}

fn validate_outcome(
    outcome: DoctorRemediationOperationV1,
) -> Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1> {
    let invalid_effect_binding = outcome.effect_receipt.as_ref().is_some_and(|receipt| {
        receipt.operation.as_str() != outcome.owning_operation.as_str()
            || &receipt.request_id != outcome.operation_id.request_id()
            || outcome
                .execution
                .as_ref()
                .is_none_or(|execution| execution.termination != receipt.outcome.into())
    });
    if !reference_is_registered(&outcome.owning_operation, DoctorRemediationKindV1::Action)
        || outcome
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
    if expected_termination.is_some_and(|expected| {
        outcome
            .execution
            .as_ref()
            .is_none_or(|execution| execution.termination != expected)
    }) || (outcome.phase == DoctorRemediationOperationPhaseV1::Previewed
        && outcome.preview_id.is_none())
        || (outcome.phase == DoctorRemediationOperationPhaseV1::Previewed
            && outcome.effect_receipt.is_some())
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

fn response(
    state: &DashboardState,
    result: Result<DoctorRemediationOperationV1, DoctorRemediationDispatchErrorV1>,
) -> Json<DashboardEnvelopeV1<DoctorRemediationPayloadV1>> {
    let scope = scope_from_state(state);
    match result {
        Ok(operation) => {
            let domain_state = match operation.phase {
                DoctorRemediationOperationPhaseV1::Previewed
                | DoctorRemediationOperationPhaseV1::Completed => DashboardDomainStateV1::Ready,
                DoctorRemediationOperationPhaseV1::Running
                | DoctorRemediationOperationPhaseV1::Partial => DashboardDomainStateV1::Partial,
                DoctorRemediationOperationPhaseV1::Cancelled => DashboardDomainStateV1::Cancelled,
                DoctorRemediationOperationPhaseV1::TimedOut => DashboardDomainStateV1::TimedOut,
                DoctorRemediationOperationPhaseV1::Failed
                | DoctorRemediationOperationPhaseV1::EffectUnknown => DashboardDomainStateV1::Error,
            };
            Json(DashboardEnvelopeV1::new(
                scope,
                domain_state,
                DashboardCoverageV1::complete(1, "doctor_remediation_operation"),
                DashboardFreshnessV1::fresh_now(),
                DoctorRemediationPayloadV1::Operation { operation },
            ))
        }
        Err(error) => {
            let domain_state = match error {
                DoctorRemediationDispatchErrorV1::Unsupported => {
                    DashboardDomainStateV1::Unsupported
                }
                DoctorRemediationDispatchErrorV1::Denied
                | DoctorRemediationDispatchErrorV1::ConfirmationRequired => {
                    DashboardDomainStateV1::Denied
                }
                DoctorRemediationDispatchErrorV1::InvalidReference => DashboardDomainStateV1::Error,
                DoctorRemediationDispatchErrorV1::OwnerUnavailable => {
                    DashboardDomainStateV1::Offline
                }
            };
            let coverage = if error == DoctorRemediationDispatchErrorV1::Unsupported {
                DashboardCoverageV1::unsupported()
            } else {
                DashboardCoverageV1::unknown()
            };
            let freshness = if error == DoctorRemediationDispatchErrorV1::Unsupported {
                DashboardFreshnessV1::unsupported()
            } else {
                DashboardFreshnessV1::unknown()
            };
            Json(DashboardEnvelopeV1::new(
                scope,
                domain_state,
                coverage,
                freshness,
                DoctorRemediationPayloadV1::Unavailable { reason: error },
            ))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_application::{Deadline, OperationBudgetUsage, OperationTermination};
    use tracedecay_domain::UtcMicros;

    use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

    async fn initialize_project(project_root: &std::path::Path) -> TraceDecay {
        let profile_root = crate::config::user_data_dir().expect("isolated profile root");
        std::fs::create_dir_all(&profile_root).expect("create isolated profile root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&profile_root, std::fs::Permissions::from_mode(0o700))
                .expect("secure isolated profile root");
        }
        let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
            &profile_root,
            "doctor remediation fixture initialization",
        )
        .expect("fixture lifecycle authority");
        let _database_scope = crate::db::enter_maintenance_database_scope(
            &lifecycle,
            &profile_root,
            "doctor remediation fixture initialization",
        )
        .expect("fixture maintenance database scope");
        TraceDecay::init_with_exclusive_maintenance(
            project_root,
            TraceDecayOpenOptions {
                profile_root: Some(profile_root),
                global_db_path: None,
            },
            &lifecycle,
        )
        .await
        .expect("project init")
    }

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = initialize_project(project.path()).await;
        let state = crate::dashboard::build_state(&cg)
            .await
            .expect("dashboard state");
        (project, state)
    }

    fn configuration_operation() -> DoctorOwningOperationRefV1 {
        DoctorOwningOperationRefV1::new(
            tracedecay_application::doctor::operations::CONFIGURATION_PROTECTED_APPLY,
        )
        .unwrap()
    }

    fn runtime_recovery_operation() -> DoctorOwningOperationRefV1 {
        DoctorOwningOperationRefV1::new(
            tracedecay_application::doctor::operations::RUNTIME_RECOVER_DAEMON,
        )
        .unwrap()
    }

    fn configuration_preview_target() -> DoctorRemediationTargetV1 {
        DoctorRemediationTargetV1::ConfigurationProtectedPreview(
            ConfigurationProtectedPreviewSurfaceRequest {
                change:
                    tracedecay_domain::configuration::ProtectedChange::ReplaceWorkTopologyPolicy(
                        tracedecay_domain::configuration::safe_work_topology_policy_v1(),
                    ),
                expected_revision: tracedecay_domain::configuration::ConfigurationRevisionId::new(
                    "configuration-revision.doctor-preview",
                )
                .unwrap(),
            },
        )
    }

    fn configuration_apply_target() -> DoctorRemediationTargetV1 {
        DoctorRemediationTargetV1::ConfigurationProtectedApply(
            ConfigurationProtectedApplySurfaceRequest {
                plan_id: tracedecay_domain::configuration::ChangePlanId::new(
                    "change-plan.doctor-apply",
                )
                .unwrap(),
                expected_base_revision_id:
                    tracedecay_domain::configuration::ConfigurationRevisionId::new(
                        "configuration-revision.doctor-apply",
                    )
                    .unwrap(),
                operation_digest: ManifestDigest::new(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .unwrap(),
                idempotency_key:
                    tracedecay_domain::configuration::ConfigurationIdempotencyKey::new(
                        "configuration-idempotency.doctor-apply",
                    )
                    .unwrap(),
            },
        )
    }

    #[test]
    fn typed_target_rejects_a_different_registered_operation() {
        let target = DoctorRemediationTargetV1::StorageRetentionCollect;

        assert!(
            target
                .validate_for(
                    &DoctorOwningOperationRefV1::new(
                        tracedecay_application::doctor::operations::STORAGE_RETENTION_COLLECT,
                    )
                    .unwrap(),
                    DoctorRemediationKindV1::Action,
                )
                .is_ok()
        );
        assert_eq!(
            target.validate_for(&configuration_operation(), DoctorRemediationKindV1::Action),
            Err(DoctorRemediationDispatchErrorV1::InvalidReference)
        );
    }

    fn failed_operation() -> DoctorRemediationOperationV1 {
        DoctorRemediationOperationV1 {
            operation_id: OperationId::from_request(
                RequestId::new("request.doctor-remediation-failed").unwrap(),
            ),
            owning_operation: configuration_operation(),
            phase: DoctorRemediationOperationPhaseV1::Failed,
            preview_id: Some(PreviewId::new("preview.doctor-remediation-failed").unwrap()),
            execution: Some(OperationReceipt {
                started_at: UtcMicros(1),
                ended_at: UtcMicros(2),
                effective_deadline: Deadline::new(UtcMicros(10)).unwrap(),
                cancellation: None,
                budget: OperationBudgetUsage::default(),
                termination: OperationTermination::Failed,
            }),
            effect_receipt: None,
            owner_effect_receipt: None,
            owner_result_digest: None,
        }
    }

    #[tokio::test]
    async fn preview_is_typed_unsupported_without_an_admitted_dispatcher() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;

        let Json(envelope) = preview(
            State(state),
            Json(DoctorRemediationPreviewRequestV1 {
                operation: configuration_operation(),
                target: configuration_preview_target(),
            }),
        )
        .await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Unsupported);
        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Unavailable {
                reason: DoctorRemediationDispatchErrorV1::Unsupported
            }
        );
    }

    #[tokio::test]
    async fn denied_owner_apply_stays_denied() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.doctor_remediation_dispatcher = Some(DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new(|_| Box::pin(async { Err(DoctorRemediationDispatchErrorV1::Denied) })),
        ));

        let Json(envelope) = apply(
            State(state),
            Json(DoctorRemediationApplyRequestV1 {
                operation: configuration_operation(),
                target: configuration_apply_target(),
                preview_id: Some(PreviewId::new("preview.denied").unwrap()),
                idempotency_key: IdempotencyKey::new("idempotency.denied").unwrap(),
                confirmed: true,
            }),
        )
        .await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Denied);
        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Unavailable {
                reason: DoctorRemediationDispatchErrorV1::Denied
            }
        );
    }

    #[tokio::test]
    async fn apply_rejects_an_owner_outcome_for_a_different_preview() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        state.doctor_remediation_dispatcher = Some(DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new(|_| Box::pin(async { Ok(failed_operation()) })),
        ));

        let Json(envelope) = apply(
            State(state),
            Json(DoctorRemediationApplyRequestV1 {
                operation: configuration_operation(),
                target: configuration_apply_target(),
                preview_id: Some(PreviewId::new("preview.different").unwrap()),
                idempotency_key: IdempotencyKey::new("idempotency.different").unwrap(),
                confirmed: true,
            }),
        )
        .await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Error);
        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Unavailable {
                reason: DoctorRemediationDispatchErrorV1::InvalidReference
            }
        );
    }

    #[tokio::test]
    async fn legal_actions_reject_dry_run_when_registry_has_no_preview() {
        let dispatcher = DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new(|_| Box::pin(async { Err(DoctorRemediationDispatchErrorV1::Denied) })),
        );
        let reference = DoctorRemediationRefV1::new(
            DoctorOwningOperationRefV1::new(
                tracedecay_application::doctor::operations::RUNTIME_RECOVER_DAEMON,
            )
            .unwrap(),
            DoctorRemediationKindV1::Action,
        );

        assert!(dispatcher.legal_actions(&reference).await.is_empty());
    }

    #[tokio::test]
    async fn apply_delegates_an_operation_that_has_no_preview() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, mut state) = state_for_test().await;
        let mut failed = failed_operation();
        failed.owning_operation = runtime_recovery_operation();
        failed.preview_id = None;
        state.doctor_remediation_dispatcher = Some(DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new({
                let failed = failed.clone();
                move |_| {
                    let failed = failed.clone();
                    Box::pin(async move { Ok(failed) })
                }
            }),
        ));

        let Json(envelope) = apply(
            State(state),
            Json(DoctorRemediationApplyRequestV1 {
                operation: runtime_recovery_operation(),
                target: DoctorRemediationTargetV1::RuntimeRecoverDaemon,
                preview_id: None,
                idempotency_key: IdempotencyKey::new("idempotency.runtime-recovery").unwrap(),
                confirmed: true,
            }),
        )
        .await;

        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Operation { operation: failed }
        );
    }

    #[tokio::test]
    async fn apply_requires_explicit_confirmation_before_owner_dispatch() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;

        let Json(envelope) = apply(
            State(state),
            Json(DoctorRemediationApplyRequestV1 {
                operation: configuration_operation(),
                target: configuration_apply_target(),
                preview_id: Some(PreviewId::new("preview.unconfirmed").unwrap()),
                idempotency_key: IdempotencyKey::new("idempotency.unconfirmed").unwrap(),
                confirmed: false,
            }),
        )
        .await;

        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Denied);
        assert_eq!(
            envelope.payload,
            DoctorRemediationPayloadV1::Unavailable {
                reason: DoctorRemediationDispatchErrorV1::ConfirmationRequired
            }
        );
    }

    #[tokio::test]
    async fn failed_receipt_is_resumed_through_same_owner_after_dashboard_rebuild() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let failed = failed_operation();
        let dispatcher = DoctorRemediationDispatcherV1::new(
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new({
                let failed = failed.clone();
                move |_| {
                    let failed = failed.clone();
                    Box::pin(async move { Ok(failed) })
                }
            }),
        );
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = initialize_project(project.path()).await;
        let mut first_state = crate::dashboard::build_state(&cg)
            .await
            .expect("first dashboard state");
        first_state.doctor_remediation_dispatcher = Some(dispatcher.clone());

        let Json(first) = preview(
            State(first_state),
            Json(DoctorRemediationPreviewRequestV1 {
                operation: configuration_operation(),
                target: configuration_preview_target(),
            }),
        )
        .await;
        assert_eq!(first.domain_state, DashboardDomainStateV1::Error);

        let operation_id = failed.operation_id.to_string();
        let mut rebuilt_state = crate::dashboard::build_state(&cg)
            .await
            .expect("rebuilt dashboard state");
        rebuilt_state.doctor_remediation_dispatcher = Some(dispatcher);
        let Json(resumed) = status(State(rebuilt_state), Path(operation_id)).await;

        assert_eq!(
            resumed.payload,
            DoctorRemediationPayloadV1::Operation { operation: failed }
        );
        assert_eq!(resumed.domain_state, DashboardDomainStateV1::Error);
    }

    #[tokio::test]
    async fn durable_dispatcher_resumes_terminal_receipt_after_rebuild() {
        let root = tempfile::tempdir().expect("receipt root");
        let command = DoctorRemediationDispatchCommandV1::Preview {
            operation: configuration_operation(),
            target: configuration_preview_target(),
        };
        let mut failed = failed_operation();
        failed.operation_id = operation_id_for_command(&command).unwrap();
        let first = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new({
                let failed = failed.clone();
                move |_| {
                    let failed = failed.clone();
                    Box::pin(async move { Ok(failed) })
                }
            }),
        );
        first
            .dispatch(command)
            .await
            .expect("persist terminal operation");

        let rebuilt = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new(|_| {
                Box::pin(async { Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable) })
            }),
        );
        let resumed = rebuilt
            .dispatch(DoctorRemediationDispatchCommandV1::Status {
                operation_id: failed.operation_id.clone(),
            })
            .await
            .expect("resume durable terminal operation");

        assert_eq!(resumed, failed);
    }

    #[tokio::test]
    async fn durable_apply_reuses_idempotent_owner_result_without_redispatch() {
        let root = tempfile::tempdir().expect("receipt root");
        let command = DoctorRemediationDispatchCommandV1::Apply {
            operation: configuration_operation(),
            target: configuration_apply_target(),
            preview_id: Some(PreviewId::new("preview.durable-idempotency").unwrap()),
            idempotency_key: IdempotencyKey::new("idempotency.durable-apply").unwrap(),
        };
        let mut failed = failed_operation();
        failed.operation_id = operation_id_for_command(&command).unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let legal: LegalActions =
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] }));
        let owner: Dispatch = Arc::new({
            let calls = Arc::clone(&calls);
            let failed = failed.clone();
            move |_| {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let failed = failed.clone();
                Box::pin(async move { Ok(failed) })
            }
        });
        let first_dispatcher = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::clone(&legal),
            Arc::clone(&owner),
        );
        let second_dispatcher =
            DoctorRemediationDispatcherV1::new_durable(root.path().to_path_buf(), legal, owner);

        let (first, second) = tokio::join!(
            first_dispatcher.dispatch(command.clone()),
            second_dispatcher.dispatch(command)
        );
        let first = first.unwrap();
        let second = second.unwrap();

        assert_eq!(first, failed);
        assert_eq!(second, failed);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn durable_terminal_status_survives_owner_unmount() {
        let root = tempfile::tempdir().expect("receipt root");
        let command = DoctorRemediationDispatchCommandV1::Preview {
            operation: configuration_operation(),
            target: configuration_preview_target(),
        };
        let mut failed = failed_operation();
        failed.operation_id = operation_id_for_command(&command).unwrap();
        DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestDryRun] })),
            Arc::new({
                let failed = failed.clone();
                move |_| {
                    let failed = failed.clone();
                    Box::pin(async move { Ok(failed) })
                }
            }),
        )
        .dispatch(command)
        .await
        .unwrap();
        let rebuilt = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { Vec::new() })),
            Arc::new(|_| {
                Box::pin(async { Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable) })
            }),
        );

        let operation_id = failed.operation_id.clone();
        assert_eq!(
            rebuilt
                .dispatch(DoctorRemediationDispatchCommandV1::Status { operation_id })
                .await
                .unwrap(),
            failed
        );
    }

    #[tokio::test]
    async fn durable_status_recovers_a_running_idempotent_owner_command() {
        let root = tempfile::tempdir().expect("receipt root");
        let command = DoctorRemediationDispatchCommandV1::Apply {
            operation: configuration_operation(),
            target: configuration_apply_target(),
            preview_id: Some(PreviewId::new("preview.running-recovery").unwrap()),
            idempotency_key: IdempotencyKey::new("idempotency.running-recovery").unwrap(),
        };
        let operation_id = operation_id_for_command(&command).unwrap();
        let first = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new(|_| {
                Box::pin(async { Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable) })
            }),
        );
        assert_eq!(
            first.dispatch(command).await,
            Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable)
        );
        let mut recovered = failed_operation();
        recovered.operation_id = operation_id.clone();
        recovered.preview_id = Some(PreviewId::new("preview.running-recovery").unwrap());
        let rebuilt = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new({
                let recovered = recovered.clone();
                move |command| {
                    assert!(matches!(
                        command,
                        DoctorRemediationDispatchCommandV1::Resume { .. }
                    ));
                    let recovered = recovered.clone();
                    Box::pin(async move { Ok(recovered) })
                }
            }),
        );

        assert_eq!(
            rebuilt
                .dispatch(DoctorRemediationDispatchCommandV1::Status { operation_id })
                .await
                .unwrap(),
            recovered
        );
    }

    #[tokio::test]
    async fn durable_apply_retry_resumes_instead_of_replaying_fresh_apply() {
        let root = tempfile::tempdir().expect("receipt root");
        let command = DoctorRemediationDispatchCommandV1::Apply {
            operation: configuration_operation(),
            target: configuration_apply_target(),
            preview_id: Some(PreviewId::new("preview.apply-recovery").unwrap()),
            idempotency_key: IdempotencyKey::new("idempotency.apply-recovery").unwrap(),
        };
        let mut recovered = failed_operation();
        recovered.operation_id = operation_id_for_command(&command).unwrap();
        recovered.preview_id = Some(PreviewId::new("preview.apply-recovery").unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let dispatcher = DoctorRemediationDispatcherV1::new_durable(
            root.path().to_path_buf(),
            Arc::new(|_| Box::pin(async { vec![DashboardLegalActionKindV1::RequestApply] })),
            Arc::new({
                let calls = Arc::clone(&calls);
                let recovered = recovered.clone();
                move |owner_command| {
                    let call = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let recovered = recovered.clone();
                    Box::pin(async move {
                        if call == 0 {
                            return Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable);
                        }
                        assert!(matches!(
                            owner_command,
                            DoctorRemediationDispatchCommandV1::Resume { .. }
                        ));
                        Ok(recovered)
                    })
                }
            }),
        );

        assert_eq!(
            dispatcher.dispatch(command.clone()).await,
            Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable)
        );
        assert_eq!(dispatcher.dispatch(command).await.unwrap(), recovered);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }
}

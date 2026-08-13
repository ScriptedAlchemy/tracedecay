//! OS-locked durable reservation and terminal replay for memory automation.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    CancellationSignal, CapabilityGrantId, DirectorySyncPolicy, DisclosureClass, EffectReceipt,
    RequestId, ResolvedScope,
    retained_surfaces::{AutomationRunRequestV1, AutomationTaskV1},
};
use tracedecay_domain::{ActorId, FactOwnerV1, ManifestDigest};

use super::retirement::RetirementBinding;
use super::{AutomationSettledProblem, AutomationSettledTerminal, contract_error};
use crate::errors::Result;

const MAX_AUTOMATION_JOURNAL_BYTES: u64 = 512 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct DurableAutomationAdmission {
    pub(super) schema_version: u32,
    pub(super) request: AutomationRunRequestV1,
    pub(super) input_digest: ManifestDigest,
    pub(super) configuration_digest: ManifestDigest,
    /// Exact registered grant/catalog/privacy authority used to prepare the
    /// outer retained effect. Restart recovery must reproduce this binding;
    /// a newly registered grant cannot silently inherit an older admission.
    pub(super) effect_authority_digest: ManifestDigest,
    pub(super) grant_id: CapabilityGrantId,
    pub(super) grant_revision: u64,
    pub(super) grant_digest: ManifestDigest,
    pub(super) disclosure: DisclosureClass,
    /// Exact prepared outer-effect receipt material. Recovery changes only
    /// its committed-state digest; it never mints a new grant or request.
    pub(super) effect_receipt_template: EffectReceipt,
    pub(super) actor: ActorId,
    pub(super) scope: ResolvedScope,
    pub(super) request_id: RequestId,
    pub(super) process_run_id: String,
    pub(super) recovery: AutomationRecoveryBinding,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "binding",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum AutomationRecoveryBinding {
    Memory {
        owner: FactOwnerV1,
        recovery_problem: AutomationSettledProblem,
        retirement: Option<RetirementBinding>,
        reset_source_digest: Option<String>,
    },
    /// External effects have no canonical destination-side receipt store.
    /// Restart recovery must close them with this typed indeterminate problem
    /// and must never repeat the delivery or skill mutation.
    External {
        recovery_problem: AutomationSettledProblem,
    },
}

impl DurableAutomationAdmission {
    pub(super) fn recovery_problem(&self) -> &AutomationSettledProblem {
        match &self.recovery {
            AutomationRecoveryBinding::Memory {
                recovery_problem, ..
            }
            | AutomationRecoveryBinding::External { recovery_problem } => recovery_problem,
        }
    }

    pub(super) fn memory_owner(&self) -> Option<&FactOwnerV1> {
        match &self.recovery {
            AutomationRecoveryBinding::Memory { owner, .. } => Some(owner),
            AutomationRecoveryBinding::External { .. } => None,
        }
    }

    pub(super) fn retirement(&self) -> Option<&RetirementBinding> {
        match &self.recovery {
            AutomationRecoveryBinding::Memory { retirement, .. } => retirement.as_ref(),
            AutomationRecoveryBinding::External { .. } => None,
        }
    }

    pub(super) fn reset_source_digest(&self) -> Option<&str> {
        match &self.recovery {
            AutomationRecoveryBinding::Memory {
                reset_source_digest,
                ..
            } => reset_source_digest.as_deref(),
            AutomationRecoveryBinding::External { .. } => None,
        }
    }

    pub(super) fn is_external(&self) -> bool {
        matches!(self.recovery, AutomationRecoveryBinding::External { .. })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "state",
    content = "terminal",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum DurableAutomationState {
    Reserved,
    Terminal(AutomationSettledTerminal),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct DurableAutomationRecord {
    admission: DurableAutomationAdmission,
    state: DurableAutomationState,
}

impl DurableAutomationRecord {
    pub(super) fn admission(&self) -> &DurableAutomationAdmission {
        &self.admission
    }

    pub(super) fn terminal(&self) -> Option<&AutomationSettledTerminal> {
        match &self.state {
            DurableAutomationState::Reserved => None,
            DurableAutomationState::Terminal(terminal) => Some(terminal),
        }
    }
}

pub(super) enum ReservationResult {
    Execute {
        retirement: Option<RetirementBinding>,
    },
    Replay {
        terminal: AutomationSettledTerminal,
        retirement: Option<RetirementBinding>,
    },
    /// A prior process durably reserved this exact admission but did not
    /// publish its outer terminal. The caller must reconcile against the
    /// canonical memory receipt authority before this reservation can close.
    Recover {
        retirement: Option<RetirementBinding>,
    },
    /// The run identity already has a valid durable record, but the newly
    /// prepared admission does not match the authority bound to that record.
    /// This is an idempotency conflict, not a journal I/O or shape failure.
    Conflict { terminal: bool },
}

pub(super) fn retained_source_bindings(
    path: &Path,
) -> Result<(Option<RetirementBinding>, Option<String>)> {
    with_journal_lock(path, || {
        Ok(read_record(path)?
            .map(|record| {
                (
                    record.admission.retirement().cloned(),
                    record.admission.reset_source_digest().map(str::to_owned),
                )
            })
            .unwrap_or_default())
    })
}

pub(super) fn reserve_or_replay_blocking(
    path: &Path,
    requested: DurableAutomationAdmission,
) -> Result<ReservationResult> {
    with_journal_lock(path, || {
        let existing = read_record(path)?;
        match existing {
            None => {
                let retirement = requested.retirement().cloned();
                write_record(
                    path,
                    &DurableAutomationRecord {
                        admission: requested,
                        state: DurableAutomationState::Reserved,
                    },
                )?;
                Ok(ReservationResult::Execute { retirement })
            }
            Some(record) => {
                if !stable_admission_matches(&record.admission, &requested) {
                    return Ok(ReservationResult::Conflict {
                        terminal: matches!(record.state, DurableAutomationState::Terminal(_)),
                    });
                }
                match &record.state {
                    DurableAutomationState::Terminal(terminal) => Ok(ReservationResult::Replay {
                        terminal: terminal.clone(),
                        retirement: record.admission.retirement().cloned(),
                    }),
                    DurableAutomationState::Reserved
                        if record.admission.process_run_id == requested.process_run_id =>
                    {
                        Err(contract_error(
                            "an identical memory automation run is already in flight",
                        ))
                    }
                    DurableAutomationState::Reserved => Ok(ReservationResult::Recover {
                        retirement: record.admission.retirement().cloned(),
                    }),
                }
            }
        }
    })
}

pub(super) fn read_indexed_record_blocking(path: &Path) -> Result<Option<DurableAutomationRecord>> {
    with_journal_lock(path, || read_record(path))
}

/// Persists the terminal produced by canonical receipt reconciliation for a
/// reservation owned by a prior process. This is the only path allowed to
/// close a foreign reservation, and it retains the original admission bytes.
pub(super) fn persist_recovered_terminal_blocking(
    path: &Path,
    requested: &DurableAutomationAdmission,
    terminal: AutomationSettledTerminal,
    cancellation: Option<&CancellationSignal>,
) -> Result<Option<AutomationSettledTerminal>> {
    with_journal_lock(path, || {
        if cancellation.is_some_and(CancellationSignal::is_cancelled) {
            return Ok(None);
        }
        if !terminal.matches_admission(requested) {
            return Err(contract_error(
                "recovered memory automation terminal does not match its durable admission",
            ));
        }
        let mut record = read_record(path)?.ok_or_else(|| {
            contract_error("memory automation recovery has no durable reservation")
        })?;
        validate_stable_admission(&record.admission, requested)?;
        match &record.state {
            DurableAutomationState::Terminal(stored) if stored == &terminal => {
                return Ok(Some(stored.clone()));
            }
            DurableAutomationState::Terminal(_) => {
                return Err(contract_error(
                    "recovered memory automation terminal conflicts with durable replay",
                ));
            }
            DurableAutomationState::Reserved
                if record.admission.process_run_id == requested.process_run_id =>
            {
                return Err(contract_error(
                    "the current process cannot use restart recovery for its live reservation",
                ));
            }
            DurableAutomationState::Reserved => {}
        }
        record.state = DurableAutomationState::Terminal(terminal.clone());
        write_record(path, &record)?;
        let stored = read_record(path)?.ok_or_else(|| {
            contract_error("recovered memory automation terminal disappeared after write")
        })?;
        validate_stable_admission(&stored.admission, requested)?;
        match stored.state {
            DurableAutomationState::Terminal(stored) if stored == terminal => Ok(Some(stored)),
            _ => Err(contract_error(
                "recovered memory automation terminal did not replay byte-identically",
            )),
        }
    })
}

pub(super) fn persist_terminal_blocking(
    path: &Path,
    requested: &DurableAutomationAdmission,
    terminal: AutomationSettledTerminal,
) -> Result<AutomationSettledTerminal> {
    with_journal_lock(path, || {
        if !terminal.matches_admission(requested) {
            return Err(contract_error(
                "memory automation terminal does not match its durable admission",
            ));
        }
        let mut record = read_record(path)?.ok_or_else(|| {
            contract_error("memory automation terminal has no durable reservation")
        })?;
        validate_stable_admission(&record.admission, requested)?;
        match &record.state {
            DurableAutomationState::Terminal(stored) if stored == &terminal => {
                return Ok(stored.clone());
            }
            DurableAutomationState::Terminal(_) => {
                return Err(contract_error(
                    "memory automation terminal conflicts with its durable replay",
                ));
            }
            DurableAutomationState::Reserved
                if record.admission.process_run_id != requested.process_run_id =>
            {
                return Err(contract_error(
                    "memory automation reservation belongs to another process run",
                ));
            }
            DurableAutomationState::Reserved => {}
        }
        record.state = DurableAutomationState::Terminal(terminal.clone());
        write_record(path, &record)?;
        let stored = read_record(path)?.ok_or_else(|| {
            contract_error("memory automation terminal disappeared after durable write")
        })?;
        validate_stable_admission(&stored.admission, requested)?;
        match stored.state {
            DurableAutomationState::Terminal(stored) if stored == terminal => Ok(stored),
            _ => Err(contract_error(
                "memory automation terminal replay does not match the durable write",
            )),
        }
    })
}

pub(super) fn abandon_reservation_blocking(
    path: &Path,
    requested: &DurableAutomationAdmission,
) -> Result<()> {
    with_journal_lock(path, || {
        let record = read_record(path)?.ok_or_else(|| {
            contract_error("memory automation reservation disappeared before rollback")
        })?;
        validate_stable_admission(&record.admission, requested)?;
        if !matches!(record.state, DurableAutomationState::Reserved)
            || record.admission.process_run_id != requested.process_run_id
        {
            return Err(contract_error(
                "only the owning uncommitted memory automation reservation can roll back",
            ));
        }
        std::fs::remove_file(path).map_err(|error| {
            contract_error(format!(
                "memory automation reservation rollback failed: {error}"
            ))
        })?;
        tracedecay_application::sync_parent_directory(path, DirectorySyncPolicy::Strict).map_err(
            |error| {
                contract_error(format!(
                    "memory automation reservation rollback directory sync failed: {error}"
                ))
            },
        )
    })
}

#[cfg(test)]
#[path = "journal/tests.rs"]
mod tests;

fn with_journal_lock<T>(path: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let parent = path
        .parent()
        .ok_or_else(|| contract_error("automation terminal path has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        contract_error(format!(
            "automation terminal directory creation failed: {error}"
        ))
    })?;
    let lock_path = crate::storage::append_lock_path(path);
    let lock = crate::storage::acquire_sidecar_lock_blocking(&lock_path)
        .map_err(|error| contract_error(format!("automation terminal lock failed: {error}")))?;
    let result = operation();
    let unlock = fs2::FileExt::unlock(&lock)
        .map_err(|error| contract_error(format!("automation terminal unlock failed: {error}")));
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn read_record(path: &Path) -> Result<Option<DurableAutomationRecord>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(contract_error(format!(
                "automation terminal metadata read failed: {error}"
            )));
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_AUTOMATION_JOURNAL_BYTES
    {
        return Err(contract_error(
            "automation terminal is not a bounded regular file",
        ));
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            let record = serde_json::from_slice::<DurableAutomationRecord>(&bytes)
                .map_err(contract_error)?;
            if record.admission.schema_version != 1 || !record.admission.request.validate() {
                return Err(contract_error(
                    "memory automation durable admission has an unsupported shape",
                ));
            }
            let memory_task = matches!(
                record.admission.request.task_kind(),
                AutomationTaskV1::MemoryCurator | AutomationTaskV1::SessionReflector
            );
            if memory_task == record.admission.is_external() {
                return Err(contract_error(
                    "automation recovery binding does not match the admitted task",
                ));
            }
            record.admission.scope.validate().map_err(contract_error)?;
            if let Some(owner) = record.admission.memory_owner() {
                owner.validate().map_err(contract_error)?;
            }
            record.admission.actor.validate().map_err(contract_error)?;
            record
                .admission
                .effect_authority_digest
                .validate()
                .map_err(contract_error)?;
            record
                .admission
                .grant_digest
                .validate()
                .map_err(contract_error)?;
            record
                .admission
                .effect_receipt_template
                .validate()
                .map_err(contract_error)?;
            if !record
                .admission
                .recovery_problem()
                .matches_terminal(&record.admission.request_id)
            {
                return Err(contract_error(
                    "memory automation recovery problem is inconsistent",
                ));
            }
            let expected_owner = FactOwnerV1::Project {
                project_id: record.admission.scope.project_id.clone(),
            };
            let template = &record.admission.effect_receipt_template;
            if record
                .admission
                .memory_owner()
                .is_some_and(|owner| owner != &expected_owner)
                || template.request_id != record.admission.request_id
                || template.actor != record.admission.actor
                || template.scope != record.admission.scope
                || template.configuration_digest != record.admission.configuration_digest
                || template.policy_digest != record.admission.grant_digest
                || record.admission.grant_revision == 0
                || template.outcome != tracedecay_application::EffectTermination::Partial
                || template.committed_state.is_some()
            {
                return Err(contract_error(
                    "memory automation prepared effect binding is inconsistent",
                ));
            }
            if let DurableAutomationState::Terminal(terminal) = &record.state
                && !terminal.matches_admission(&record.admission)
            {
                return Err(contract_error(
                    "memory automation durable terminal is inconsistent",
                ));
            }
            Ok(Some(record))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(contract_error(format!(
            "automation terminal read failed: {error}"
        ))),
    }
}

fn write_record(path: &Path, record: &DurableAutomationRecord) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(record).map_err(contract_error)?;
    tracedecay_application::atomic_write(
        path,
        "automation-run-terminal",
        &bytes,
        DirectorySyncPolicy::Strict,
    )
    .map_err(|error| contract_error(format!("automation terminal write failed: {error}")))
}

fn validate_stable_admission(
    stored: &DurableAutomationAdmission,
    requested: &DurableAutomationAdmission,
) -> Result<()> {
    if !stable_admission_matches(stored, requested) {
        return Err(contract_error(
            "memory automation replay conflicts with the persisted admission",
        ));
    }
    Ok(())
}

fn stable_admission_matches(
    stored: &DurableAutomationAdmission,
    requested: &DurableAutomationAdmission,
) -> bool {
    stored.schema_version == 1
        && stored.request == requested.request
        && stored.input_digest == requested.input_digest
        && stored.configuration_digest == requested.configuration_digest
        && stored.effect_authority_digest == requested.effect_authority_digest
        && stored.grant_id == requested.grant_id
        && stored.grant_revision == requested.grant_revision
        && stored.grant_digest == requested.grant_digest
        && stored.disclosure == requested.disclosure
        && stored.effect_receipt_template == requested.effect_receipt_template
        && stored.actor == requested.actor
        && stored.scope == requested.scope
        && stored.request_id == requested.request_id
        && stored.recovery == requested.recovery
}

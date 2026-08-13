//! OS-locked durable reservation and terminal replay for memory automation.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, ambient_authority};
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_agent_hosts::automation::run_ledger::ExactRunPublication;
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
const MAX_AUTOMATION_TERMINAL_BYTES: u64 = 64 * 1024 * 1024;

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
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum DurableAutomationState {
    Reserved,
    Prepared {
        terminal: DurableAutomationTerminalBinding,
        publication: ExactRunPublication,
    },
    Terminal {
        terminal: DurableAutomationTerminalBinding,
        publication: Option<ExactRunPublication>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DurableAutomationTerminalBinding {
    schema_version: u32,
    digest: ManifestDigest,
    payload_len: u64,
}

impl DurableAutomationTerminalBinding {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.payload_len == 0
            || self.payload_len > MAX_AUTOMATION_TERMINAL_BYTES
        {
            return Err(contract_error(
                "automation terminal sidecar has an unsupported size or schema",
            ));
        }
        self.digest.validate().map_err(contract_error)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct DurableAutomationRecord {
    admission: DurableAutomationAdmission,
    state: DurableAutomationState,
    #[serde(skip)]
    legacy_terminal: Option<AutomationSettledTerminal>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentDurableAutomationRecord {
    admission: DurableAutomationAdmission,
    state: DurableAutomationState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDurableAutomationRecord {
    admission: DurableAutomationAdmission,
    state: LegacyDurableAutomationState,
}

#[derive(Deserialize)]
#[serde(
    tag = "state",
    content = "terminal",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum LegacyDurableAutomationState {
    Reserved,
    Terminal(Box<AutomationSettledTerminal>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DurableAutomationRecordWire {
    Current(CurrentDurableAutomationRecord),
    Legacy(LegacyDurableAutomationRecord),
}

impl<'de> Deserialize<'de> for DurableAutomationRecord {
    fn deserialize<Deserializer>(
        deserializer: Deserializer,
    ) -> std::result::Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        match DurableAutomationRecordWire::deserialize(deserializer)? {
            DurableAutomationRecordWire::Current(record) => Ok(Self {
                admission: record.admission,
                state: record.state,
                legacy_terminal: None,
            }),
            DurableAutomationRecordWire::Legacy(record) => match record.state {
                LegacyDurableAutomationState::Reserved => Ok(Self {
                    admission: record.admission,
                    state: DurableAutomationState::Reserved,
                    legacy_terminal: None,
                }),
                LegacyDurableAutomationState::Terminal(terminal) => Ok(Self {
                    admission: record.admission,
                    state: DurableAutomationState::Reserved,
                    legacy_terminal: Some(*terminal),
                }),
            },
        }
    }
}

impl DurableAutomationRecord {
    pub(super) fn admission(&self) -> &DurableAutomationAdmission {
        &self.admission
    }

    pub(super) fn is_terminal(&self) -> bool {
        matches!(self.state, DurableAutomationState::Terminal { .. })
    }

    pub(super) fn prepared(&self) -> Option<&ExactRunPublication> {
        match &self.state {
            DurableAutomationState::Prepared { publication, .. } => Some(publication),
            DurableAutomationState::Reserved | DurableAutomationState::Terminal { .. } => None,
        }
    }

    pub(super) fn publication(&self) -> Option<&ExactRunPublication> {
        match &self.state {
            DurableAutomationState::Prepared { publication, .. } => Some(publication),
            DurableAutomationState::Terminal { publication, .. } => publication.as_ref(),
            DurableAutomationState::Reserved => None,
        }
    }
}

pub(super) enum ReservationResult {
    Execute {
        claim: AutomationReservationClaim,
        retirement: Option<RetirementBinding>,
    },
    Replay {
        terminal: AutomationSettledTerminal,
        publication: Option<ExactRunPublication>,
        retirement: Option<RetirementBinding>,
    },
    /// A prior process durably reserved this exact admission but did not
    /// publish its outer terminal. The caller must reconcile against the
    /// canonical memory receipt authority before this reservation can close.
    Recover {
        retirement: Option<RetirementBinding>,
    },
    /// A terminal was accepted and its exact ledger row was durably staged,
    /// but publication did not finish before the prior process stopped.
    RecoverPrepared {
        terminal: AutomationSettledTerminal,
        publication: ExactRunPublication,
        retirement: Option<RetirementBinding>,
    },
    /// The run identity already has a valid durable record, but the newly
    /// prepared admission does not match the authority bound to that record.
    /// This is an idempotency conflict, not a journal I/O or shape failure.
    Conflict { terminal: bool },
}

/// Process-local proof that an `Execute` admission still has a live owner.
///
/// The durable journal remains the crash authority. This token only
/// distinguishes a genuinely live same-process `Reserved` record from one
/// whose future was dropped before it could persist a terminal. The weak
/// registry entry makes dropping the authority release ownership without an
/// async cleanup path or a second durable lease authority.
pub(super) struct AutomationReservationClaim {
    path: PathBuf,
    token: Arc<()>,
}

impl Drop for AutomationReservationClaim {
    fn drop(&mut self) {
        let mut claims = reservation_claims_guard();
        if claims
            .get(&self.path)
            .is_some_and(|registered| Weak::ptr_eq(registered, &Arc::downgrade(&self.token)))
        {
            claims.remove(&self.path);
        }
    }
}

fn reservation_claims() -> &'static Mutex<HashMap<PathBuf, Weak<()>>> {
    static CLAIMS: OnceLock<Mutex<HashMap<PathBuf, Weak<()>>>> = OnceLock::new();
    CLAIMS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn reservation_claims_guard() -> MutexGuard<'static, HashMap<PathBuf, Weak<()>>> {
    match reservation_claims().lock() {
        Ok(claims) => claims,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn acquire_reservation_claim(path: &Path) -> Result<AutomationReservationClaim> {
    let mut claims = reservation_claims_guard();
    if claims.get(path).and_then(Weak::upgrade).is_some() {
        return Err(contract_error(
            "an identical memory automation run is already in flight",
        ));
    }
    let token = Arc::new(());
    claims.insert(path.to_path_buf(), Arc::downgrade(&token));
    Ok(AutomationReservationClaim {
        path: path.to_path_buf(),
        token,
    })
}

fn reservation_claim_is_live(path: &Path) -> bool {
    reservation_claims_guard()
        .get(path)
        .and_then(Weak::upgrade)
        .is_some()
}

pub(super) fn retained_source_bindings(
    path: &Path,
) -> Result<(Option<RetirementBinding>, Option<String>)> {
    with_journal_lock(path, || {
        Ok(read_stabilized_record(path)?
            .map(|record| {
                (
                    record.admission.retirement().cloned(),
                    record.admission.reset_source_digest().map(str::to_owned),
                )
            })
            .unwrap_or_default())
    })
}

#[cfg(test)]
pub(super) fn reserve_or_replay_blocking(
    path: &Path,
    requested: DurableAutomationAdmission,
) -> Result<ReservationResult> {
    reserve_or_replay_with_index(path, requested, || Ok(()), || Ok(()))
}

pub(super) fn reserve_or_replay_indexed_blocking(
    path: &Path,
    requested: DurableAutomationAdmission,
    ensure_pending: impl FnOnce() -> Result<()>,
    _rollback_pending: impl FnOnce() -> Result<()>,
) -> Result<ReservationResult> {
    reserve_or_replay_with_index_and_writer(path, requested, ensure_pending, write_record)
}

#[cfg(test)]
fn reserve_or_replay_with_index(
    path: &Path,
    requested: DurableAutomationAdmission,
    ensure_pending: impl FnOnce() -> Result<()>,
    _rollback_pending: impl FnOnce() -> Result<()>,
) -> Result<ReservationResult> {
    reserve_or_replay_with_index_and_writer(path, requested, ensure_pending, write_record)
}

fn reserve_or_replay_with_index_and_writer(
    path: &Path,
    requested: DurableAutomationAdmission,
    ensure_pending: impl FnOnce() -> Result<()>,
    write_fresh: impl FnOnce(&Path, &DurableAutomationRecord) -> Result<()>,
) -> Result<ReservationResult> {
    validate_admission_shape(&requested)?;
    with_journal_lock(path, || {
        let existing = read_stabilized_record(path)?;
        match existing {
            None => {
                let claim = acquire_reservation_claim(path)?;
                ensure_pending()?;
                let retirement = requested.retirement().cloned();
                write_fresh(
                    path,
                    &DurableAutomationRecord {
                        admission: requested,
                        state: DurableAutomationState::Reserved,
                        legacy_terminal: None,
                    },
                )?;
                Ok(ReservationResult::Execute { claim, retirement })
            }
            Some(record) => {
                if !stable_admission_matches(&record.admission, &requested) {
                    return Ok(ReservationResult::Conflict {
                        terminal: matches!(record.state, DurableAutomationState::Terminal { .. }),
                    });
                }
                if !matches!(record.state, DurableAutomationState::Terminal { .. }) {
                    ensure_pending()?;
                }
                match record.state {
                    DurableAutomationState::Terminal {
                        terminal: binding,
                        publication,
                    } => Ok(ReservationResult::Replay {
                        terminal: read_terminal_sidecar(path, &binding)?,
                        publication,
                        retirement: record.admission.retirement().cloned(),
                    }),
                    DurableAutomationState::Prepared {
                        terminal: binding,
                        publication,
                    } => Ok(ReservationResult::RecoverPrepared {
                        terminal: read_terminal_sidecar(path, &binding)?,
                        publication,
                        retirement: record.admission.retirement().cloned(),
                    }),
                    DurableAutomationState::Reserved if reservation_claim_is_live(path) => Err(
                        contract_error("an identical memory automation run is already in flight"),
                    ),
                    DurableAutomationState::Reserved => Ok(ReservationResult::Recover {
                        retirement: record.admission.retirement().cloned(),
                    }),
                }
            }
        }
    })
}

pub(super) fn read_indexed_record_blocking(path: &Path) -> Result<Option<DurableAutomationRecord>> {
    with_journal_lock(path, || read_stabilized_record(path))
}

pub(super) fn read_indexed_terminal_blocking(
    path: &Path,
) -> Result<Option<AutomationSettledTerminal>> {
    with_journal_lock(path, || {
        let Some(record) = read_stabilized_record(path)? else {
            return Ok(None);
        };
        let terminal = match record.state {
            DurableAutomationState::Prepared { terminal, .. }
            | DurableAutomationState::Terminal { terminal, .. } => terminal,
            DurableAutomationState::Reserved => return Ok(None),
        };
        read_terminal_sidecar(path, &terminal).map(Some)
    })
}

/// Exact durable state observed while revalidating an uncertain settlement.
///
/// `Prepared` proves that the intended terminal and publication binding are
/// durable, but it is not final settlement and must retain its owner guard.
/// Only `Terminal` permits that guard to be released. `Missing` and `Reserved`
/// also cannot generically release without separate exact-row absence proof;
/// any admission, terminal, or publication conflict is returned as an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DurableSettlementClassification {
    Missing,
    Reserved,
    Prepared,
    Terminal,
}

impl DurableSettlementClassification {
    pub(super) fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

/// Revalidates the exact intended settlement without changing journal state.
pub(super) fn classify_durable_settlement_blocking(
    path: &Path,
    requested: &DurableAutomationAdmission,
    intended_terminal: &AutomationSettledTerminal,
    intended_publication: Option<&ExactRunPublication>,
) -> Result<DurableSettlementClassification> {
    classify_durable_settlement_with_stabilizer(
        path,
        requested,
        intended_terminal,
        intended_publication,
        write_record,
    )
}

fn classify_durable_settlement_with_stabilizer(
    path: &Path,
    requested: &DurableAutomationAdmission,
    intended_terminal: &AutomationSettledTerminal,
    intended_publication: Option<&ExactRunPublication>,
    stabilize_record: impl FnOnce(&Path, &DurableAutomationRecord) -> Result<()>,
) -> Result<DurableSettlementClassification> {
    if !intended_terminal.matches_admission(requested) {
        return Err(contract_error(
            "automation settlement terminal does not match its durable admission",
        ));
    }
    if let Some(publication) = intended_publication {
        publication.validate().map_err(contract_error)?;
    }
    with_journal_lock(path, || {
        let Some(record) = read_stabilized_record_with_writer(path, stabilize_record)? else {
            return Ok(DurableSettlementClassification::Missing);
        };
        validate_stable_admission(&record.admission, requested)?;
        match record.state {
            DurableAutomationState::Reserved => Ok(DurableSettlementClassification::Reserved),
            DurableAutomationState::Prepared {
                terminal,
                publication,
            } => {
                if intended_publication != Some(&publication)
                    || read_terminal_sidecar(path, &terminal)? != *intended_terminal
                {
                    return Err(contract_error(
                        "prepared automation settlement conflicts with the intended terminal or publication",
                    ));
                }
                Ok(DurableSettlementClassification::Prepared)
            }
            DurableAutomationState::Terminal {
                terminal,
                publication,
            } => {
                if publication.as_ref() != intended_publication
                    || read_terminal_sidecar(path, &terminal)? != *intended_terminal
                {
                    return Err(contract_error(
                        "terminal automation settlement conflicts with the intended terminal or publication",
                    ));
                }
                Ok(DurableSettlementClassification::Terminal)
            }
        }
    })
}

/// Revalidates the sole state in which run-id-wide spool cleanup is safe.
///
/// Callers must hold the exact-publication spool lock before entering this
/// function. That preserves the global `spool -> journal -> live-claim` lock
/// order used by binding and makes the state check atomic with subsequent
/// cleanup: a writer cannot stage and bind `Prepared` between this check and
/// deletion.
pub(super) fn unbound_reserved_cleanup_is_safe_blocking(
    path: &Path,
    expected: &DurableAutomationAdmission,
) -> Result<bool> {
    with_journal_lock(path, || {
        let Some(record) = read_stabilized_record(path)? else {
            return Ok(false);
        };
        Ok(stable_admission_matches(&record.admission, expected)
            && matches!(record.state, DurableAutomationState::Reserved)
            && !reservation_claim_is_live(path))
    })
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
        if !terminal.matches_admission(requested) {
            return Err(contract_error(
                "recovered memory automation terminal does not match its durable admission",
            ));
        }
        let mut record = read_stabilized_record(path)?.ok_or_else(|| {
            contract_error("memory automation recovery has no durable reservation")
        })?;
        validate_stable_admission(&record.admission, requested)?;
        match &record.state {
            DurableAutomationState::Terminal {
                terminal: binding, ..
            } => {
                let stored = read_terminal_sidecar(path, binding)?;
                return if stored == terminal {
                    Ok(Some(stored))
                } else {
                    Err(contract_error(
                        "recovered memory automation terminal conflicts with durable replay",
                    ))
                };
            }
            DurableAutomationState::Prepared { .. } => {
                return Err(contract_error(
                    "reserved-effect recovery cannot replace a prepared terminal",
                ));
            }
            DurableAutomationState::Reserved if reservation_claim_is_live(path) => {
                return Err(contract_error(
                    "the current process cannot use restart recovery for its live reservation",
                ));
            }
            DurableAutomationState::Reserved => {}
        }
        if cancellation.is_some_and(CancellationSignal::is_cancelled) {
            return Ok(None);
        }
        let terminal_binding = write_terminal_sidecar(path, &terminal)?;
        record.state = DurableAutomationState::Terminal {
            terminal: terminal_binding,
            publication: None,
        };
        write_record(path, &record)?;
        let stored = read_stabilized_record(path)?.ok_or_else(|| {
            contract_error("recovered memory automation terminal disappeared after write")
        })?;
        validate_stable_admission(&stored.admission, requested)?;
        match stored.state {
            DurableAutomationState::Terminal {
                terminal: binding, ..
            } if read_terminal_sidecar(path, &binding)? == terminal => Ok(Some(terminal)),
            _ => Err(contract_error(
                "recovered memory automation terminal did not replay byte-identically",
            )),
        }
    })
}

pub(super) fn persist_prepared_terminal_blocking(
    path: &Path,
    requested: &DurableAutomationAdmission,
    terminal: &AutomationSettledTerminal,
    publication: ExactRunPublication,
) -> Result<()> {
    with_journal_lock(path, || {
        publication.validate().map_err(contract_error)?;
        if !terminal.matches_admission(requested) {
            return Err(contract_error(
                "prepared automation terminal does not match its durable admission",
            ));
        }
        let mut record = read_stabilized_record(path)?.ok_or_else(|| {
            contract_error("prepared automation terminal has no durable reservation")
        })?;
        validate_stable_admission(&record.admission, requested)?;
        match &record.state {
            DurableAutomationState::Prepared {
                terminal: binding,
                publication: stored_publication,
            } if stored_publication == &publication => {
                let stored = read_terminal_sidecar(path, binding)?;
                return if stored == *terminal {
                    Ok(())
                } else {
                    Err(contract_error(
                        "prepared automation terminal conflicts with its sidecar",
                    ))
                };
            }
            DurableAutomationState::Prepared { .. } | DurableAutomationState::Terminal { .. } => {
                return Err(contract_error(
                    "prepared automation terminal conflicts with durable state",
                ));
            }
            DurableAutomationState::Reserved
                if record.admission.process_run_id != requested.process_run_id =>
            {
                return Err(contract_error(
                    "automation reservation belongs to another process run",
                ));
            }
            DurableAutomationState::Reserved => {}
        }
        let terminal_binding = write_terminal_sidecar(path, terminal)?;
        record.state = DurableAutomationState::Prepared {
            terminal: terminal_binding,
            publication,
        };
        write_record(path, &record)?;
        let stored = read_stabilized_record(path)?.ok_or_else(|| {
            contract_error("prepared automation terminal disappeared after durable write")
        })?;
        validate_stable_admission(&stored.admission, requested)?;
        match stored.state {
            DurableAutomationState::Prepared {
                terminal: binding, ..
            } if read_terminal_sidecar(path, &binding)? == *terminal => Ok(()),
            _ => Err(contract_error(
                "prepared automation terminal did not replay byte-identically",
            )),
        }
    })
}

/// Classifies an uncertain exact-bind failure without changing state. A
/// matching `Prepared` (or already promoted `Terminal`) proves the binding was
/// durable despite the surfaced I/O/readback error; `Reserved` proves no
/// journal binding and leaves the spool for recovery cleanup.
pub(super) fn replay_exact_binding_after_error_blocking(
    path: &Path,
    requested: &DurableAutomationAdmission,
    terminal: &AutomationSettledTerminal,
    publication: &ExactRunPublication,
) -> Result<Option<AutomationSettledTerminal>> {
    with_journal_lock(path, || {
        let record = read_stabilized_record(path)?.ok_or_else(|| {
            contract_error("uncertain prepared automation binding lost its durable journal")
        })?;
        validate_stable_admission(&record.admission, requested)?;
        match record.state {
            DurableAutomationState::Reserved => Ok(None),
            DurableAutomationState::Prepared {
                terminal: binding,
                publication: stored_publication,
            }
            | DurableAutomationState::Terminal {
                terminal: binding,
                publication: Some(stored_publication),
            } if stored_publication == *publication => {
                let stored = read_terminal_sidecar(path, &binding)?;
                if stored == *terminal {
                    Ok(Some(stored))
                } else {
                    Err(contract_error(
                        "uncertain prepared automation binding conflicts with its terminal",
                    ))
                }
            }
            DurableAutomationState::Prepared { .. } | DurableAutomationState::Terminal { .. } => {
                Err(contract_error(
                    "uncertain prepared automation binding conflicts with durable state",
                ))
            }
        }
    })
}

pub(super) fn promote_prepared_terminal_blocking(
    path: &Path,
    requested: &DurableAutomationAdmission,
    terminal: AutomationSettledTerminal,
    publication: &ExactRunPublication,
) -> Result<AutomationSettledTerminal> {
    promote_prepared_terminal_with_writers(
        path,
        requested,
        terminal,
        publication,
        write_record,
        write_record,
    )
}

#[cfg(test)]
fn promote_prepared_terminal_with_writer(
    path: &Path,
    requested: &DurableAutomationAdmission,
    terminal: AutomationSettledTerminal,
    publication: &ExactRunPublication,
    write_terminal_record: impl FnOnce(&Path, &DurableAutomationRecord) -> Result<()>,
) -> Result<AutomationSettledTerminal> {
    promote_prepared_terminal_with_writers(
        path,
        requested,
        terminal,
        publication,
        write_record,
        write_terminal_record,
    )
}

fn promote_prepared_terminal_with_writers(
    path: &Path,
    requested: &DurableAutomationAdmission,
    terminal: AutomationSettledTerminal,
    publication: &ExactRunPublication,
    stabilize_record: impl FnOnce(&Path, &DurableAutomationRecord) -> Result<()>,
    write_terminal_record: impl FnOnce(&Path, &DurableAutomationRecord) -> Result<()>,
) -> Result<AutomationSettledTerminal> {
    with_journal_lock(path, || {
        publication.validate().map_err(contract_error)?;
        if !terminal.matches_admission(requested) {
            return Err(contract_error(
                "prepared automation terminal does not match its durable admission",
            ));
        }
        let mut record = read_stabilized_record_with_writer(path, stabilize_record)?
            .ok_or_else(|| contract_error("prepared automation terminal has no durable journal"))?;
        validate_stable_admission(&record.admission, requested)?;
        match &record.state {
            DurableAutomationState::Terminal {
                terminal: binding,
                publication: Some(stored_publication),
            } if stored_publication == publication => {
                let stored = read_terminal_sidecar(path, binding)?;
                return if stored == terminal {
                    Ok(stored)
                } else {
                    Err(contract_error(
                        "promoted automation terminal conflicts with its sidecar",
                    ))
                };
            }
            DurableAutomationState::Prepared {
                terminal: binding,
                publication: stored_publication,
            } if stored_publication == publication
                && read_terminal_sidecar(path, binding)? == terminal => {}
            DurableAutomationState::Reserved
            | DurableAutomationState::Prepared { .. }
            | DurableAutomationState::Terminal { .. } => {
                return Err(contract_error(
                    "prepared automation promotion conflicts with durable state",
                ));
            }
        }
        // Prepared means outer settlement already accepted this terminal and
        // the exact ledger row is being (or has been) published. Cancellation
        // may stop work before that boundary, but must not strand a published
        // row behind a forever-Prepared journal.
        record.state = DurableAutomationState::Terminal {
            terminal: terminal_binding(&terminal)?,
            publication: Some(publication.clone()),
        };
        write_terminal_record(path, &record)?;
        let stored = read_stabilized_record(path)?.ok_or_else(|| {
            contract_error("promoted automation terminal disappeared after durable write")
        })?;
        match stored.state {
            DurableAutomationState::Terminal {
                terminal: binding,
                publication: Some(stored_publication),
            } if stored_publication == *publication
                && read_terminal_sidecar(path, &binding)? == terminal =>
            {
                Ok(terminal)
            }
            _ => Err(contract_error(
                "promoted automation terminal did not replay byte-identically",
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
        let mut record = read_stabilized_record(path)?.ok_or_else(|| {
            contract_error("memory automation terminal has no durable reservation")
        })?;
        validate_stable_admission(&record.admission, requested)?;
        match &record.state {
            DurableAutomationState::Terminal {
                terminal: binding,
                publication: None,
            } => {
                let stored = read_terminal_sidecar(path, binding)?;
                return if stored == terminal {
                    Ok(stored)
                } else {
                    Err(contract_error(
                        "memory automation terminal conflicts with its durable replay",
                    ))
                };
            }
            DurableAutomationState::Terminal { .. } => {
                return Err(contract_error(
                    "memory automation terminal conflicts with its durable replay",
                ));
            }
            DurableAutomationState::Prepared { .. } => {
                return Err(contract_error(
                    "non-published terminal cannot replace a prepared terminal",
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
        let terminal_binding = write_terminal_sidecar(path, &terminal)?;
        record.state = DurableAutomationState::Terminal {
            terminal: terminal_binding,
            publication: None,
        };
        write_record(path, &record)?;
        let stored = read_stabilized_record(path)?.ok_or_else(|| {
            contract_error("memory automation terminal disappeared after durable write")
        })?;
        validate_stable_admission(&stored.admission, requested)?;
        match stored.state {
            DurableAutomationState::Terminal {
                terminal: binding,
                publication: None,
            } if read_terminal_sidecar(path, &binding)? == terminal => Ok(terminal),
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
        let Some(record) = read_stabilized_record(path)? else {
            return tracedecay_application::sync_parent_directory(
                path,
                DirectorySyncPolicy::Strict,
            )
            .map_err(|error| {
                contract_error(format!(
                    "memory automation reservation rollback directory sync failed: {error}"
                ))
            });
        };
        validate_stable_admission(&record.admission, requested)?;
        if matches!(record.state, DurableAutomationState::Terminal { .. }) {
            return Ok(());
        }
        if !matches!(record.state, DurableAutomationState::Reserved)
            || record.admission.process_run_id != requested.process_run_id
        {
            return Err(contract_error(
                "only the owning uncommitted memory automation reservation can roll back",
            ));
        }
        let sidecar = terminal_sidecar_path(path)?;
        match crate::storage::PrivateStoreIo::remove_file_durable(&sidecar) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(contract_error(format!(
                    "memory automation terminal sidecar rollback failed: {error}"
                )));
            }
        }
        crate::storage::PrivateStoreIo::remove_file_durable(path).map_err(|error| {
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
    crate::storage::PrivateStoreIo::create_dir_all_durable(parent).map_err(|error| {
        contract_error(format!(
            "automation terminal directory creation failed: {error}"
        ))
    })?;
    let lock_path = crate::storage::append_lock_path(path);
    let lock = open_lock_nofollow(&lock_path)
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

/// Makes a visible journal replacement durable before any caller uses it to
/// publish an exact row or retire recovery authority. An atomic replacement
/// can become visible and still return an error after publication, so
/// visibility alone is not settlement evidence.
fn read_stabilized_record(path: &Path) -> Result<Option<DurableAutomationRecord>> {
    let Some(record) = read_record(path)? else {
        return Ok(None);
    };
    stabilize_bound_record_after_visibility(path, &record).map(Some)
}

fn read_stabilized_record_with_writer(
    path: &Path,
    stabilize_record: impl FnOnce(&Path, &DurableAutomationRecord) -> Result<()>,
) -> Result<Option<DurableAutomationRecord>> {
    let Some(record) = read_record(path)? else {
        return Ok(None);
    };
    stabilize_bound_record_after_visibility_with(path, &record, stabilize_record).map(Some)
}

fn stabilize_bound_record_after_visibility(
    path: &Path,
    expected: &DurableAutomationRecord,
) -> Result<DurableAutomationRecord> {
    stabilize_bound_record_after_visibility_with(path, expected, write_record)
}

fn stabilize_bound_record_after_visibility_with(
    path: &Path,
    expected: &DurableAutomationRecord,
    republish: impl FnOnce(&Path, &DurableAutomationRecord) -> Result<()>,
) -> Result<DurableAutomationRecord> {
    match &expected.state {
        DurableAutomationState::Prepared { terminal, .. }
        | DurableAutomationState::Terminal { terminal, .. } => {
            let stored_terminal = read_terminal_sidecar(path, terminal)?;
            if write_terminal_sidecar(path, &stored_terminal)? != *terminal {
                return Err(contract_error(
                    "automation terminal sidecar binding changed during exact republication",
                ));
            }
        }
        DurableAutomationState::Reserved => {}
    }
    republish(path, expected)?;
    let stabilized = read_record(path)?.ok_or_else(|| {
        contract_error("automation terminal disappeared after exact durable republication")
    })?;
    if stabilized != *expected {
        return Err(contract_error(
            "automation terminal changed while stabilizing visible durable state",
        ));
    }
    Ok(stabilized)
}

fn read_record(path: &Path) -> Result<Option<DurableAutomationRecord>> {
    let Some(file) = open_regular_nofollow(path)? else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(contract_error)?;
    if metadata.len() > MAX_AUTOMATION_JOURNAL_BYTES {
        return Err(contract_error(
            "automation terminal is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_AUTOMATION_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(contract_error)?;
    if bytes.len() as u64 > MAX_AUTOMATION_JOURNAL_BYTES {
        return Err(contract_error(
            "automation terminal grew beyond its durable byte bound",
        ));
    }
    {
        let mut record =
            serde_json::from_slice::<DurableAutomationRecord>(&bytes).map_err(contract_error)?;
        validate_admission_shape(&record.admission)?;
        if let Some(legacy_terminal) = record.legacy_terminal.take() {
            if !legacy_terminal.matches_admission(&record.admission) {
                return Err(contract_error(
                    "legacy automation terminal is inconsistent with its admission",
                ));
            }
            let binding = write_terminal_sidecar(path, &legacy_terminal)?;
            record.state = DurableAutomationState::Terminal {
                terminal: binding,
                publication: None,
            };
            write_record(path, &record)?;
        }
        match &record.state {
            DurableAutomationState::Reserved => {
                // A terminal sidecar without Prepared is the crash residue of
                // a bind that never durably published its journal transition.
                // The journal lock proves no writer can still complete that
                // transition, so remove the orphan before any retry binds a
                // different exact terminal.
                remove_terminal_sidecar_if_present(path)?;
            }
            DurableAutomationState::Prepared {
                terminal,
                publication,
            } => {
                terminal.validate()?;
                publication.validate().map_err(contract_error)?;
                if !read_terminal_sidecar(path, terminal)?.matches_admission(&record.admission) {
                    return Err(contract_error(
                        "memory automation prepared terminal is inconsistent",
                    ));
                }
            }
            DurableAutomationState::Terminal {
                terminal,
                publication,
            } => {
                terminal.validate()?;
                if let Some(publication) = publication {
                    publication.validate().map_err(contract_error)?;
                }
                if !read_terminal_sidecar(path, terminal)?.matches_admission(&record.admission) {
                    return Err(contract_error(
                        "memory automation durable terminal is inconsistent",
                    ));
                }
            }
        }
        Ok(Some(record))
    }
}

fn open_lock_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        crate::storage::PrivateStoreIo::create_dir_all_durable(parent)?;
    }
    crate::storage::reject_symlink_components(path, "automation terminal journal lock")?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "journal lock has no parent",
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "journal lock has no filename",
        )
    })?;
    let directory = Dir::open_ambient_dir(parent, ambient_authority())?;
    let mut options = CapOpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    let file = directory.open_with(name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "automation terminal journal lock is not a regular file",
        ));
    }
    let file = file.into_std();
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

fn write_record(path: &Path, record: &DurableAutomationRecord) -> Result<()> {
    write_record_with_publisher(path, record, |temporary, destination| {
        replace_automation_file_atomically(temporary, destination, "automation terminal journal")
    })
}

fn write_record_with_publisher(
    path: &Path,
    record: &DurableAutomationRecord,
    publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let mut bytes = BoundedJournalBytes::new(MAX_AUTOMATION_JOURNAL_BYTES as usize);
    serde_json::to_writer_pretty(&mut bytes, record).map_err(contract_error)?;
    tracedecay_domain::with_owned_temp_publish(
        path,
        "automation-run-terminal",
        publish,
        |output| output.write_all(bytes.as_slice()),
        DirectorySyncPolicy::Strict,
    )
    .map_err(|error| contract_error(format!("automation terminal write failed: {error}")))?;
    let stored = read_record(path)?.ok_or_else(|| {
        contract_error("automation terminal disappeared after durable replacement")
    })?;
    if stored != *record {
        return Err(contract_error(
            "automation terminal replacement did not read back byte-identically",
        ));
    }
    Ok(())
}

pub(super) fn replace_automation_file_atomically(
    temporary: &Path,
    destination: &Path,
    record_name: &str,
) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let temporary_file =
            tracedecay_runtime_core::windows_security::make_private_file(temporary)?;
        temporary_file.sync_all()?;
        drop(temporary_file);
    }
    crate::db::DatabaseAuthority::replace_file_atomically(temporary, destination, record_name)
        .map_err(std::io::Error::other)?;
    #[cfg(windows)]
    tracedecay_runtime_core::windows_security::validate_private_file(destination)?;
    Ok(())
}

fn terminal_sidecar_path(journal_path: &Path) -> Result<PathBuf> {
    let filename = journal_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| contract_error("automation journal has no terminal sidecar identity"))?;
    Ok(journal_path.with_file_name(format!("{filename}.terminal")))
}

fn remove_terminal_sidecar_if_present(journal_path: &Path) -> Result<()> {
    let path = terminal_sidecar_path(journal_path)?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(contract_error(
                "automation terminal sidecar cleanup target is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(contract_error(format!(
                "automation terminal sidecar cleanup inspection failed: {error}"
            )));
        }
    }
    crate::storage::PrivateStoreIo::remove_file_durable(&path)
        .map(|_| ())
        .map_err(|error| {
            contract_error(format!(
                "automation terminal sidecar cleanup failed: {error}"
            ))
        })
}

fn terminal_binding(
    terminal: &AutomationSettledTerminal,
) -> Result<DurableAutomationTerminalBinding> {
    let mut writer = TerminalDigestWriter::default();
    serde_json::to_writer(&mut writer, terminal).map_err(contract_error)?;
    writer.finish()
}

fn write_terminal_sidecar(
    journal_path: &Path,
    terminal: &AutomationSettledTerminal,
) -> Result<DurableAutomationTerminalBinding> {
    write_terminal_sidecar_with_publisher(journal_path, terminal, |temporary, destination| {
        replace_automation_file_atomically(temporary, destination, "automation terminal sidecar")
    })
}

fn write_terminal_sidecar_with_publisher(
    journal_path: &Path,
    terminal: &AutomationSettledTerminal,
    publish: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<DurableAutomationTerminalBinding> {
    let binding = terminal_binding(terminal)?;
    let path = terminal_sidecar_path(journal_path)?;
    if let Some(existing) = read_terminal_sidecar_if_present(&path, &binding)? {
        if existing != *terminal {
            return Err(contract_error(
                "automation terminal sidecar conflicts with its durable binding",
            ));
        }
    }
    tracedecay_domain::with_owned_temp_publish(
        &path,
        "automation-terminal-sidecar",
        publish,
        |output| serde_json::to_writer(output, terminal).map_err(std::io::Error::other),
        DirectorySyncPolicy::Strict,
    )
    .map_err(|error| {
        contract_error(format!("automation terminal sidecar write failed: {error}"))
    })?;
    let stored = read_terminal_sidecar_if_present(&path, &binding)?.ok_or_else(|| {
        contract_error("automation terminal sidecar disappeared after durable write")
    })?;
    if stored != *terminal {
        return Err(contract_error(
            "automation terminal sidecar did not replay byte-identically",
        ));
    }
    Ok(binding)
}

fn read_terminal_sidecar(
    journal_path: &Path,
    binding: &DurableAutomationTerminalBinding,
) -> Result<AutomationSettledTerminal> {
    let path = terminal_sidecar_path(journal_path)?;
    read_terminal_sidecar_if_present(&path, binding)?.ok_or_else(|| {
        contract_error("automation terminal sidecar is missing from its durable journal")
    })
}

fn read_terminal_sidecar_if_present(
    path: &Path,
    binding: &DurableAutomationTerminalBinding,
) -> Result<Option<AutomationSettledTerminal>> {
    binding.validate()?;
    let Some(mut file) = open_regular_nofollow(path)? else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(|error| {
        contract_error(format!(
            "automation terminal sidecar metadata failed: {error}"
        ))
    })?;
    if metadata.len() != binding.payload_len {
        return Err(contract_error(
            "automation terminal sidecar length conflicts with its journal binding",
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut remaining = binding.payload_len;
    while remaining > 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            contract_error("automation terminal sidecar length is not representable")
        })?;
        file.read_exact(&mut buffer[..take]).map_err(|error| {
            contract_error(format!("automation terminal sidecar read failed: {error}"))
        })?;
        hasher.update(&buffer[..take]);
        remaining -= take as u64;
    }
    let actual = ManifestDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(contract_error)?;
    if actual != binding.digest {
        return Err(contract_error(
            "automation terminal sidecar digest conflicts with its journal binding",
        ));
    }
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        contract_error(format!(
            "automation terminal sidecar rewind failed: {error}"
        ))
    })?;
    serde_json::from_reader(file.take(binding.payload_len))
        .map(Some)
        .map_err(contract_error)
}

fn open_regular_nofollow(path: &Path) -> Result<Option<std::fs::File>> {
    crate::storage::reject_symlink_components(path, "automation terminal journal or sidecar")
        .map_err(contract_error)?;
    let parent = path
        .parent()
        .ok_or_else(|| contract_error("automation terminal sidecar has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| contract_error("automation terminal sidecar has no filename"))?;
    let directory = match Dir::open_ambient_dir(parent, ambient_authority()) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(contract_error(error)),
    };
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    match directory.open_with(name, &options) {
        Ok(file) => {
            let metadata = file.metadata().map_err(contract_error)?;
            if !metadata.is_file() {
                return Err(contract_error(
                    "automation terminal sidecar is not a regular file",
                ));
            }
            Ok(Some(file.into_std()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(contract_error(error)),
    }
}

#[derive(Default)]
struct TerminalDigestWriter {
    hasher: Sha256,
    len: u64,
}

impl TerminalDigestWriter {
    fn finish(self) -> Result<DurableAutomationTerminalBinding> {
        if self.len == 0 || self.len > MAX_AUTOMATION_TERMINAL_BYTES {
            return Err(contract_error(
                "automation terminal sidecar exceeds its durable byte bound",
            ));
        }
        Ok(DurableAutomationTerminalBinding {
            schema_version: 1,
            digest: ManifestDigest::new(format!("sha256:{}", hex::encode(self.hasher.finalize())))
                .map_err(contract_error)?,
            payload_len: self.len,
        })
    }
}

impl Write for TerminalDigestWriter {
    fn write(&mut self, incoming: &[u8]) -> std::io::Result<usize> {
        let next = self
            .len
            .checked_add(incoming.len() as u64)
            .ok_or_else(|| std::io::Error::other("automation terminal sidecar size overflow"))?;
        if next > MAX_AUTOMATION_TERMINAL_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "automation terminal sidecar exceeds its durable byte bound",
            ));
        }
        self.hasher.update(incoming);
        self.len = next;
        Ok(incoming.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct BoundedJournalBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedJournalBytes {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(8 * 1024)),
            limit,
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::io::Write for BoundedJournalBytes {
    fn write(&mut self, incoming: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(incoming.len())
            .ok_or_else(|| std::io::Error::other("automation journal size overflow"))?;
        if next > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "automation journal exceeds its durable byte bound",
            ));
        }
        self.bytes.extend_from_slice(incoming);
        Ok(incoming.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

fn validate_admission_shape(admission: &DurableAutomationAdmission) -> Result<()> {
    if admission.schema_version != 1
        || !admission.request.validate()
        || !tracedecay_domain::canonical_text::is_canonical_text_within(
            &admission.process_run_id,
            tracedecay_domain::canonical_text::CANONICAL_TEXT_MAX_BYTES,
        )
    {
        return Err(contract_error(
            "memory automation durable admission has an unsupported shape",
        ));
    }
    let memory_task = matches!(
        admission.request.task_kind(),
        AutomationTaskV1::MemoryCurator | AutomationTaskV1::SessionReflector
    );
    if memory_task == admission.is_external() {
        return Err(contract_error(
            "automation recovery binding does not match the admitted task",
        ));
    }
    if let AutomationRecoveryBinding::Memory {
        retirement,
        reset_source_digest,
        ..
    } = &admission.recovery
    {
        if retirement.is_some() && reset_source_digest.is_some() {
            return Err(contract_error(
                "automation recovery cannot retire and reset the same source",
            ));
        }
        if let Some(retirement) = retirement {
            validate_sha256_text(&retirement.source_digest)?;
            let expected = format!(
                "fact_proposals.{}.json",
                retirement.source_digest.trim_start_matches("sha256:")
            );
            if retirement.archive_name != expected {
                return Err(contract_error(
                    "automation retirement archive identity is inconsistent",
                ));
            }
        }
        if let Some(source_digest) = reset_source_digest {
            validate_sha256_text(source_digest)?;
        }
    }
    admission.scope.validate().map_err(contract_error)?;
    admission.input_digest.validate().map_err(contract_error)?;
    admission
        .configuration_digest
        .validate()
        .map_err(contract_error)?;
    if let Some(owner) = admission.memory_owner() {
        owner.validate().map_err(contract_error)?;
    }
    admission.actor.validate().map_err(contract_error)?;
    admission
        .effect_authority_digest
        .validate()
        .map_err(contract_error)?;
    admission.grant_digest.validate().map_err(contract_error)?;
    admission
        .effect_receipt_template
        .validate()
        .map_err(contract_error)?;
    if !AutomationSettledTerminal::Problem(admission.recovery_problem().clone())
        .matches_admission(admission)
    {
        return Err(contract_error(
            "memory automation recovery problem is inconsistent",
        ));
    }
    let expected_owner = FactOwnerV1::Project {
        project_id: admission.scope.project_id.clone(),
    };
    let template = &admission.effect_receipt_template;
    if admission
        .memory_owner()
        .is_some_and(|owner| owner != &expected_owner)
        || template.request_id != admission.request_id
        || template.actor != admission.actor
        || template.scope != admission.scope
        || template.configuration_digest != admission.configuration_digest
        || template.policy_digest != admission.grant_digest
        || admission.grant_revision == 0
        || template.outcome != tracedecay_application::EffectTermination::Partial
        || template.committed_state.is_some()
    {
        return Err(contract_error(
            "memory automation prepared effect binding is inconsistent",
        ));
    }
    Ok(())
}

fn validate_sha256_text(digest: &str) -> Result<()> {
    let Some(raw) = digest.strip_prefix("sha256:") else {
        return Err(contract_error(
            "automation recovery source digest is not canonical SHA-256",
        ));
    };
    if raw.len() != 64
        || !raw
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(contract_error(
            "automation recovery source digest is not canonical SHA-256",
        ));
    }
    Ok(())
}

fn stable_admission_matches(
    stored: &DurableAutomationAdmission,
    requested: &DurableAutomationAdmission,
) -> bool {
    stored.schema_version == 1
        && requested.schema_version == 1
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

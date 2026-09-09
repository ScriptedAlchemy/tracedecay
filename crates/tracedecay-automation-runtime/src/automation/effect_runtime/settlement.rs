//! Blocking ownership of exact automation journal and ledger settlement.

use super::journal::{
    DurableAutomationAdmission, DurableSettlementClassification,
    classify_durable_settlement_blocking, persist_prepared_terminal_blocking,
    promote_prepared_terminal_blocking, replay_exact_binding_after_error_blocking,
};
use super::{AutomationSettledTerminal, contract_error, remove_pending_blocking};
use crate::automation::run_ledger::{
    self, AutomationRunLedgerRecord, ExactRunPublication, ExactRunPublishOutcome,
};
use std::path::PathBuf;
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::Arc;
use std::time::Duration;
use tracedecay_contracts::CancellationSignal;
use tracedecay_domain::errors::Result;

/// Exact publication state retained throughout bounded retries. The caller keeps
/// its admission claim and scheduler guards alive until settlement returns.
pub struct BoundSettlement {
    dashboard_root: PathBuf,
    journal_path: PathBuf,
    admission: DurableAutomationAdmission,
    cancellation: CancellationSignal,
    terminal: AutomationSettledTerminal,
    ledger: AutomationRunLedgerRecord,
    publication: Option<ExactRunPublication>,
    #[cfg(any(test, feature = "test-helpers"))]
    phase_hook: Option<SettlementPhaseHook>,
    #[cfg(any(test, feature = "test-helpers"))]
    prepared_write_hook: Option<PreparedWriteHook>,
}

impl BoundSettlement {
    pub fn new(
        dashboard_root: PathBuf,
        journal_path: PathBuf,
        admission: DurableAutomationAdmission,
        cancellation: CancellationSignal,
        terminal: AutomationSettledTerminal,
        ledger: AutomationRunLedgerRecord,
        publication: Option<ExactRunPublication>,
    ) -> Self {
        Self {
            dashboard_root,
            journal_path,
            admission,
            cancellation,
            terminal,
            ledger,
            publication,
            #[cfg(any(test, feature = "test-helpers"))]
            phase_hook: None,
            #[cfg(any(test, feature = "test-helpers"))]
            prepared_write_hook: None,
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[must_use]
    pub fn with_test_hooks(
        mut self,
        phase_hook: Option<SettlementPhaseHook>,
        prepared_write_hook: Option<PreparedWriteHook>,
    ) -> Self {
        self.phase_hook = phase_hook;
        self.prepared_write_hook = prepared_write_hook;
        self
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedSettlementPhase {
    PreparedWriteFailed,
    Prepared,
    Published,
}

#[cfg(any(test, feature = "test-helpers"))]
pub struct SettlementPhaseHook {
    callback: Arc<dyn Fn(RetainedSettlementPhase) + Send + Sync + 'static>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl SettlementPhaseHook {
    pub fn new(callback: impl Fn(RetainedSettlementPhase) + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    fn notify(&self, phase: RetainedSettlementPhase) {
        (self.callback)(phase);
    }
}

#[cfg(any(test, feature = "test-helpers"))]
type PreparedWriteCallback =
    Arc<dyn Fn(&ExactRunPublication) -> Result<()> + Send + Sync + 'static>;

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone)]
pub struct PreparedWriteHook {
    callback: PreparedWriteCallback,
}

#[cfg(any(test, feature = "test-helpers"))]
impl PreparedWriteHook {
    pub fn new(
        callback: impl Fn(&ExactRunPublication) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    fn before_write(&self, publication: &ExactRunPublication) -> Result<()> {
        (self.callback)(publication)
    }
}

#[hotpath::measure(label = "daemon.automation.effect.settle")]
pub fn settle(
    mut state: BoundSettlement,
    budget: Duration,
) -> Result<(AutomationSettledTerminal, AutomationRunLedgerRecord)> {
    let started = std::time::Instant::now();
    let mut delay = Duration::from_millis(25);
    loop {
        let error = match settle_bound_once(&mut state) {
            Ok(()) => return Ok((state.terminal, state.ledger)),
            Err(error) => {
                match classify_bound_settlement(&state) {
                    Ok(classification)
                        if classification.is_terminal() && state.publication.is_some() =>
                    {
                        tracing::warn!(
                            run_id = %state.ledger.run_id,
                            error = %error,
                            "automation settlement reached its exact terminal with deferred housekeeping"
                        );
                        cleanup_bound_terminal(&state);
                        return Ok((state.terminal, state.ledger));
                    }
                    Ok(_) => tracing::warn!(
                        run_id = %state.ledger.run_id,
                        error = %error,
                        "automation finalization remains pending under its blocking owner"
                    ),
                    Err(classification_error) => tracing::warn!(
                            run_id = %state.ledger.run_id,
                            error = %error,
                            classification_error = %classification_error,
                            "automation finalization remains uncertain under its blocking owner"
                    ),
                }
                error
            }
        };
        if state.cancellation.is_cancelled() {
            return Err(contract_error(format!(
                "retained automation settlement for run '{}' was cancelled while its blocking owner retried; state remains recoverable: {error}",
                state.ledger.run_id
            )));
        }
        if started.elapsed() >= budget {
            return Err(contract_error(format!(
                "retained automation settlement for run '{}' exceeded its retry budget; state remains recoverable: {error}",
                state.ledger.run_id
            )));
        }
        std::thread::sleep(delay);
        delay = delay.saturating_mul(2).min(Duration::from_secs(5));
    }
}

fn settle_bound_once(state: &mut BoundSettlement) -> Result<()> {
    if state.publication.is_none() {
        #[cfg(any(test, feature = "test-helpers"))]
        let prepared_write_hook = state.prepared_write_hook.clone();
        let bound = run_ledger::bind_staged_run_record_exact(
            &state.dashboard_root,
            &state.ledger,
            |publication| {
                #[cfg(any(test, feature = "test-helpers"))]
                if let Some(hook) = prepared_write_hook.as_ref() {
                    hook.before_write(publication)?;
                }
                let first = persist_prepared_terminal_blocking(
                    &state.journal_path,
                    &state.admission,
                    &state.terminal,
                    publication.clone(),
                );
                match first {
                    Ok(()) => Ok(()),
                    Err(first_error) => replay_exact_binding_after_error_blocking(
                        &state.journal_path,
                        &state.admission,
                        &state.terminal,
                        publication,
                    )?
                    .map(|_| ())
                    .ok_or(first_error),
                }
            },
        );
        match bound {
            Ok((publication, ())) => {
                state.publication = Some(publication);
                #[cfg(any(test, feature = "test-helpers"))]
                if let Some(phase_hook) = state.phase_hook.as_ref() {
                    phase_hook.notify(RetainedSettlementPhase::Prepared);
                }
            }
            Err(error) => {
                // A staged payload identifies only captured bytes. Until the
                // bind callback returns successfully, the journal has not
                // proven that exact publication as Prepared. Leave it
                // unbound so this owner re-enters the canonical bind path,
                // which reuses the digest-owned spool without publishing it.
                state.publication = None;
                #[cfg(any(test, feature = "test-helpers"))]
                if let Some(phase_hook) = state.phase_hook.as_ref() {
                    phase_hook.notify(RetainedSettlementPhase::PreparedWriteFailed);
                }
                return Err(error);
            }
        }
    }
    let publication = state
        .publication
        .as_ref()
        .ok_or_else(|| contract_error("prepared settlement lost its exact publication"))?;
    let published = hotpath::measure_block!("daemon.automation.effect.publish", {
        run_ledger::publish_staged_run_record_exact_blocking(
            &state.dashboard_root,
            state.admission.request.run_id.as_str(),
            publication,
        )
    })?;
    if published == ExactRunPublishOutcome::MissingPayload {
        return Err(contract_error(
            "prepared automation terminal has neither its spool nor exact ledger row",
        ));
    }
    #[cfg(any(test, feature = "test-helpers"))]
    {
        if let Some(phase_hook) = state.phase_hook.as_ref() {
            phase_hook.notify(RetainedSettlementPhase::Published);
        }
    }
    state.terminal = promote_prepared_terminal_blocking(
        &state.journal_path,
        &state.admission,
        state.terminal.clone(),
        publication,
    )?;
    cleanup_bound_terminal(state);
    Ok(())
}

fn classify_bound_settlement(state: &BoundSettlement) -> Result<DurableSettlementClassification> {
    classify_durable_settlement_blocking(
        &state.journal_path,
        &state.admission,
        &state.terminal,
        state.publication.as_ref(),
    )
}

fn cleanup_bound_terminal(state: &BoundSettlement) {
    if let Some(publication) = state.publication.as_ref()
        && let Err(error) = run_ledger::discard_staged_run_record_exact_blocking(
            &state.dashboard_root,
            state.admission.request.run_id.as_str(),
            publication,
        )
    {
        tracing::warn!(
            run_id = %state.ledger.run_id,
            error = %error,
            "exact automation terminal is committed; spool cleanup remains recoverable"
        );
        return;
    }
    if let Err(error) = remove_pending_blocking(&state.dashboard_root, &state.journal_path) {
        tracing::warn!(
            run_id = %state.ledger.run_id,
            error = %error,
            "exact automation terminal is committed; pending-index cleanup remains recoverable"
        );
    }
}

//! Durable application settlement for automatic memory runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus,
};
use tracedecay_agent_hosts::automation::{AutomationCommittedReceipt, AutomationRunError};
use tracedecay_application::retained_surfaces::{
    AutomationCommittedReceiptV1, AutomationRunProblemV1, AutomationRunRequestV1,
    AutomationRunResultV1, AutomationRunSummaryV1, AutomationRunTerminalV1, AutomationSkipReasonV1,
    AutomationTaskV1, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope, CancellationSignal,
    Deadline, ProblemOwningLayer, RequestContext, RequestId, ResolvedScope,
    RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1, RetainedSurfaceOperation,
    retained_surface_application_operation, retained_surface_execution_problem,
    retained_surface_outcome_matches_terminal, retained_surface_problem_matches_terminal,
};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{ManifestDigest, RunId, UtcMicros};
use tracedecay_store::FactReadControl;

use crate::daemon::retained_owner::receipts::{PreparedRetainedEffect, prepare_retained_effect};
use crate::daemon::service::invocation::{
    DaemonInvocationService, RegisteredRetainedRequestContextError,
};
use crate::errors::Result;

mod authority;
mod contract;
mod input;
mod journal;
mod problem;
mod projection;
mod recovery_index;
mod retirement;
use authority::{finalize_retirement, remove_pending_index};
use contract::{contract_error, digest};
use journal::{
    AutomationRecoveryBinding, DurableAutomationAdmission, ReservationResult,
    abandon_reservation_blocking, persist_recovered_terminal_blocking, persist_terminal_blocking,
    reserve_or_replay_blocking, retained_source_bindings,
};
use problem::{
    failed_ledger_problem, indeterminate_external_effect_problem, reset_required_problem,
    runtime_problem, shipped_proposal_reset_required_problem,
};
use projection::{
    project_committed_receipts, project_recovered_committed_receipts, project_run_summary,
    project_skip_reason,
};

pub(crate) struct AutomationEffectAuthority {
    context: RequestContext,
    cancellation: CancellationSignal,
    operation: tracedecay_application::ApplicationOperation,
    prepared: PreparedRetainedEffect,
    admission: DurableAutomationAdmission,
    journal_path: PathBuf,
    dashboard_root: PathBuf,
}

pub(crate) type AutomationSettledProblem = AutomationRunProblemV1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum AutomationSettledTerminal {
    Outcome {
        scope: ResolvedScope,
        outcome: ApplicationOutcome<RetainedSurfaceResultV1>,
    },
    Problem(AutomationSettledProblem),
}

impl AutomationSettledTerminal {
    pub(crate) fn into_outcome(
        self,
    ) -> std::result::Result<ApplicationOutcome<RetainedSurfaceResultV1>, AutomationSettledProblem>
    {
        match self {
            Self::Outcome { outcome, .. } => Ok(outcome),
            Self::Problem(problem) => Err(problem),
        }
    }

    fn matches_admission(&self, admission: &DurableAutomationAdmission) -> bool {
        match self {
            Self::Outcome {
                scope: terminal_scope,
                outcome,
            } => {
                terminal_scope == &admission.scope
                    && retained_surface_outcome_matches_terminal(
                        RetainedSurfaceOperation::FactStoreCurate,
                        &admission.request_id,
                        &admission.scope,
                        outcome,
                    )
                    && matches!(
                        outcome,
                        ApplicationOutcome::Effect(effect)
                            if matches!(
                                effect.payload.as_ref(),
                                Some(RetainedSurfaceResultV1::FactStoreCurate(result))
                                    if result.matches_admission(&admission.request)
                            )
                    )
            }
            Self::Problem(problem) => {
                problem.scope == admission.scope
                    && problem.matches_terminal(&admission.request_id)
                    && problem.matches_admission(&admission.request, &admission.request_id)
            }
        }
    }

    pub(crate) fn run_result(&self) -> Option<&AutomationRunResultV1> {
        let Self::Outcome { outcome, .. } = self else {
            return None;
        };
        let ApplicationOutcome::Effect(effect) = outcome else {
            return None;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return None;
        };
        Some(result)
    }

    pub(crate) fn problem(&self) -> Option<&AutomationSettledProblem> {
        match self {
            Self::Outcome { .. } => None,
            Self::Problem(problem) => Some(problem),
        }
    }

    pub(crate) fn is_completed(&self) -> bool {
        let Self::Outcome { outcome, .. } = self else {
            return false;
        };
        let ApplicationOutcome::Effect(effect) = outcome else {
            return false;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return false;
        };
        matches!(result.terminal, AutomationRunTerminalV1::Completed { .. })
    }

    fn is_retirement_terminal(&self) -> bool {
        let Self::Outcome { outcome, .. } = self else {
            return false;
        };
        let ApplicationOutcome::Effect(effect) = outcome else {
            return false;
        };
        let Some(RetainedSurfaceResultV1::FactStoreCurate(result)) = effect.payload.as_ref() else {
            return false;
        };
        matches!(
            &result.terminal,
            AutomationRunTerminalV1::Skipped { reason, .. }
                if Some(*reason)
                    == AutomationSkipReasonV1::from_ledger_reason(
                        "shipped_fact_proposal_history_retired"
                    )
        ) && result.committed_receipts.is_empty()
    }
}

pub(crate) enum AutomationEffectAdmission {
    Execute(AutomationEffectAuthority),
    Replay(AutomationSettledTerminal),
    /// The registered application request was cancelled or timed out before
    /// the durable automation admission. This is deliberately not written to
    /// the automation journal: it is a pre-admission application problem.
    PreAdmissionProblem(ApplicationProblemEnvelope),
}

pub(crate) use input::{
    combined_review_run_request, memory_curator_run_request, session_reflector_run_request,
    skill_writer_run_request, user_job_run_request,
};
pub(crate) use recovery_index::{
    AutomationEffectRecoveryReport, reconcile_reserved_automation_effects_for_project,
};

pub(crate) fn pinned_automation_configuration_digest(
    revision: &ConfigurationRevisionId,
    behavior: &ManifestDigest,
    provenance: &ManifestDigest,
) -> Result<ManifestDigest> {
    digest(&(
        "tracedecay.automation.configuration.v1",
        revision,
        behavior,
        provenance,
    ))
}

impl AutomationEffectAuthority {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare(
        invocation: &DaemonInvocationService,
        memory: &crate::tracedecay::TraceDecay,
        project_root: &Path,
        dashboard_root: &Path,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: &CancellationSignal,
        observed_at: UtcMicros,
        configuration_digest: ManifestDigest,
        request: AutomationRunRequestV1,
    ) -> Result<AutomationEffectAdmission> {
        if !request.validate() {
            return Err(contract_error("automation run identity is empty"));
        }
        let journal_key = digest(&("tracedecay.automation-run.terminal-key.v1", &request.run_id))?;
        let journal_path = dashboard_root.join("automation_effects").join(format!(
            "{}.json",
            journal_key.as_str().trim_start_matches("sha256:")
        ));
        let task = request.task_kind();
        let classification = retirement::classify_for_task(task, dashboard_root).await?;
        let (retained_binding, retained_reset_digest) =
            if task == AutomationTaskV1::SessionReflector {
                let binding_path = journal_path.clone();
                tokio::task::spawn_blocking(move || retained_source_bindings(&binding_path))
                    .await
                    .map_err(|error| {
                        contract_error(format!(
                            "automation retirement binding reader failed: {error}"
                        ))
                    })??
            } else {
                (None, None)
            };
        let (live_retirement, shipped_reset) = match classification {
            retirement::RetirementClassification::Absent => (None, None),
            retirement::RetirementClassification::ResetRequired {
                source_digest,
                detail,
            } => (None, Some((source_digest, detail))),
            retirement::RetirementClassification::Terminal(plan) => (Some(plan), None),
        };
        if let (Some(plan), Some(binding)) = (&live_retirement, &retained_binding) {
            retirement::verify_plan_matches_binding(plan, binding)?;
        }
        let retirement_binding =
            retained_binding.or_else(|| live_retirement.as_ref().map(|plan| plan.binding.clone()));
        let reset_source_digest = match (retained_reset_digest, shipped_reset.as_ref()) {
            (Some(stored), Some((live, _))) if stored != *live => {
                return Err(contract_error(
                    "unresolved shipped proposal bytes changed after durable admission",
                ));
            }
            (Some(stored), _) => Some(stored),
            (None, Some((live, _))) => Some(live.clone()),
            (None, None) => None,
        };
        let retained_operation = RetainedSurfaceOperation::FactStoreCurate;
        let operation =
            retained_surface_application_operation(retained_operation).map_err(contract_error)?;
        let context = match invocation
            .registered_retained_request_context(
                project_root,
                request_id.clone(),
                deadline,
                cancellation.context(),
                observed_at,
                &operation,
            )
            .await
        {
            Ok(context) => context,
            Err(RegisteredRetainedRequestContextError::Application(problem)) => {
                let envelope = ApplicationProblemEnvelope::new(
                    operation.result_contract().clone(),
                    request_id,
                    problem,
                )
                .map_err(contract_error)?;
                return Ok(AutomationEffectAdmission::PreAdmissionProblem(envelope));
            }
            Err(RegisteredRetainedRequestContextError::Runtime(error)) => return Err(error),
        };
        let input_digest = digest(&(
            "tracedecay.automation-run.input.v1",
            operation.use_case_id(),
            context.actor(),
            context.scope(),
            &configuration_digest,
            &request,
            &retirement_binding,
            &reset_source_digest,
        ))?;
        let execution_cancellation = cancellation.clone();
        let execution = RetainedSurfaceExecutionContextV1 {
            request_context: &context,
            cancellation_signal: &execution_cancellation,
            operation: &operation,
            observed_at,
        };
        let prepared = prepare_retained_effect(
            &execution,
            retained_operation,
            &configuration_digest,
            &(&request, &retirement_binding, &reset_source_digest),
            request.run_id.as_str(),
        )
        .map_err(|_| contract_error("canonical memory automation effect preparation failed"))?;
        let placeholder = digest(&"tracedecay.automation-run.uncommitted.v1")?;
        let RetainedSurfaceExecutionErrorV1::PartialEffect {
            mut committed_receipt,
            ..
        } = prepared.partial_error_with_digest(
            &placeholder,
            "application.automation-run.recovery-template",
            "Durable automation recovery receipt template.",
        )
        else {
            return Err(contract_error(
                "canonical memory automation recovery template is unavailable",
            ));
        };
        committed_receipt.committed_state = None;
        let effect_authority_digest = digest(&(
            "tracedecay.automation-run.effect-authority.v1",
            context.actor(),
            context.scope(),
            &context.grant().grant_id,
            context.grant().revision,
            &context.grant().digest,
            context.grant().disclosure,
            operation.capability_id(),
            operation.use_case_id(),
            operation.result_contract(),
            &configuration_digest,
            &input_digest,
            &request,
            &committed_receipt,
        ))?;
        let recovery_problem = if matches!(
            task,
            AutomationTaskV1::MemoryCurator | AutomationTaskV1::SessionReflector
        ) {
            reset_required_problem(&operation, &context, &request)?
        } else {
            indeterminate_external_effect_problem(&operation, &context, &request)?
        };
        let recovery = if matches!(
            task,
            AutomationTaskV1::MemoryCurator | AutomationTaskV1::SessionReflector
        ) {
            AutomationRecoveryBinding::Memory {
                owner: memory.project_memory_owner()?,
                recovery_problem,
                retirement: retirement_binding,
                reset_source_digest,
            }
        } else {
            if retirement_binding.is_some() || reset_source_digest.is_some() {
                return Err(contract_error(
                    "external automation admission carried memory retirement state",
                ));
            }
            AutomationRecoveryBinding::External { recovery_problem }
        };
        let admission = DurableAutomationAdmission {
            schema_version: 1,
            request,
            input_digest,
            configuration_digest,
            effect_authority_digest,
            grant_id: context.grant().grant_id.clone(),
            grant_revision: context.grant().revision,
            grant_digest: context.grant().digest.clone(),
            disclosure: context.grant().disclosure,
            effect_receipt_template: committed_receipt,
            actor: context.actor().clone(),
            scope: context.scope().clone(),
            request_id: context.request_id().clone(),
            process_run_id: crate::runtime_identity::process_run_id().to_owned(),
            recovery,
        };
        let reserve_path = journal_path.clone();
        let requested = admission.clone();
        let index_root = dashboard_root.to_path_buf();
        let index_path = journal_path.clone();
        let indexed = admission.clone();
        let reservation = tokio::task::spawn_blocking(move || {
            recovery_index::add_pending_blocking(&index_root, &index_path, &indexed)?;
            reserve_or_replay_blocking(&reserve_path, requested)
        })
        .await
        .map_err(|error| {
            contract_error(format!("automation reservation writer failed: {error}"))
        })??;
        match reservation {
            ReservationResult::Replay {
                terminal,
                retirement,
            } => {
                remove_pending_index(dashboard_root, &journal_path).await?;
                if let Some(binding) = retirement {
                    if terminal.is_retirement_terminal() {
                        finalize_retirement(dashboard_root, binding, live_retirement).await?;
                    } else if terminal.problem().is_none() {
                        return Err(contract_error(
                            "retirement-bound automation replay is neither its zero-effect terminal nor a typed recovery problem",
                        ));
                    }
                }
                Ok(AutomationEffectAdmission::Replay(terminal))
            }
            ReservationResult::Execute { retirement } => {
                let authority = Self {
                    context,
                    cancellation: cancellation.clone(),
                    operation,
                    prepared,
                    admission,
                    journal_path,
                    dashboard_root: dashboard_root.to_path_buf(),
                };
                if let Some((_digest, _detail)) = shipped_reset {
                    let problem = shipped_proposal_reset_required_problem(
                        &authority.operation,
                        &authority.context,
                        &authority.admission.request,
                    )?;
                    let terminal = authority
                        .persist_terminal(AutomationSettledTerminal::Problem(problem))
                        .await?;
                    Ok(AutomationEffectAdmission::Replay(terminal))
                } else if let Some(binding) = retirement {
                    let terminal = authority.settle_retirement().await?;
                    finalize_retirement(dashboard_root, binding, live_retirement).await?;
                    Ok(AutomationEffectAdmission::Replay(terminal))
                } else {
                    Ok(AutomationEffectAdmission::Execute(authority))
                }
            }
            ReservationResult::Recover { retirement } => {
                let authority = Self {
                    context,
                    cancellation: cancellation.clone(),
                    operation,
                    prepared,
                    admission,
                    journal_path,
                    dashboard_root: dashboard_root.to_path_buf(),
                };
                if authority.admission.is_external() {
                    let terminal = authority
                        .persist_recovered_terminal(AutomationSettledTerminal::Problem(
                            authority.admission.recovery_problem().clone(),
                        ))
                        .await?;
                    return Ok(AutomationEffectAdmission::Replay(terminal));
                }
                let recovery_cancellation = cancellation.clone();
                let read_control =
                    FactReadControl::new(Arc::new(move || recovery_cancellation.is_cancelled()));
                let recovered = memory
                    .project_memory_application()
                    .await?
                    .project_memory_automation_run_receipts(
                        authority.admission.request.run_id.clone(),
                        &read_control,
                    )
                    .await
                    .map_err(|error| {
                        contract_error(format!(
                            "canonical memory automation receipt recovery failed: {error}"
                        ))
                    })?;
                let committed_receipts =
                    project_recovered_committed_receipts(&authority.admission.request, &recovered)?;
                if retirement.is_some() && !committed_receipts.is_empty() {
                    return Err(contract_error(
                        "proposal retirement recovery found unrelated canonical memory commits",
                    ));
                }
                let terminal = if retirement.is_some() {
                    authority.settle_recovered_retirement().await?
                } else if !committed_receipts.is_empty() {
                    authority
                        .persist_recovered_terminal(recovery_index::recovered_partial_terminal(
                            &authority.admission,
                            committed_receipts,
                            &authority.operation,
                        )?)
                        .await?
                } else if authority.admission.reset_source_digest().is_some() {
                    let problem = shipped_proposal_reset_required_problem(
                        &authority.operation,
                        &authority.context,
                        &authority.admission.request,
                    )?;
                    authority
                        .persist_recovered_terminal(AutomationSettledTerminal::Problem(problem))
                        .await?
                } else {
                    let terminal = AutomationSettledTerminal::Problem(
                        authority.admission.recovery_problem().clone(),
                    );
                    authority.persist_recovered_terminal(terminal).await?
                };
                if let Some(binding) = retirement {
                    if !terminal.is_retirement_terminal() {
                        return Err(contract_error(
                            "proposal retirement recovery did not produce its exact terminal",
                        ));
                    }
                    finalize_retirement(dashboard_root, binding, live_retirement).await?;
                }
                Ok(AutomationEffectAdmission::Replay(terminal))
            }
        }
    }

    pub(crate) async fn settle_run(
        &self,
        ledger: &AutomationRunLedgerRecord,
        committed: Option<&AutomationCommittedReceipt>,
    ) -> Result<AutomationSettledTerminal> {
        if ledger.run_id != self.admission.request.run_id.as_str() {
            return Err(contract_error(
                "automation ledger identity changed before settlement",
            ));
        }
        let committed_receipts = committed
            .map(|receipt| project_committed_receipts(&self.admission.request, receipt))
            .transpose()?
            .unwrap_or_default();
        if ledger.status == AutomationRunStatus::Failed {
            if !committed_receipts.is_empty() {
                return Err(contract_error(
                    "a failed automation run carried canonical commits without a partial terminal",
                ));
            }
            let problem = self.problem_envelope(
                failed_ledger_problem(&self.context, &self.cancellation, ledger)?,
                Vec::new(),
            )?;
            return self
                .persist_terminal(AutomationSettledTerminal::Problem(problem))
                .await;
        }
        let terminal = match ledger.status {
            AutomationRunStatus::Succeeded => AutomationRunTerminalV1::Completed {
                summary: project_run_summary(
                    self.admission.request.task_kind(),
                    &committed_receipts,
                )?,
            },
            AutomationRunStatus::Skipped => {
                if ledger.error != ledger.fallback_status {
                    return Err(contract_error(
                        "skipped automation ledger reason disagrees with its fallback status",
                    ));
                }
                AutomationRunTerminalV1::Skipped {
                    reason: project_skip_reason(ledger.error.as_deref().ok_or_else(|| {
                        contract_error("skipped automation terminal has no exact reason")
                    })?)?,
                    summary: AutomationRunSummaryV1 {
                        reviewed_count: 0,
                        accepted_count: 0,
                        rejected_count: 0,
                        skipped_count: 1,
                    },
                }
            }
            _ => {
                return Err(contract_error(
                    "successful automation settlement requires a terminal success or skip status",
                ));
            }
        };
        if matches!(terminal, AutomationRunTerminalV1::Skipped { .. })
            && !committed_receipts.is_empty()
        {
            return Err(contract_error(
                "a skipped automation run carried committed receipts",
            ));
        }
        let result = AutomationRunResultV1 {
            run_id: self.admission.request.run_id.clone(),
            task: self.admission.request.task_kind(),
            request_digest: self
                .admission
                .request
                .input_digest()
                .map_err(contract_error)?,
            terminal,
            committed_receipts,
        };
        self.persist_success_result(result).await
    }

    async fn settle_retirement(&self) -> Result<AutomationSettledTerminal> {
        self.persist_success_result(self.retirement_result()?).await
    }

    async fn settle_recovered_retirement(&self) -> Result<AutomationSettledTerminal> {
        self.persist_success_result_with(self.retirement_result()?, true)
            .await
    }

    fn retirement_result(&self) -> Result<AutomationRunResultV1> {
        let reason =
            AutomationSkipReasonV1::from_ledger_reason("shipped_fact_proposal_history_retired")
                .ok_or_else(|| {
                    contract_error("shipped proposal retirement reason is not registered")
                })?;
        Ok(AutomationRunResultV1 {
            run_id: self.admission.request.run_id.clone(),
            task: self.admission.request.task_kind(),
            request_digest: self
                .admission
                .request
                .input_digest()
                .map_err(contract_error)?,
            terminal: AutomationRunTerminalV1::Skipped {
                reason,
                summary: AutomationRunSummaryV1 {
                    reviewed_count: 0,
                    accepted_count: 0,
                    rejected_count: 0,
                    skipped_count: 1,
                },
            },
            committed_receipts: Vec::new(),
        })
    }

    async fn persist_success_result(
        &self,
        result: AutomationRunResultV1,
    ) -> Result<AutomationSettledTerminal> {
        self.persist_success_result_with(result, false).await
    }

    async fn persist_success_result_with(
        &self,
        result: AutomationRunResultV1,
        recovered: bool,
    ) -> Result<AutomationSettledTerminal> {
        if !result.matches_terminal() {
            return Err(contract_error(
                "memory automation success terminal is inconsistent",
            ));
        }
        let committed_state = self
            .prepared
            .material_committed_state_digest(&result)
            .map_err(|_| contract_error("memory automation committed state is invalid"))?;
        let execution = RetainedSurfaceExecutionContextV1 {
            request_context: &self.context,
            cancellation_signal: &self.cancellation,
            operation: &self.operation,
            observed_at: tracedecay_application::now_micros(),
        };
        let committed_outer_result = result.clone();
        let outcome = self.prepared.complete_with_digest(
            &execution,
            &committed_state,
            tracedecay_application::ReconciliationState::Reconciled,
            RetainedSurfaceResultV1::FactStoreCurate(result),
            None,
        );
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(RetainedSurfaceExecutionErrorV1::PartialEffect {
                reason_code,
                committed_receipt,
                detail,
            }) => {
                let terminal = self.outer_result_partial_problem(
                    reason_code,
                    committed_receipt,
                    detail,
                    committed_outer_result,
                )?;
                return if recovered {
                    self.persist_recovered_terminal(AutomationSettledTerminal::Problem(terminal))
                        .await
                } else {
                    self.persist_terminal(AutomationSettledTerminal::Problem(terminal))
                        .await
                };
            }
            Err(_) => {
                return Err(contract_error(
                    "canonical memory automation effect completion failed before a typed post-commit terminal",
                ));
            }
        };
        if !retained_surface_outcome_matches_terminal(
            RetainedSurfaceOperation::FactStoreCurate,
            self.context.request_id(),
            self.context.scope(),
            &outcome,
        ) {
            return Err(contract_error(
                "memory automation outcome does not match its registered admission",
            ));
        }
        let terminal = AutomationSettledTerminal::Outcome {
            scope: self.context.scope().clone(),
            outcome,
        };
        if recovered {
            self.persist_recovered_terminal(terminal).await
        } else {
            self.persist_terminal(terminal).await
        }
    }

    fn outer_result_partial_problem(
        &self,
        reason_code: String,
        committed_receipt: tracedecay_application::EffectReceipt,
        detail: String,
        committed_outer_result: AutomationRunResultV1,
    ) -> Result<AutomationSettledProblem> {
        let problem =
            retained_surface_execution_problem(RetainedSurfaceExecutionErrorV1::PartialEffect {
                reason_code,
                committed_receipt,
                detail,
            });
        problem.validate().map_err(contract_error)?;
        let envelope = ApplicationProblemEnvelope::new(
            self.operation.result_contract().clone(),
            self.context.request_id().clone(),
            problem,
        )
        .map(|problem| problem.with_owning_layer(ProblemOwningLayer::Application))
        .map_err(contract_error)?;
        AutomationRunProblemV1::new_outer_effect_partial(
            &self.admission.request,
            self.context.scope().clone(),
            envelope,
            committed_outer_result,
            self.context.request_id(),
        )
        .map_err(contract_error)
    }

    /// Projects an admitted automation failure into the canonical application
    /// terminal. Cancellation, deadline, execution, reset, and partial-effect
    /// states are never flattened into runner strings after reservation.
    pub(crate) async fn settle_problem(
        self,
        error: &AutomationRunError,
    ) -> Result<AutomationSettledProblem> {
        let problem = match error {
            AutomationRunError::PartialEffect {
                run_id,
                committed_receipt,
                detail,
                ..
            } => self.settle_partial(run_id, committed_receipt, detail)?,
            AutomationRunError::Runtime(error) => self.problem_envelope(
                runtime_problem(&self.context, &self.cancellation, error)?,
                Vec::new(),
            )?,
        };
        let terminal = self
            .persist_terminal(AutomationSettledTerminal::Problem(problem))
            .await?;
        match terminal {
            AutomationSettledTerminal::Problem(problem) => Ok(problem),
            AutomationSettledTerminal::Outcome { .. } => Err(contract_error(
                "automation problem settlement replayed a successful terminal",
            )),
        }
    }

    pub(crate) async fn abandon_uncommitted(self) -> Result<()> {
        let path = self.journal_path;
        let admission = self.admission;
        let dashboard_root = self.dashboard_root;
        let index_path = path.clone();
        tokio::task::spawn_blocking(move || {
            abandon_reservation_blocking(&path, &admission)?;
            recovery_index::remove_pending_blocking(&dashboard_root, &index_path)
        })
        .await
        .map_err(|error| {
            contract_error(format!("automation reservation rollback failed: {error}"))
        })?
    }

    fn settle_partial(
        &self,
        run_id: &str,
        committed: &AutomationCommittedReceipt,
        detail: &'static str,
    ) -> Result<AutomationSettledProblem> {
        if run_id != self.admission.request.run_id.as_str() {
            return Err(contract_error(
                "automation run identity changed before settlement",
            ));
        }
        let committed_receipts = project_committed_receipts(&self.admission.request, committed)?;
        if committed_receipts.is_empty() {
            return Err(contract_error(
                "zero committed memory effects cannot produce a partial-effect terminal",
            ));
        }
        let committed_state = digest(&(
            "tracedecay.automation-run.partial-state.v1",
            run_id,
            &committed_receipts,
        ))?;
        let problem = retained_surface_execution_problem(self.prepared.partial_error_with_digest(
            &committed_state,
            "application.automation-run.partial-effect",
            detail,
        ));
        if !retained_surface_problem_matches_terminal(
            RetainedSurfaceOperation::FactStoreCurate,
            self.context.request_id(),
            Some(self.context.scope()),
            &problem,
        ) {
            return Err(contract_error(
                "memory automation partial terminal does not match its admission",
            ));
        }
        self.problem_envelope(problem, committed_receipts)
    }

    fn problem_envelope(
        &self,
        problem: ApplicationProblem,
        committed_receipts: Vec<AutomationCommittedReceiptV1>,
    ) -> Result<AutomationSettledProblem> {
        problem.validate().map_err(contract_error)?;
        let problem = ApplicationProblemEnvelope::new(
            self.operation.result_contract().clone(),
            self.context.request_id().clone(),
            problem,
        )
        .map(|problem| problem.with_owning_layer(ProblemOwningLayer::Application))
        .map_err(contract_error)?;
        AutomationRunProblemV1::new(
            &self.admission.request,
            self.context.scope().clone(),
            problem,
            committed_receipts,
            self.context.request_id(),
        )
        .map_err(contract_error)
    }

    async fn persist_terminal(
        &self,
        terminal: AutomationSettledTerminal,
    ) -> Result<AutomationSettledTerminal> {
        let path = self.journal_path.clone();
        let admission = self.admission.clone();
        let dashboard_root = self.dashboard_root.clone();
        let index_path = path.clone();
        tokio::task::spawn_blocking(move || {
            let terminal = persist_terminal_blocking(&path, &admission, terminal)?;
            recovery_index::remove_pending_blocking(&dashboard_root, &index_path)?;
            Ok(terminal)
        })
        .await
        .map_err(|error| contract_error(format!("automation terminal writer failed: {error}")))?
    }

    async fn persist_recovered_terminal(
        &self,
        terminal: AutomationSettledTerminal,
    ) -> Result<AutomationSettledTerminal> {
        let path = self.journal_path.clone();
        let admission = self.admission.clone();
        let dashboard_root = self.dashboard_root.clone();
        let index_path = path.clone();
        let cancellation = self.cancellation.clone();
        tokio::task::spawn_blocking(move || {
            let terminal = persist_recovered_terminal_blocking(
                &path,
                &admission,
                terminal,
                Some(&cancellation),
            )?
            .ok_or_else(|| contract_error("automation recovery settlement was cancelled"))?;
            recovery_index::remove_pending_blocking(&dashboard_root, &index_path)?;
            Ok(terminal)
        })
        .await
        .map_err(|error| {
            contract_error(format!(
                "automation recovery terminal writer failed: {error}"
            ))
        })?
    }
}

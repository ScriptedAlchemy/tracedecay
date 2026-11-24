//! The automation runtime's admitted handle on registered run-state history.
//!
//! Run metadata lives only in the project's registered store; the dashboard
//! root is retained for artifact content, the effect journal, and task locks,
//! whose size or external-effect contracts keep them on the filesystem.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::{
    AutomationRunStagePublishOutcomeV1, RegisteredGlobalDb, RegisteredGlobalDbLeaseV1,
};

use super::backend::{AgentTaskKind, task_key as canonical_task_key};
use super::config_error;
use super::exact_run::{
    ExactRunPublication, ExactRunPublishOutcome, ExactRunUnboundDiscardOutcome,
};
use super::run_ledger::{
    AutomationRunLedgerPageV1, AutomationRunLedgerRecord, AutomationRunLedgerTaskSummary,
    validate_run_id_component, validate_run_ledger_record_semantics,
};

#[cfg(test)]
pub(crate) mod tests;

/// Retired file-ledger artifacts. Their presence means run history this
/// store never imported; run-state operations refuse rather than silently
/// starting an empty history beside it.
const LEGACY_RUN_LEDGER_PATHS: [&str; 5] = [
    "automation_runs.jsonl",
    "automation_runs.jsonl.append-intent",
    "automation_run_spool",
    "automation_run_spool.lock",
    "automation_run_append_intent_quarantine",
];

/// Which nested validation-report cursor a task's newest run resumes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationValidationCursorV1 {
    /// `validation_report.pagination.resume_after_fact_id`.
    ResumeAfterFactId,
}

/// Run-state authority bound to one admitted project shard.
#[derive(Clone)]
pub struct RegisteredAutomationRunStore {
    database: RegisteredGlobalDbLeaseV1,
    dashboard_root: PathBuf,
}

impl std::fmt::Debug for RegisteredAutomationRunStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredAutomationRunStore")
            .field("dashboard_root", &self.dashboard_root)
            .finish_non_exhaustive()
    }
}

impl RegisteredAutomationRunStore {
    /// Admits the project's registered store as its run-state authority.
    ///
    /// The namespace comes only from the lease's admitted shard. A dashboard
    /// root still holding the retired file ledger is refused for run-state
    /// operations; its bytes are left untouched.
    pub fn admit(database: RegisteredGlobalDbLeaseV1, dashboard_root: PathBuf) -> Result<Self> {
        refuse_legacy_run_ledger(&dashboard_root)?;
        Ok(Self {
            database,
            dashboard_root,
        })
    }

    /// Artifact, effect-journal, and task-lock root of this project.
    pub fn dashboard_root(&self) -> &Path {
        &self.dashboard_root
    }

    pub fn database(&self) -> &RegisteredGlobalDb {
        &self.database
    }

    /// Publishes one run transition; an identical replay is a no-op and a
    /// conflicting same-status replay is refused.
    #[tracing::instrument(name = "automation.run_store.append", level = "trace", skip_all)]
    pub async fn append_run_record(&self, record: &AutomationRunLedgerRecord) -> Result<()> {
        validate_run_ledger_record_semantics(record)?;
        self.database
            .publish_automation_run(record)
            .await
            .map(|_| ())
    }

    #[tracing::instrument(name = "automation.run_store.find", level = "trace", skip_all)]
    pub async fn find_run_record(&self, run_id: &str) -> Result<Option<AutomationRunLedgerRecord>> {
        validate_run_id_component(run_id)?;
        self.database.automation_run_exact(run_id).await
    }

    /// Up to `limit` most recently published runs, newest first.
    pub async fn load_run_records(&self, limit: usize) -> Result<Vec<AutomationRunLedgerRecord>> {
        self.load_run_records_page(limit)
            .await
            .map(|page| page.records)
    }

    #[tracing::instrument(name = "automation.run_store.page", level = "trace", skip_all)]
    pub async fn load_run_records_page(&self, limit: usize) -> Result<AutomationRunLedgerPageV1> {
        let page = self.database.automation_run_page(limit).await?;
        Ok(AutomationRunLedgerPageV1 {
            records: page.records,
            has_more: page.has_more,
        })
    }

    pub async fn load_run_records_for_task_key(
        &self,
        task_key: &str,
        limit: usize,
    ) -> Result<Vec<AutomationRunLedgerRecord>> {
        validate_requested_task_key(task_key)?;
        self.database
            .automation_run_records_for_task_key(task_key, limit)
            .await
    }

    /// The six scheduler selections for one exact task identity.
    #[tracing::instrument(name = "automation.run_store.task_summary", level = "trace", skip_all)]
    pub async fn load_run_ledger_task_summary(
        &self,
        task: AgentTaskKind,
        task_key: &str,
    ) -> Result<AutomationRunLedgerTaskSummary> {
        validate_requested_task_key(task_key)?;
        if task_key != canonical_task_key(task) && !task_key.starts_with("user_job:") {
            return Err(config_error(
                "automation task summary requires an exact canonical task identity",
            ));
        }
        self.database
            .automation_run_task_summary(task, task_key)
            .await
            .map(AutomationRunLedgerTaskSummary::from_selections)
    }

    pub async fn load_latest_scheduler_effectful_for_task_key(
        &self,
        task_key: &str,
    ) -> Result<Option<AutomationRunLedgerRecord>> {
        if !task_key.starts_with("user_job:") {
            return Err(config_error(
                "scheduler effectful task-key lookup requires a UserJob task key",
            ));
        }
        self.load_run_ledger_task_summary(AgentTaskKind::UserJob, task_key)
            .await
            .map(|summary| summary.latest_scheduler_effectful().cloned())
    }

    /// The newest run's validation cursor for `task_key`, when one recorded
    /// it. An explicit JSON `null` cursor is the newest answer, not absence.
    pub async fn load_latest_task_validation_cursor(
        &self,
        task_key: &str,
        cursor: AutomationValidationCursorV1,
    ) -> Result<Option<Value>> {
        validate_requested_task_key(task_key)?;
        match cursor {
            AutomationValidationCursorV1::ResumeAfterFactId => {
                self.database
                    .automation_run_validation_cursor(task_key)
                    .await
            }
        }
    }

    /// Appends a scheduler diagnostic or reuses the exact durable one,
    /// validating the persistent anchor in the same transaction.
    pub async fn append_or_reuse_scheduler_diagnostic(
        &self,
        candidate: &AutomationRunLedgerRecord,
        effectful_anchor_run_id: Option<&str>,
    ) -> Result<AutomationRunLedgerRecord> {
        validate_run_ledger_record_semantics(candidate)?;
        self.database
            .append_or_reuse_automation_scheduler_diagnostic(candidate, effectful_anchor_run_id)
            .await
    }

    /// Stages `record`'s exact terminal, then runs `bind` to make that
    /// identity durable in the external-effect journal.
    ///
    /// The staged row is invisible to history until
    /// [`Self::publish_staged_run_record_exact`]. Staging is re-asserted after
    /// `bind` so an abandoned-reservation sweep that interleaved before the
    /// journal bound this terminal cannot leave the binding without its row.
    #[tracing::instrument(name = "automation.run_store.bind_exact", level = "trace", skip_all)]
    pub async fn bind_staged_run_record_exact<T, F>(
        &self,
        record: &AutomationRunLedgerRecord,
        bind: F,
    ) -> Result<(ExactRunPublication, T)>
    where
        T: Send + 'static,
        F: FnOnce(&ExactRunPublication) -> Result<T> + Send + 'static,
    {
        validate_run_ledger_record_semantics(record)?;
        let staged = self.database.stage_automation_run_terminal(record).await?;
        let publication = ExactRunPublication::from_staged(staged);
        let binding = publication.clone();
        let bound =
            tracedecay_runtime_core::native_runtime::spawn_owned_blocking(move || bind(&binding))
                .await
                .map_err(|error| {
                    config_error(format!(
                        "exact automation run binding failed to run: {error}"
                    ))
                })??;
        self.database.stage_automation_run_terminal(record).await?;
        Ok((publication, bound))
    }

    /// Publishes the staged terminal `publication` bound; replaying an
    /// already committed publication succeeds without another transition.
    #[tracing::instrument(name = "automation.run_store.publish_exact", level = "trace", skip_all)]
    pub async fn publish_staged_run_record_exact(
        &self,
        run_id: &str,
        publication: &ExactRunPublication,
    ) -> Result<ExactRunPublishOutcome> {
        publication.validate()?;
        let outcome = self
            .database
            .publish_staged_automation_run_terminal(run_id, &publication.staged_for(run_id))
            .await?;
        Ok(match outcome {
            AutomationRunStagePublishOutcomeV1::Published => ExactRunPublishOutcome::Published,
            AutomationRunStagePublishOutcomeV1::MissingPayload => {
                ExactRunPublishOutcome::MissingPayload
            }
        })
    }

    /// Whether `publication` is the run's committed terminal.
    pub async fn is_exact_terminal_published(
        &self,
        run_id: &str,
        publication: &ExactRunPublication,
    ) -> Result<bool> {
        publication.validate()?;
        Ok(self
            .database
            .automation_run_terminal_digest(run_id)
            .await?
            .is_some_and(|digest| digest == publication.digest()))
    }

    /// Retires every staged terminal of `run_id` only while `validate_unbound`
    /// proves its reservation is abandoned and unbound.
    pub async fn discard_unbound_staged_run_records_if<F>(
        &self,
        run_id: &str,
        validate_unbound: F,
    ) -> Result<ExactRunUnboundDiscardOutcome>
    where
        F: FnOnce() -> Result<bool> + Send + 'static,
    {
        let unbound =
            tracedecay_runtime_core::native_runtime::spawn_owned_blocking(validate_unbound)
                .await
                .map_err(|error| {
                    config_error(format!(
                        "abandoned automation reservation validation failed to run: {error}"
                    ))
                })??;
        if !unbound {
            return Ok(ExactRunUnboundDiscardOutcome::Retained);
        }
        self.database
            .discard_unbound_staged_automation_run(run_id)
            .await?;
        Ok(ExactRunUnboundDiscardOutcome::Discarded)
    }
}

fn refuse_legacy_run_ledger(dashboard_root: &Path) -> Result<()> {
    for name in LEGACY_RUN_LEDGER_PATHS {
        let path = dashboard_root.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(TraceDecayError::project_route(
                    "automation_run_legacy_jsonl_refused",
                    false,
                    format!(
                        "automation run history at '{}' predates the registered run store and is not carried forward; remove it to start registered run history",
                        path.display()
                    ),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(TraceDecayError::from(error)),
        }
    }
    Ok(())
}

fn validate_requested_task_key(task_key: &str) -> Result<()> {
    if task_key.len() > tracedecay_domain::canonical_text::CANONICAL_TEXT_MAX_BYTES {
        Err(config_error(
            "requested automation task key exceeds its byte bound",
        ))
    } else {
        Ok(())
    }
}

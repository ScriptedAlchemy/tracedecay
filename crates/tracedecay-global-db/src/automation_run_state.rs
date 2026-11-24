//! Registered-store authority for automation run state.
//!
//! Automation runs are durable product history: every admitted run publishes
//! ordered lifecycle transitions, a current terminal state, and its canonical
//! record bytes. This module owns that state as rows in the existing registered
//! database, in the same writer-transaction authority every other durable
//! authority uses. It is not a second database: the namespace comes only from
//! the admitted logical shard, and the caller retains the artifact content
//! files whose size/streaming contract requires them.
//!
//! Semantics preserved from the retired append-only file ledger:
//! * A run's identity (`task`, `task_key`, `trigger`) is immutable.
//! * At most one publication exists per `(run_id, status)`. Re-publishing the
//!   same status with byte-identical canonical JSON is an idempotent replay;
//!   different bytes are a conflicting replay and are refused.
//! * Statuses advance only `queued -> running -> terminal`, or directly to a
//!   terminal state, and completion time never regresses.
//! * A stage row binds a terminal publication's digest before it is visible in
//!   the current/history rows, so recovery can replay or discard it exactly.
//!
//! Existing `automation_runs.jsonl` history is not imported or dual-written.
//! The runtime adapter refuses it with a feature-scoped route error before any
//! run-state operation; these rows start empty and never treat an empty table
//! as evidence that retired file history did not exist.

use tracedecay_contracts::automation::{
    AgentTaskKind, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use crate::RegisteredGlobalDb;

/// Canonical physcial DDL for the automation run-state authority.
///
/// Installed through the registered schema transaction with the rest of the
/// store's tables. `automation_run_transitions` is the immutable ordered
/// history; `automation_runs` is the current-state projection plus the newest
/// canonical record bytes, so exact lookup and bounded history never scan
/// unrelated runs.
pub const AUTOMATION_RUN_STATE_SCHEMA_V1: &str = "
    CREATE TABLE IF NOT EXISTS automation_runs (
        namespace TEXT NOT NULL CHECK(length(namespace) > 0),
        run_id TEXT NOT NULL CHECK(length(run_id) > 0),
        task TEXT NOT NULL,
        task_key TEXT NOT NULL CHECK(length(task_key) > 0),
        trigger TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN (
            'queued', 'running', 'succeeded', 'failed', 'skipped'
        )),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        completed_at_seconds INTEGER NOT NULL CHECK(completed_at_seconds >= 0),
        completed_at_micros INTEGER NOT NULL CHECK(completed_at_micros >= 0),
        newest_sequence INTEGER NOT NULL CHECK(newest_sequence > 0),
        published_at_micros INTEGER NOT NULL,
        PRIMARY KEY(namespace, run_id)
    ) STRICT;

    CREATE TABLE IF NOT EXISTS automation_run_transitions (
        namespace TEXT NOT NULL,
        run_id TEXT NOT NULL CHECK(length(run_id) > 0),
        status TEXT NOT NULL CHECK(status IN (
            'queued', 'running', 'succeeded', 'failed', 'skipped'
        )),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        completed_at_seconds INTEGER NOT NULL CHECK(completed_at_seconds >= 0),
        completed_at_micros INTEGER NOT NULL CHECK(completed_at_micros >= 0),
        published_sequence INTEGER NOT NULL CHECK(published_sequence > 0),
        published_at_micros INTEGER NOT NULL,
        PRIMARY KEY(namespace, run_id, status),
        UNIQUE(namespace, published_sequence)
    ) STRICT;

    CREATE TABLE IF NOT EXISTS automation_run_sequence (
        namespace TEXT NOT NULL PRIMARY KEY CHECK(length(namespace) > 0),
        next_sequence INTEGER NOT NULL CHECK(next_sequence > 0)
    ) STRICT;

    CREATE TABLE IF NOT EXISTS automation_run_staging (
        namespace TEXT NOT NULL,
        run_id TEXT NOT NULL CHECK(length(run_id) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        staged_at_micros INTEGER NOT NULL,
        PRIMARY KEY(namespace, run_id)
    ) STRICT;

    CREATE INDEX IF NOT EXISTS idx_automation_runs_recent
        ON automation_runs(namespace, newest_sequence DESC);

    CREATE INDEX IF NOT EXISTS idx_automation_runs_task
        ON automation_runs(
            namespace, task, task_key,
            completed_at_seconds DESC, completed_at_micros DESC,
            newest_sequence DESC
        );

    CREATE INDEX IF NOT EXISTS idx_automation_runs_terminal
        ON automation_runs(
            namespace, status,
            completed_at_seconds DESC, completed_at_micros DESC,
            newest_sequence DESC
        );

    CREATE INDEX IF NOT EXISTS idx_automation_run_transitions_run
        ON automation_run_transitions(namespace, run_id, published_sequence);

    CREATE INDEX IF NOT EXISTS idx_automation_runs_validation_cursor
        ON automation_runs(
            namespace, task_key,
            completed_at_seconds DESC, completed_at_micros DESC
        )
        WHERE task_key IS NOT NULL;

    CREATE TRIGGER IF NOT EXISTS automation_run_transitions_immutable
    BEFORE UPDATE ON automation_run_transitions BEGIN
        SELECT RAISE(ABORT, 'automation run transitions are immutable');
    END;

    CREATE TRIGGER IF NOT EXISTS automation_run_transitions_no_delete
    BEFORE DELETE ON automation_run_transitions BEGIN
        SELECT RAISE(ABORT, 'automation run transitions are retained');
    END;

    CREATE TRIGGER IF NOT EXISTS automation_run_staging_no_update
    BEFORE UPDATE ON automation_run_staging BEGIN
        SELECT RAISE(ABORT, 'automation run staging rows are immutable');
    END;

    CREATE TRIGGER IF NOT EXISTS automation_runs_identity_immutable
    BEFORE UPDATE ON automation_runs
    WHEN OLD.namespace != NEW.namespace
      OR OLD.run_id != NEW.run_id
      OR OLD.task != NEW.task
      OR OLD.task_key != NEW.task_key
      OR OLD.trigger != NEW.trigger
    BEGIN
        SELECT RAISE(ABORT, 'automation run identity is immutable');
    END;
";

/// Whether one publication advanced the run or replayed an identical one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationRunPublishOutcomeV1 {
    /// A new transition became the run's current state.
    Published { sequence: i64 },
    /// The same status was re-published with byte-identical canonical JSON.
    Replayed { sequence: i64 },
}

/// The digest a staged terminal publication bound before it became visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationRunStagedPublicationV1 {
    pub run_id: String,
    /// Canonical SHA-256 of the staged record.
    pub digest: String,
}

/// Whether a staged publication still had a payload to publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationRunStagePublishOutcomeV1 {
    Published,
    MissingPayload,
}

/// The newest record for each scheduler-summary selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutomationRunTaskSummaryV1 {
    pub latest_logical_activity: Option<AutomationRunLedgerRecord>,
    pub latest_successful: Option<AutomationRunLedgerRecord>,
    pub latest_effectful_any_trigger: Option<AutomationRunLedgerRecord>,
    pub latest_scheduler_effectful: Option<AutomationRunLedgerRecord>,
    pub latest_scheduler_activity: Option<AutomationRunLedgerRecord>,
    pub latest_session_evidence_budget_exhausted: Option<AutomationRunLedgerRecord>,
}

/// One bounded newest-first page of current run states.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutomationRunPageV1 {
    pub records: Vec<AutomationRunLedgerRecord>,
    pub has_more: bool,
}

impl RegisteredGlobalDb {
    /// The project namespace this registered store admits automation state for.
    ///
    /// A store with no project identity cannot own project automation history;
    /// that is a typed refusal rather than a shared default namespace.
    fn automation_run_namespace(&self) -> Result<&str> {
        self.binding()
            .shard_id
            .scope
            .project_id()
            .map(|project_id| project_id.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "registered store has no project identity for automation run state"
                    .to_owned(),
            })
    }

    /// Publishes one run record as the authority for its status.
    ///
    /// The write is one transaction: identity and lifecycle validation, the
    /// immutable transition, the current-state projection, and the newest
    /// record bytes commit together or not at all.
    pub async fn publish_automation_run(
        &self,
        record: &AutomationRunLedgerRecord,
    ) -> Result<AutomationRunPublishOutcomeV1> {
        validate_record(record)?;
        let namespace = self.automation_run_namespace()?.to_owned();
        let record = record.clone();
        self.transact_or(std::convert::identity, async move |transaction| {
            let namespace = namespace.as_str();
            let record = &record;
            publish_in_transaction(&transaction, namespace, record).await
        })
        .await
    }

    /// Exact current-state lookup by run id. One primary-key read; it never
    /// scans unrelated history.
    pub async fn automation_run_exact(
        &self,
        run_id: &str,
    ) -> Result<Option<AutomationRunLedgerRecord>> {
        let namespace = self.automation_run_namespace()?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read automation run state".to_owned(),
                message: error.to_string(),
            })?;
        let stored = read_current_json(&snapshot, namespace, run_id).await?;
        stored
            .map(|record_json| {
                serde_json::from_str(&record_json).map_err(|error| TraceDecayError::Database {
                    operation: "decode automation run state".to_owned(),
                    message: error.to_string(),
                })
            })
            .transpose()
    }

    /// Newest-first bounded history over current run states. Ordering is the
    /// explicit publication sequence index, so the read never loads all
    /// history to decide what is current.
    pub async fn automation_run_page(&self, limit: usize) -> Result<AutomationRunPageV1> {
        if limit == 0 {
            return Ok(AutomationRunPageV1::default());
        }
        let namespace = self.automation_run_namespace()?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read automation run page".to_owned(),
                message: error.to_string(),
            })?;
        let bound = i64::try_from(limit)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| TraceDecayError::Config {
                message: "automation run page limit overflowed".to_owned(),
            })?;
        let mut rows = snapshot
            .query(
                "SELECT record_json
                   FROM automation_runs
                  WHERE namespace = ?1
                  ORDER BY newest_sequence DESC
                  LIMIT ?2",
                params![namespace, bound],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query automation run page".to_owned(),
                message: error.to_string(),
            })?;
        let mut records = Vec::new();
        let mut has_more = false;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read automation run page".to_owned(),
                message: error.to_string(),
            })?
        {
            if records.len() == limit {
                has_more = true;
                break;
            }
            let record_json = row
                .get::<String>(0)
                .map_err(|error| TraceDecayError::Database {
                    operation: "decode automation run page".to_owned(),
                    message: error.to_string(),
                })?;
            records.push(serde_json::from_str(&record_json).map_err(|error| {
                TraceDecayError::Database {
                    operation: "decode automation run page".to_owned(),
                    message: error.to_string(),
                }
            })?);
        }
        Ok(AutomationRunPageV1 { records, has_more })
    }

    /// Newest records for one exact task key.
    pub async fn automation_run_records_for_task_key(
        &self,
        task_key: &str,
        limit: usize,
    ) -> Result<Vec<AutomationRunLedgerRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let namespace = self.automation_run_namespace()?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read automation runs by task key".to_owned(),
                message: error.to_string(),
            })?;
        let bound = i64::try_from(limit).map_err(|_| TraceDecayError::Config {
            message: "automation task-key limit overflowed".to_owned(),
        })?;
        let mut rows = snapshot
            .query(
                "SELECT record_json
                   FROM automation_runs
                  WHERE namespace = ?1 AND task_key = ?2
                  ORDER BY completed_at_seconds DESC,
                           completed_at_micros DESC,
                           newest_sequence DESC
                  LIMIT ?3",
                params![namespace, task_key, bound],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query automation runs by task key".to_owned(),
                message: error.to_string(),
            })?;
        let mut records = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read automation runs by task key".to_owned(),
                message: error.to_string(),
            })?
        {
            let record_json = row
                .get::<String>(0)
                .map_err(|error| TraceDecayError::Database {
                    operation: "decode automation runs by task key".to_owned(),
                    message: error.to_string(),
                })?;
            records.push(serde_json::from_str(&record_json).map_err(|error| {
                TraceDecayError::Database {
                    operation: "decode automation runs by task key".to_owned(),
                    message: error.to_string(),
                }
            })?);
        }
        Ok(records)
    }

    /// The six task-summary selections the scheduler and dashboard require.
    ///
    /// Each selection is one indexed query over the current-state projection;
    /// no selection walks full history to decide which run is newest.
    pub async fn automation_run_task_summary(
        &self,
        task: AgentTaskKind,
        task_key: &str,
    ) -> Result<AutomationRunTaskSummaryV1> {
        let namespace = self.automation_run_namespace()?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read automation task summary".to_owned(),
                message: error.to_string(),
            })?;
        let task_name = task_name(task);
        let mut summary = AutomationRunTaskSummaryV1::default();
        summary.latest_logical_activity = summary_row(
            &snapshot,
            namespace,
            task_name,
            task_key,
            SummaryFilter::Any,
        )
        .await?;
        summary.latest_successful = summary_row(
            &snapshot,
            namespace,
            task_name,
            task_key,
            SummaryFilter::Successful,
        )
        .await?;
        summary.latest_effectful_any_trigger = summary_row(
            &snapshot,
            namespace,
            task_name,
            task_key,
            SummaryFilter::Effectful,
        )
        .await?;
        summary.latest_scheduler_effectful = summary_row(
            &snapshot,
            namespace,
            task_name,
            task_key,
            SummaryFilter::SchedulerEffectful,
        )
        .await?;
        summary.latest_scheduler_activity = summary_row(
            &snapshot,
            namespace,
            task_name,
            task_key,
            SummaryFilter::Scheduler,
        )
        .await?;
        summary.latest_session_evidence_budget_exhausted = summary_row(
            &snapshot,
            namespace,
            task_name,
            task_key,
            SummaryFilter::SessionEvidenceBudgetExhausted,
        )
        .await?;
        Ok(summary)
    }

    /// The nested validation-report cursor of the newest run for one task key.
    ///
    /// The nested expression is the exact indexed lookup path, never a scan of
    /// every run's full report.
    pub async fn automation_run_validation_cursor(
        &self,
        task_key: &str,
    ) -> Result<Option<serde_json::Value>> {
        let namespace = self.automation_run_namespace()?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read automation validation cursor".to_owned(),
                message: error.to_string(),
            })?;
        let mut rows = snapshot
            .query(
                "SELECT json_extract(
                            record_json,
                            '$.validation_report.pagination.resume_after_fact_id'
                        )
                   FROM automation_runs
                  WHERE namespace = ?1 AND task_key = ?2
                    AND json_extract(
                            record_json,
                            '$.validation_report.pagination.resume_after_fact_id'
                        ) IS NOT NULL
                  ORDER BY completed_at_seconds DESC,
                           completed_at_micros DESC,
                           newest_sequence DESC
                  LIMIT 1",
                params![namespace, task_key],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query automation validation cursor".to_owned(),
                message: error.to_string(),
            })?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read automation validation cursor".to_owned(),
                message: error.to_string(),
            })?
        else {
            return Ok(None);
        };
        let cursor = row
            .get::<tracedecay_runtime_core::db::engine::Value>(0)
            .map_err(|error| TraceDecayError::Database {
                operation: "decode automation validation cursor".to_owned(),
                message: error.to_string(),
            })?;
        use tracedecay_runtime_core::db::engine::Value as EngineValue;
        match cursor {
            EngineValue::Null => Ok(None),
            EngineValue::Integer(value) => Ok(Some(serde_json::Value::from(value))),
            EngineValue::Real(value) => {
                Ok(serde_json::Number::from_f64(value).map(serde_json::Value::Number))
            }
            EngineValue::Text(value) => Ok(Some(serde_json::Value::String(value))),
            EngineValue::Blob(_) => Err(TraceDecayError::Database {
                operation: "decode automation validation cursor".to_owned(),
                message: "validation cursor is not a JSON scalar".to_owned(),
            }),
        }
    }

    /// Stages a terminal record so its digest can be bound into the external
    /// effect receipt before the row becomes visible in history.
    pub async fn stage_automation_run_terminal(
        &self,
        record: &AutomationRunLedgerRecord,
    ) -> Result<AutomationRunStagedPublicationV1> {
        validate_record(record)?;
        if !record.status.is_terminal() {
            return Err(TraceDecayError::Config {
                message: "exact automation run staging requires a terminal record".to_owned(),
            });
        }
        let namespace = self.automation_run_namespace()?.to_owned();
        let record = record.clone();
        let digest = record_digest(&record)?;
        let staged = AutomationRunStagedPublicationV1 {
            run_id: record.run_id.clone(),
            digest,
        };
        let staged_for_tx = staged.clone();
        self.transact_or(std::convert::identity, async move |transaction| {
            let namespace = namespace.as_str();
            let record = &record;
            let run_id = record.run_id.as_str();
            let staged_digest = staged_for_tx.digest.as_str();
            let existing = read_staging_digest(&transaction, namespace, run_id).await?;
            let record_json = canonical_record_json(record)?;
            if let Some((existing_digest, existing_json)) = existing {
                if existing_digest != staged_digest || existing_json != record_json {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "automation run '{}' has conflicting staged history",
                            record.run_id
                        ),
                    });
                }
                return Ok(staged_for_tx.clone());
            }
            transaction
                .execute(
                    "INSERT INTO automation_run_staging
                        (namespace, run_id, record_json, record_digest, staged_at_micros)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        namespace,
                        run_id,
                        record_json,
                        staged_digest,
                        tracedecay_contracts::clock::now_micros().0,
                    ],
                )
                .await
                .map_err(|error| TraceDecayError::Database {
                    operation: "stage automation run terminal".to_owned(),
                    message: error.to_string(),
                })?;
            Ok(staged_for_tx.clone())
        })
        .await?;
        Ok(staged)
    }

    /// Publishes the staged record that matches `publication`, in one
    /// transaction that also removes its staging row.
    pub async fn publish_staged_automation_run_terminal(
        &self,
        run_id: &str,
        publication: &AutomationRunStagedPublicationV1,
    ) -> Result<AutomationRunStagePublishOutcomeV1> {
        if publication.run_id != run_id {
            return Err(TraceDecayError::Config {
                message: "staged automation publication names a different run".to_owned(),
            });
        }
        let namespace = self.automation_run_namespace()?.to_owned();
        let run_id = run_id.to_owned();
        let publication_digest = publication.digest.clone();
        self.transact_or(std::convert::identity, async move |transaction| {
            let namespace = namespace.as_str();
            let run_id = run_id.as_str();
            let publication_digest = publication_digest.as_str();
            let Some((digest, record_json)) =
                read_staging_digest(&transaction, namespace, run_id).await?
            else {
                return Ok(AutomationRunStagePublishOutcomeV1::MissingPayload);
            };
            if digest != publication_digest {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "automation run '{run_id}' staged digest changed before publication"
                    ),
                });
            }
            let record: AutomationRunLedgerRecord =
                serde_json::from_str(&record_json).map_err(|error| TraceDecayError::Database {
                    operation: "decode staged automation run".to_owned(),
                    message: error.to_string(),
                })?;
            validate_record(&record)?;
            publish_in_transaction(&transaction, namespace, &record).await?;
            transaction
                .execute(
                    "DELETE FROM automation_run_staging WHERE namespace = ?1 AND run_id = ?2",
                    params![namespace, run_id],
                )
                .await
                .map_err(|error| TraceDecayError::Database {
                    operation: "clear staged automation run".to_owned(),
                    message: error.to_string(),
                })?;
            Ok(AutomationRunStagePublishOutcomeV1::Published)
        })
        .await
    }

    /// Discards one staged terminal only when it still matches `expected`.
    pub async fn discard_staged_automation_run_terminal(
        &self,
        run_id: &str,
        expected: &AutomationRunStagedPublicationV1,
    ) -> Result<()> {
        let namespace = self.automation_run_namespace()?.to_owned();
        let run_id = run_id.to_owned();
        let expected_digest = expected.digest.clone();
        self.transact_or(std::convert::identity, async move |transaction| {
            let namespace = namespace.as_str();
            let run_id = run_id.as_str();
            let expected_digest = expected_digest.as_str();
            let Some((digest, _)) = read_staging_digest(&transaction, namespace, run_id).await?
            else {
                return Ok(());
            };
            if digest != expected_digest {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "automation run '{run_id}' staged digest changed before discard"
                    ),
                });
            }
            transaction
                .execute(
                    "DELETE FROM automation_run_staging WHERE namespace = ?1 AND run_id = ?2",
                    params![namespace, run_id],
                )
                .await
                .map_err(|error| TraceDecayError::Database {
                    operation: "discard staged automation run".to_owned(),
                    message: error.to_string(),
                })?;
            Ok(())
        })
        .await
    }

    /// Appends a scheduler diagnostic run or reuses the exact durable one.
    ///
    /// The anchor is the newest effectful scheduler run the diagnostic
    /// describes; an unknown anchor or an identity conflict is refused in the
    /// same transaction that would have appended.
    pub async fn append_or_reuse_automation_scheduler_diagnostic(
        &self,
        candidate: &AutomationRunLedgerRecord,
        effectful_anchor_run_id: Option<&str>,
    ) -> Result<AutomationRunLedgerRecord> {
        validate_record(candidate)?;
        if candidate.trigger != AutomationTrigger::Scheduler
            || candidate.status != AutomationRunStatus::Skipped
            || candidate.error.is_none()
        {
            return Err(TraceDecayError::Config {
                message: "scheduler diagnostic must be a scheduler skip with a reason".to_owned(),
            });
        }
        let namespace = self.automation_run_namespace()?.to_owned();
        let candidate = candidate.clone();
        let anchor = effectful_anchor_run_id.map(str::to_owned);
        self.transact_or(std::convert::identity, async move |transaction| {
            let namespace = namespace.as_str();
            let candidate = &candidate;
            let anchor = anchor.as_deref();
            let anchor_sequence = match anchor {
                Some(anchor_run_id) => {
                    let Some(row) =
                        read_current_identity(&transaction, namespace, anchor_run_id).await?
                    else {
                        return Err(TraceDecayError::Config {
                            message: format!(
                                "automation store does not contain the scheduler diagnostic effectful anchor '{anchor_run_id}'"
                            ),
                        });
                    };
                    if row.task != task_name(candidate.task)
                        || row.task_key != candidate.task_key
                        || row.trigger != trigger_name(AutomationTrigger::Scheduler)
                        || !matches!(
                            row.status.as_str(),
                            "succeeded" | "failed"
                        )
                    {
                        return Err(TraceDecayError::Config {
                            message: "scheduler diagnostic effectful anchor identity is invalid"
                                .to_owned(),
                        });
                    }
                    Some(row.newest_sequence)
                }
                None => None,
            };
            if let Some(existing) =
                read_current_record(&transaction, namespace, candidate.run_id.as_str()).await?
            {
                if existing.task != candidate.task
                    || existing.task_key != candidate.task_key
                    || existing.trigger != candidate.trigger
                    || existing.status != candidate.status
                {
                    return Err(TraceDecayError::Config {
                        message:
                            "scheduler diagnostic identity conflicts with its persisted terminal"
                                .to_owned(),
                    });
                }
                if existing.error != candidate.error {
                    return Err(TraceDecayError::Config {
                        message:
                            "scheduler diagnostic identity conflicts with its persisted terminal"
                                .to_owned(),
                    });
                }
                if let Some(sequence) = anchor_sequence {
                    let existing_sequence =
                        read_current_sequence(&transaction, namespace, candidate.run_id.as_str())
                            .await?
                            .ok_or_else(|| TraceDecayError::Config {
                                message: "scheduler diagnostic current row disappeared".to_owned(),
                            })?;
                    if existing_sequence <= sequence {
                        return Err(TraceDecayError::Config {
                            message:
                                "scheduler diagnostic precedes its effectful anchor".to_owned(),
                        });
                    }
                }
                return Ok(existing);
            }
            publish_in_transaction(&transaction, namespace, candidate).await?;
            Ok(candidate.clone())
        })
        .await
    }
}

#[derive(Clone, Copy)]
enum SummaryFilter {
    Any,
    Successful,
    Effectful,
    SchedulerEffectful,
    Scheduler,
    SessionEvidenceBudgetExhausted,
}

impl SummaryFilter {
    fn predicate(self) -> &'static str {
        match self {
            Self::Any => "1 = 1",
            Self::Successful => "status = 'succeeded'",
            Self::Effectful => "status IN ('succeeded', 'failed')",
            Self::SchedulerEffectful => {
                "trigger = 'scheduler' AND status IN ('succeeded', 'failed')"
            }
            Self::Scheduler => "trigger = 'scheduler'",
            Self::SessionEvidenceBudgetExhausted => {
                "status = 'skipped' AND json_extract(record_json, '$.error') = ?4"
            }
        }
    }
}

async fn summary_row(
    snapshot: &tracedecay_runtime_core::db::DatabaseEngineReadSnapshot,
    namespace: &str,
    task_name: &str,
    task_key: &str,
    filter: SummaryFilter,
) -> Result<Option<AutomationRunLedgerRecord>> {
    let sql = format!(
        "SELECT record_json
           FROM automation_runs
          WHERE namespace = ?1 AND task = ?2 AND task_key = ?3 AND {}
          ORDER BY completed_at_seconds DESC,
                   completed_at_micros DESC,
                   newest_sequence DESC
          LIMIT 1",
        filter.predicate()
    );
    let query_result = match filter {
        SummaryFilter::SessionEvidenceBudgetExhausted => {
            snapshot
                .query(
                    &sql,
                    params![
                        namespace,
                        task_name,
                        task_key,
                        tracedecay_domain::automation::SESSION_EVIDENCE_BUDGET_EXHAUSTED
                    ],
                )
                .await
        }
        _ => {
            snapshot
                .query(&sql, params![namespace, task_name, task_key])
                .await
        }
    };
    let mut rows = query_result.map_err(|error| TraceDecayError::Database {
        operation: "query automation task summary".to_owned(),
        message: error.to_string(),
    })?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read automation task summary".to_owned(),
            message: error.to_string(),
        })?
    else {
        return Ok(None);
    };
    let record_json = row
        .get::<String>(0)
        .map_err(|error| TraceDecayError::Database {
            operation: "decode automation task summary".to_owned(),
            message: error.to_string(),
        })?;
    serde_json::from_str(&record_json)
        .map(Some)
        .map_err(|error| TraceDecayError::Database {
            operation: "decode automation task summary".to_owned(),
            message: error.to_string(),
        })
}

struct CurrentIdentity {
    task: String,
    task_key: String,
    trigger: String,
    status: String,
    newest_sequence: i64,
    completed_at_seconds: i64,
    completed_at_micros: i64,
}

async fn read_current_identity<Q: QueryExecutor>(
    connection: &Q,
    namespace: &str,
    run_id: &str,
) -> Result<Option<CurrentIdentity>> {
    let mut rows = connection
        .query(
            "SELECT task, task_key, trigger, status, newest_sequence,
                    completed_at_seconds, completed_at_micros
               FROM automation_runs
              WHERE namespace = ?1 AND run_id = ?2",
            params![namespace, run_id],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read automation run current state".to_owned(),
            message: error.to_string(),
        })?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read automation run current state".to_owned(),
            message: error.to_string(),
        })?
    else {
        return Ok(None);
    };
    Ok(Some(CurrentIdentity {
        task: decode_column(&row, 0)?,
        task_key: decode_column(&row, 1)?,
        trigger: decode_column(&row, 2)?,
        status: decode_column(&row, 3)?,
        newest_sequence: decode_column(&row, 4)?,
        completed_at_seconds: decode_column(&row, 5)?,
        completed_at_micros: decode_column(&row, 6)?,
    }))
}

async fn read_current_json<Q: QueryExecutor>(
    connection: &Q,
    namespace: &str,
    run_id: &str,
) -> Result<Option<String>> {
    let mut rows = connection
        .query(
            "SELECT record_json
               FROM automation_runs
              WHERE namespace = ?1 AND run_id = ?2",
            params![namespace, run_id],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read automation run current state".to_owned(),
            message: error.to_string(),
        })?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read automation run current state".to_owned(),
            message: error.to_string(),
        })?
    else {
        return Ok(None);
    };
    decode_column(&row, 0).map(Some)
}

async fn read_current_record<Q: QueryExecutor>(
    connection: &Q,
    namespace: &str,
    run_id: &str,
) -> Result<Option<AutomationRunLedgerRecord>> {
    read_current_json(connection, namespace, run_id)
        .await?
        .map(|record_json| {
            serde_json::from_str(&record_json).map_err(|error| TraceDecayError::Database {
                operation: "decode automation run current state".to_owned(),
                message: error.to_string(),
            })
        })
        .transpose()
}

async fn read_current_sequence<Q: QueryExecutor>(
    connection: &Q,
    namespace: &str,
    run_id: &str,
) -> Result<Option<i64>> {
    Ok(read_current_identity(connection, namespace, run_id)
        .await?
        .map(|row| row.newest_sequence))
}

async fn read_staging_digest<Q: QueryExecutor>(
    connection: &Q,
    namespace: &str,
    run_id: &str,
) -> Result<Option<(String, String)>> {
    let mut rows = connection
        .query(
            "SELECT record_digest, record_json
               FROM automation_run_staging
              WHERE namespace = ?1 AND run_id = ?2",
            params![namespace, run_id],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read staged automation run".to_owned(),
            message: error.to_string(),
        })?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read staged automation run".to_owned(),
            message: error.to_string(),
        })?
    else {
        return Ok(None);
    };
    Ok(Some((decode_column(&row, 0)?, decode_column(&row, 1)?)))
}

async fn read_transition_digest<Q: QueryExecutor>(
    connection: &Q,
    namespace: &str,
    run_id: &str,
    status: &str,
) -> Result<Option<String>> {
    let mut rows = connection
        .query(
            "SELECT record_digest
               FROM automation_run_transitions
              WHERE namespace = ?1 AND run_id = ?2 AND status = ?3",
            params![namespace, run_id, status],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read automation run transition".to_owned(),
            message: error.to_string(),
        })?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "read automation run transition".to_owned(),
            message: error.to_string(),
        })?
    else {
        return Ok(None);
    };
    decode_column(&row, 0).map(Some)
}

async fn publish_in_transaction(
    transaction: &crate::RegisteredGlobalDbWriteTx<'_, '_>,
    namespace: &str,
    record: &AutomationRunLedgerRecord,
) -> Result<AutomationRunPublishOutcomeV1> {
    let status = record.status.as_str();
    let (completed_at_seconds, completed_at_micros) = canonical_completion(record)?;
    let digest = record_digest(record)?;
    let record_json = canonical_record_json(record)?;
    let current = read_current_identity(transaction, namespace, &record.run_id).await?;
    match current {
        None => {
            let sequence = next_sequence(transaction, namespace).await?;
            insert_transition(
                transaction,
                namespace,
                record,
                status,
                &record_json,
                &digest,
                completed_at_seconds,
                completed_at_micros,
                sequence,
            )
            .await?;
            insert_current(
                transaction,
                namespace,
                record,
                status,
                &record_json,
                &digest,
                completed_at_seconds,
                completed_at_micros,
                sequence,
            )
            .await?;
            Ok(AutomationRunPublishOutcomeV1::Published { sequence })
        }
        Some(current) => {
            if current.task != task_name(record.task)
                || current.task_key != record.task_key
                || current.trigger != trigger_name(record.trigger)
            {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "automation run '{}' mutates immutable identity",
                        record.run_id
                    ),
                });
            }
            if let Some(existing_digest) =
                read_transition_digest(transaction, namespace, &record.run_id, status).await?
            {
                if existing_digest != digest {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "automation run '{}' repeats a conflicting lifecycle state",
                            record.run_id
                        ),
                    });
                }
                // A byte-identical replay refreshes physical ordering without
                // regressing logical lifecycle state.
                let sequence = next_sequence(transaction, namespace).await?;
                transaction
                    .execute(
                        "UPDATE automation_runs
                            SET newest_sequence = ?3
                          WHERE namespace = ?1 AND run_id = ?2",
                        params![namespace, record.run_id.as_str(), sequence],
                    )
                    .await
                    .map_err(|error| TraceDecayError::Database {
                        operation: "refresh automation run ordering".to_owned(),
                        message: error.to_string(),
                    })?;
                return Ok(AutomationRunPublishOutcomeV1::Replayed { sequence });
            }
            let previous_status = parse_status(&current.status)?;
            if !valid_transition(previous_status, record.status) {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "automation run '{}' contains an invalid lifecycle transition",
                        record.run_id
                    ),
                });
            }
            if (completed_at_seconds, completed_at_micros)
                < (current.completed_at_seconds, current.completed_at_micros)
            {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "automation run '{}' regresses completion time",
                        record.run_id
                    ),
                });
            }
            let sequence = next_sequence(transaction, namespace).await?;
            insert_transition(
                transaction,
                namespace,
                record,
                status,
                &record_json,
                &digest,
                completed_at_seconds,
                completed_at_micros,
                sequence,
            )
            .await?;
            transaction
                .execute(
                    "UPDATE automation_runs
                        SET status = ?3, record_json = ?4, record_digest = ?5,
                            completed_at_seconds = ?6, completed_at_micros = ?7,
                            newest_sequence = ?8, published_at_micros = ?9
                      WHERE namespace = ?1 AND run_id = ?2",
                    params![
                        namespace,
                        record.run_id.as_str(),
                        status,
                        record_json.as_str(),
                        digest.as_str(),
                        completed_at_seconds,
                        completed_at_micros,
                        sequence,
                        tracedecay_contracts::clock::now_micros().0,
                    ],
                )
                .await
                .map_err(|error| TraceDecayError::Database {
                    operation: "update automation run current state".to_owned(),
                    message: error.to_string(),
                })?;
            Ok(AutomationRunPublishOutcomeV1::Published { sequence })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_transition(
    transaction: &crate::RegisteredGlobalDbWriteTx<'_, '_>,
    namespace: &str,
    record: &AutomationRunLedgerRecord,
    status: &str,
    record_json: &str,
    digest: &str,
    completed_at_seconds: i64,
    completed_at_micros: i64,
    sequence: i64,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO automation_run_transitions
                (namespace, run_id, status, record_json, record_digest,
                 completed_at_seconds, completed_at_micros,
                 published_sequence, published_at_micros)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                namespace,
                record.run_id.as_str(),
                status,
                record_json,
                digest,
                completed_at_seconds,
                completed_at_micros,
                sequence,
                tracedecay_contracts::clock::now_micros().0,
            ],
        )
        .await
        .map(|_| ())
        .map_err(|error| TraceDecayError::Database {
            operation: "record automation run transition".to_owned(),
            message: error.to_string(),
        })
}

#[allow(clippy::too_many_arguments)]
async fn insert_current(
    transaction: &crate::RegisteredGlobalDbWriteTx<'_, '_>,
    namespace: &str,
    record: &AutomationRunLedgerRecord,
    status: &str,
    record_json: &str,
    digest: &str,
    completed_at_seconds: i64,
    completed_at_micros: i64,
    sequence: i64,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO automation_runs
                (namespace, run_id, task, task_key, trigger, status,
                 record_json, record_digest, completed_at_seconds,
                 completed_at_micros, newest_sequence, published_at_micros)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                namespace,
                record.run_id.as_str(),
                task_name(record.task),
                record.task_key.as_str(),
                trigger_name(record.trigger),
                status,
                record_json,
                digest,
                completed_at_seconds,
                completed_at_micros,
                sequence,
                tracedecay_contracts::clock::now_micros().0,
            ],
        )
        .await
        .map(|_| ())
        .map_err(|error| TraceDecayError::Database {
            operation: "record automation run current state".to_owned(),
            message: error.to_string(),
        })
}

async fn next_sequence(
    transaction: &crate::RegisteredGlobalDbWriteTx<'_, '_>,
    namespace: &str,
) -> Result<i64> {
    transaction
        .execute(
            "INSERT INTO automation_run_sequence (namespace, next_sequence)
             VALUES (?1, 1)
             ON CONFLICT(namespace) DO NOTHING",
            params![namespace],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "allocate automation run sequence".to_owned(),
            message: error.to_string(),
        })?;
    let mut rows = transaction
        .query(
            "SELECT next_sequence FROM automation_run_sequence WHERE namespace = ?1",
            params![namespace],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "allocate automation run sequence".to_owned(),
            message: error.to_string(),
        })?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "allocate automation run sequence".to_owned(),
            message: error.to_string(),
        })?
    else {
        return Err(TraceDecayError::Database {
            operation: "allocate automation run sequence".to_owned(),
            message: "sequence query returned no row".to_owned(),
        });
    };
    let sequence: i64 = decode_column(&row, 0)?;
    transaction
        .execute(
            "UPDATE automation_run_sequence SET next_sequence = ?2 WHERE namespace = ?1",
            params![
                namespace,
                sequence.checked_add(1).ok_or_else(|| {
                    TraceDecayError::Database {
                        operation: "allocate automation run sequence".to_owned(),
                        message: "automation run sequence overflowed".to_owned(),
                    }
                })?
            ],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "allocate automation run sequence".to_owned(),
            message: error.to_string(),
        })?;
    Ok(sequence)
}

fn decode_column<T: tracedecay_runtime_core::db::engine::FromValue>(
    row: &tracedecay_runtime_core::db::engine::Row,
    index: i32,
) -> Result<T> {
    row.get::<T>(index)
        .map_err(|error| TraceDecayError::Database {
            operation: "decode automation run row".to_owned(),
            message: error.to_string(),
        })
}

fn task_name(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator",
        AgentTaskKind::SessionReflector => "session_reflector",
        AgentTaskKind::SkillWriter => "skill_writer",
        AgentTaskKind::CombinedReview => "combined_review",
        AgentTaskKind::UserJob => "user_job",
    }
}

fn trigger_name(trigger: AutomationTrigger) -> &'static str {
    match trigger {
        AutomationTrigger::ManualCli => "manual_cli",
        AutomationTrigger::ManualMcp => "manual_mcp",
        AutomationTrigger::Dashboard => "dashboard",
        AutomationTrigger::Application => "application",
        AutomationTrigger::Scheduler => "scheduler",
        AutomationTrigger::HostReceipt => "host_receipt",
    }
}

fn parse_status(status: &str) -> Result<AutomationRunStatus> {
    match status {
        "queued" => Ok(AutomationRunStatus::Queued),
        "running" => Ok(AutomationRunStatus::Running),
        "succeeded" => Ok(AutomationRunStatus::Succeeded),
        "failed" => Ok(AutomationRunStatus::Failed),
        "skipped" => Ok(AutomationRunStatus::Skipped),
        _ => Err(TraceDecayError::Database {
            operation: "decode automation run status".to_owned(),
            message: format!("unrecognized automation run status '{status}'"),
        }),
    }
}

fn valid_transition(previous: AutomationRunStatus, next: AutomationRunStatus) -> bool {
    match previous {
        AutomationRunStatus::Queued => next == AutomationRunStatus::Running || next.is_terminal(),
        AutomationRunStatus::Running => next.is_terminal(),
        AutomationRunStatus::Succeeded
        | AutomationRunStatus::Failed
        | AutomationRunStatus::Skipped => false,
    }
}

fn validate_record(record: &AutomationRunLedgerRecord) -> Result<()> {
    validate_run_id(&record.run_id)?;
    if record.task_key.is_empty() {
        return Err(TraceDecayError::Config {
            message: "automation run task_key must not be empty".to_owned(),
        });
    }
    canonical_started_at(record)?;
    canonical_completion(record).map(|_| ())
}

/// A row below schema v2 requires a ledger reset; a newer row is unsupported
/// rather than silently accepted by this binary.
fn require_supported_schema(schema_version: u32) -> Result<()> {
    match schema_version {
        2 => Ok(()),
        0..2 => Err(TraceDecayError::reset_required(
            "automation run store",
            format!("a record has retired schema version {schema_version}"),
        )),
        _ => Err(TraceDecayError::Config {
            message: format!("automation run store schema version {schema_version} is unsupported"),
        }),
    }
}

fn canonical_started_at(record: &AutomationRunLedgerRecord) -> Result<i64> {
    require_supported_schema(record.schema_version)?;
    parse_nonnegative_unix_integer(&record.started_at, "started_at")
}

fn canonical_completion(record: &AutomationRunLedgerRecord) -> Result<(i64, i64)> {
    require_supported_schema(record.schema_version)?;
    let completed_at = parse_nonnegative_unix_integer(&record.completed_at, "completed_at")?;
    let canonical_micros =
        completed_at
            .checked_mul(1_000_000)
            .ok_or_else(|| TraceDecayError::Config {
                message: "automation completion timestamp overflows signed microseconds".to_owned(),
            })?;
    let completed_at_micros = record.completed_at_micros.unwrap_or(canonical_micros);
    if completed_at_micros < 0 {
        return Err(TraceDecayError::Config {
            message: "automation completion timestamp predates the UNIX epoch".to_owned(),
        });
    }
    if completed_at_micros.div_euclid(1_000_000) != completed_at {
        return Err(TraceDecayError::Config {
            message: "automation completion timestamp seconds and microseconds disagree".to_owned(),
        });
    }
    Ok((completed_at, completed_at_micros))
}

fn parse_nonnegative_unix_integer(value: &str, label: &str) -> Result<i64> {
    match value.parse::<i64>() {
        Ok(seconds) if seconds >= 0 => Ok(seconds),
        Ok(_) => Err(TraceDecayError::Config {
            message: format!("automation {label} predates the UNIX epoch"),
        }),
        Err(integer_error) => Err(TraceDecayError::Config {
            message: format!(
                "automation {label} '{value}' is not nonnegative Unix seconds: {integer_error}"
            ),
        }),
    }
}

fn validate_run_id(run_id: &str) -> Result<()> {
    let valid = !run_id.is_empty()
        && run_id != "."
        && run_id != ".."
        && run_id.len() <= tracedecay_domain::canonical_text::CANONICAL_TEXT_MAX_BYTES
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(TraceDecayError::Config {
            message: format!("automation run_id '{run_id}' is not a canonical store identity"),
        })
    }
}

fn record_digest(record: &AutomationRunLedgerRecord) -> Result<String> {
    tracedecay_domain::canonical_sha256(record)
        .map(|digest| digest.as_str().to_owned())
        .map_err(|error| TraceDecayError::Config {
            message: format!("automation run canonical digest failed: {error}"),
        })
}

fn canonical_record_json(record: &AutomationRunLedgerRecord) -> Result<String> {
    serde_json::to_string(record).map_err(|error| TraceDecayError::Config {
        message: format!("automation run record serialization failed: {error}"),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;
    use tracedecay_contracts::automation::{
        AgentTaskKind, AutomationRunLedgerRecord, AutomationRunStatus,
    };
    use tracedecay_domain::ProjectId;

    use crate::tests::harness::RegisteredGlobalDbTestRuntime;

    fn record(
        run_id: &str,
        task: AgentTaskKind,
        task_key: &str,
        status: AutomationRunStatus,
        completed_at: i64,
        error: Option<&str>,
    ) -> AutomationRunLedgerRecord {
        let mut value = json!({
            "schema_version": 2,
            "run_id": run_id,
            "trigger": "scheduler",
            "task": task,
            "task_key": task_key,
            "backend": "fixture",
            "status": status,
            "accepted_count": 0,
            "rejected_count": 0,
            "started_at": completed_at.to_string(),
            "completed_at": completed_at.to_string(),
        });
        if let Some(error) = error {
            value["error"] = json!(error);
        }
        serde_json::from_value(value).unwrap()
    }

    async fn project_fixture() -> (RegisteredGlobalDbTestRuntime, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let runtime = RegisteredGlobalDbTestRuntime::project(
            root.path().join("profile"),
            root.path().join("project"),
            ProjectId::new("project.automation-run-state").unwrap(),
        )
        .await
        .unwrap();
        (runtime, root)
    }

    #[tokio::test]
    async fn publication_replay_conflict_and_bounded_history() {
        let (runtime, _root) = project_fixture().await;
        let database = runtime.project_database().unwrap();
        let task = AgentTaskKind::SessionReflector;
        let queued = record(
            "run-1",
            task,
            "session_reflector",
            AutomationRunStatus::Queued,
            10,
            None,
        );
        let running = record(
            "run-1",
            task,
            "session_reflector",
            AutomationRunStatus::Running,
            11,
            None,
        );
        let terminal = record(
            "run-1",
            task,
            "session_reflector",
            AutomationRunStatus::Succeeded,
            12,
            None,
        );

        assert_eq!(
            database.publish_automation_run(&queued).await.unwrap(),
            crate::AutomationRunPublishOutcomeV1::Published { sequence: 1 }
        );
        assert_eq!(
            database.publish_automation_run(&running).await.unwrap(),
            crate::AutomationRunPublishOutcomeV1::Published { sequence: 2 }
        );
        assert_eq!(
            database.publish_automation_run(&terminal).await.unwrap(),
            crate::AutomationRunPublishOutcomeV1::Published { sequence: 3 }
        );
        assert_eq!(
            database.publish_automation_run(&terminal).await.unwrap(),
            crate::AutomationRunPublishOutcomeV1::Replayed { sequence: 4 }
        );

        let conflicting = record(
            "run-1",
            task,
            "session_reflector",
            AutomationRunStatus::Succeeded,
            13,
            Some("different bytes"),
        );
        let error = database
            .publish_automation_run(&conflicting)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("conflicting lifecycle state"),
            "unexpected error: {error}"
        );
        assert!(
            !error.to_string().contains("reset"),
            "conflict must not reset the store: {error}"
        );

        let exact = database
            .automation_run_exact("run-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exact, terminal);
        assert_eq!(
            database.automation_run_exact("missing").await.unwrap(),
            None
        );

        let other = record(
            "run-2",
            task,
            "session_reflector",
            AutomationRunStatus::Failed,
            14,
            None,
        );
        database.publish_automation_run(&other).await.unwrap();
        let page = database.automation_run_page(1).await.unwrap();
        assert_eq!(page.records.len(), 1);
        assert!(page.has_more);
        assert_eq!(page.records[0].run_id, "run-2");
    }

    #[tokio::test]
    async fn staged_publication_is_invisible_until_published() {
        let (runtime, _root) = project_fixture().await;
        let database = runtime.project_database().unwrap();
        let task = AgentTaskKind::MemoryCurator;
        let terminal = record(
            "run-stage",
            task,
            "memory_curator",
            AutomationRunStatus::Succeeded,
            20,
            None,
        );

        let staged = database
            .stage_automation_run_terminal(&terminal)
            .await
            .unwrap();
        assert_eq!(
            database.automation_run_exact("run-stage").await.unwrap(),
            None
        );

        assert_eq!(
            database
                .publish_staged_automation_run_terminal("run-stage", &staged)
                .await
                .unwrap(),
            crate::AutomationRunStagePublishOutcomeV1::Published
        );
        assert_eq!(
            database
                .automation_run_exact("run-stage")
                .await
                .unwrap()
                .unwrap(),
            terminal
        );
        assert_eq!(
            database
                .publish_staged_automation_run_terminal("run-stage", &staged)
                .await
                .unwrap(),
            crate::AutomationRunStagePublishOutcomeV1::MissingPayload
        );

        let restaged = database
            .stage_automation_run_terminal(&terminal)
            .await
            .unwrap();
        let conflicting = record(
            "run-stage",
            task,
            "memory_curator",
            AutomationRunStatus::Succeeded,
            21,
            None,
        );
        let error = database
            .stage_automation_run_terminal(&conflicting)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("conflicting staged history"),
            "{error}"
        );
        database
            .discard_staged_automation_run_terminal("run-stage", &restaged)
            .await
            .unwrap();
        assert_eq!(
            database
                .publish_staged_automation_run_terminal("run-stage", &restaged)
                .await
                .unwrap(),
            crate::AutomationRunStagePublishOutcomeV1::MissingPayload
        );
    }

    #[tokio::test]
    async fn summary_and_validation_cursor_are_indexed() {
        let (runtime, _root) = project_fixture().await;
        let database = runtime.project_database().unwrap();
        let task = AgentTaskKind::MemoryCurator;

        let mut succeeded = record(
            "run-ok",
            task,
            "memory:one",
            AutomationRunStatus::Succeeded,
            30,
            None,
        );
        succeeded.validation_report = Some(json!({
            "pagination": { "resume_after_fact_id": "fact-42" }
        }));
        database.publish_automation_run(&succeeded).await.unwrap();
        let failed = record(
            "run-fail",
            task,
            "memory:one",
            AutomationRunStatus::Failed,
            31,
            None,
        );
        database.publish_automation_run(&failed).await.unwrap();
        let exhausted = record(
            "run-skip",
            task,
            "memory:one",
            AutomationRunStatus::Skipped,
            32,
            Some(tracedecay_domain::automation::SESSION_EVIDENCE_BUDGET_EXHAUSTED),
        );
        database.publish_automation_run(&exhausted).await.unwrap();

        let summary = database
            .automation_run_task_summary(task, "memory:one")
            .await
            .unwrap();
        assert_eq!(
            summary.latest_logical_activity.as_ref().unwrap().run_id,
            "run-skip"
        );
        assert_eq!(summary.latest_successful.as_ref().unwrap().run_id, "run-ok");
        assert_eq!(
            summary
                .latest_effectful_any_trigger
                .as_ref()
                .unwrap()
                .run_id,
            "run-fail"
        );
        assert_eq!(
            summary.latest_scheduler_effectful.as_ref().unwrap().run_id,
            "run-fail"
        );
        assert_eq!(
            summary.latest_scheduler_activity.as_ref().unwrap().run_id,
            "run-skip"
        );
        assert_eq!(
            summary
                .latest_session_evidence_budget_exhausted
                .as_ref()
                .unwrap()
                .run_id,
            "run-skip"
        );

        let cursor = database
            .automation_run_validation_cursor("memory:one")
            .await
            .unwrap();
        assert_eq!(cursor, Some(json!("fact-42")));
        assert_eq!(
            database
                .automation_run_validation_cursor("memory:other")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn scheduler_diagnostic_reuses_exact_terminal_and_refuses_conflicts() {
        let (runtime, _root) = project_fixture().await;
        let database = runtime.project_database().unwrap();
        let task = AgentTaskKind::SkillWriter;

        let anchor = record(
            "run-anchor",
            task,
            "skill_writer",
            AutomationRunStatus::Succeeded,
            40,
            None,
        );
        database.publish_automation_run(&anchor).await.unwrap();
        let diagnostic = record(
            "run-diagnostic",
            task,
            "skill_writer",
            AutomationRunStatus::Skipped,
            41,
            Some("no due session"),
        );
        let first = database
            .append_or_reuse_automation_scheduler_diagnostic(&diagnostic, Some("run-anchor"))
            .await
            .unwrap();
        assert_eq!(first, diagnostic);
        let second = database
            .append_or_reuse_automation_scheduler_diagnostic(&diagnostic, Some("run-anchor"))
            .await
            .unwrap();
        assert_eq!(second, diagnostic);

        let conflicting = record(
            "run-diagnostic",
            task,
            "skill_writer",
            AutomationRunStatus::Skipped,
            42,
            Some("different reason"),
        );
        let error = database
            .append_or_reuse_automation_scheduler_diagnostic(&conflicting, Some("run-anchor"))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("conflicts with its persisted terminal"),
            "{error}"
        );

        let unknown_anchor = record(
            "run-diagnostic-2",
            task,
            "skill_writer",
            AutomationRunStatus::Skipped,
            43,
            Some("no due session"),
        );
        let error = database
            .append_or_reuse_automation_scheduler_diagnostic(&unknown_anchor, Some("run-missing"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not contain"), "{error}");
    }

    #[tokio::test]
    async fn concurrent_identical_publication_replays_exactly_once() {
        let (runtime, _root) = project_fixture().await;
        let first = runtime.project_database_arc().unwrap();
        let second = runtime.project_database_arc().unwrap();
        let terminal = record(
            "run-concurrent",
            AgentTaskKind::CombinedReview,
            "combined_review",
            AutomationRunStatus::Succeeded,
            55,
            None,
        );
        let (left, right) = tokio::join!(
            first.publish_automation_run(&terminal),
            second.publish_automation_run(&terminal)
        );
        let left = left.unwrap();
        let right = right.unwrap();
        let published = [left, right]
            .iter()
            .filter(|outcome| {
                matches!(
                    outcome,
                    crate::AutomationRunPublishOutcomeV1::Published { .. }
                )
            })
            .count();
        let replayed = [left, right]
            .iter()
            .filter(|outcome| {
                matches!(
                    outcome,
                    crate::AutomationRunPublishOutcomeV1::Replayed { .. }
                )
            })
            .count();
        assert_eq!(published, 1, "exactly one publication must commit");
        assert_eq!(
            replayed, 1,
            "the identical concurrent publication must replay"
        );
        let transitions: i64 = first
            .read_snapshot()
            .await
            .unwrap()
            .query(
                "SELECT COUNT(*) FROM automation_run_transitions WHERE run_id = ?1",
                tracedecay_runtime_core::db::engine::params!["run-concurrent"],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(transitions, 1, "one status owns exactly one transition row");
    }

    #[tokio::test]
    async fn history_and_artifact_digests_survive_reopen() {
        let (runtime, _root) = project_fixture().await;
        let database_path = runtime.project_database().unwrap().db_path().to_path_buf();
        let project_id = ProjectId::new("project.automation-run-state").unwrap();
        let mut terminal = record(
            "run-restart",
            AgentTaskKind::SkillWriter,
            "skill_writer",
            AutomationRunStatus::Succeeded,
            60,
            None,
        );
        terminal.artifacts = serde_json::from_value(json!([{
            "schema_version": 1,
            "kind": "traces",
            "path": "automation_artifacts/run-restart/traces.json",
            "sha256": "sha256:deadbeef",
            "summary": "restart fixture",
            "created_at": "60"
        }]))
        .unwrap();
        runtime
            .project_database()
            .unwrap()
            .publish_automation_run(&terminal)
            .await
            .unwrap();
        drop(runtime);

        let (reopened, _owner) = crate::tests::harness::open_registered_test_database_fixture(
            &database_path,
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProjectSessions { project_id },
        )
        .await
        .unwrap();
        let exact = reopened
            .automation_run_exact("run-restart")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exact, terminal);
        assert_eq!(exact.artifacts[0].sha256, "sha256:deadbeef");
        let page = reopened.automation_run_page(10).await.unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0], terminal);
    }

    #[tokio::test]
    async fn store_without_project_identity_refuses_automation_state() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RegisteredGlobalDbTestRuntime::profile(root.path().join("profile"))
            .await
            .unwrap();
        let database = runtime.profile_database();
        let terminal = record(
            "run-profile",
            AgentTaskKind::MemoryCurator,
            "memory_curator",
            AutomationRunStatus::Succeeded,
            50,
            None,
        );
        let error = database
            .publish_automation_run(&terminal)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no project identity"), "{error}");
    }
}

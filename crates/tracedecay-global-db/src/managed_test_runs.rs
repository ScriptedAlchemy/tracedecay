//! Durable record of daemon-managed test runs in the project sessions store.
//!
//! A run is written twice at the moments they happen: once when the managed
//! run is admitted (its start time, requesting session, and source identity)
//! and once when it terminates (its receipt, exit status, and outcome counts).
//! The row lives beside the sessions it is attributed to and shares that
//! store's retention. A run whose request named no session keeps a NULL
//! `session_id`; attribution is never inferred.

use tracedecay_contracts::feedback::TestResultProjectionV1;
use tracedecay_contracts::{OperationReceipt, OperationTermination};
use tracedecay_domain::{CodeGenerationId, CommitId, UtcMicros};
use tracedecay_runtime_core::db::engine::{Row, params};

use super::RegisteredGlobalDb;

pub(crate) const MANAGED_TEST_RUN_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS managed_test_runs (
        operation_id TEXT PRIMARY KEY,
        root_uri TEXT NOT NULL,
        session_id TEXT,
        head_commit_id TEXT,
        code_generation_id TEXT,
        started_at_micros INTEGER NOT NULL CHECK(started_at_micros > 0),
        requested_tests INTEGER NOT NULL CHECK(requested_tests >= 0),
        finished_at_micros INTEGER,
        termination TEXT,
        exit_code INTEGER,
        passed INTEGER,
        failed INTEGER,
        ignored INTEGER,
        results_json TEXT,
        receipt_json TEXT,
        CHECK (
            (finished_at_micros IS NULL AND termination IS NULL AND exit_code IS NULL
                AND passed IS NULL AND failed IS NULL AND ignored IS NULL
                AND results_json IS NULL AND receipt_json IS NULL)
            OR (finished_at_micros >= started_at_micros AND termination IS NOT NULL
                AND passed >= 0 AND failed >= 0 AND ignored >= 0
                AND json_valid(results_json) AND json_valid(receipt_json))
        )
    ) STRICT;
    CREATE INDEX IF NOT EXISTS idx_managed_test_runs_session
        ON managed_test_runs(session_id, started_at_micros);
    CREATE INDEX IF NOT EXISTS idx_managed_test_runs_root
        ON managed_test_runs(root_uri, started_at_micros);
";

/// What is known when a managed run is admitted, before any test executes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedTestRunStartV1 {
    pub operation_id: String,
    pub root_uri: String,
    /// The requesting session exactly as the request's attribution carried it.
    pub session_id: Option<String>,
    pub head_commit_id: Option<CommitId>,
    pub code_generation_id: Option<CodeGenerationId>,
    pub started_at: UtcMicros,
    pub requested_tests: u64,
}

/// The terminal outcome of a managed run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedTestRunOutcomeV1 {
    pub receipt: OperationReceipt,
    pub exit_code: Option<i32>,
    /// Observed pass/fail results in the order libtest reported them.
    pub results: Vec<TestResultProjectionV1>,
    pub ignored: u64,
}

impl ManagedTestRunOutcomeV1 {
    pub fn passed(&self) -> u64 {
        self.results.iter().filter(|result| result.passed).count() as u64
    }

    pub fn failed(&self) -> u64 {
        self.results.iter().filter(|result| !result.passed).count() as u64
    }
}

/// One retained managed run. `outcome` is absent until the run terminates,
/// and stays absent for a run the daemon never finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedTestRunRecordV1 {
    pub start: ManagedTestRunStartV1,
    pub outcome: Option<ManagedTestRunOutcomeV1>,
}

fn normalized_root(root_uri: &str) -> &str {
    root_uri.trim_end_matches('/')
}

impl RegisteredGlobalDb {
    /// Records an admitted managed run. The operation identity is unique; a
    /// second start for the same operation is refused.
    pub async fn record_managed_test_run_start(
        &self,
        start: &ManagedTestRunStartV1,
    ) -> Result<(), String> {
        let requested_tests = i64::try_from(start.requested_tests)
            .map_err(|_| "managed test-run requested count exceeds i64".to_owned())?;
        let writer = self
            .runtime_database()
            .writer_connection("record managed test-run start")
            .await
            .map_err(|error| format!("failed to acquire managed test-run writer: {error}"))?;
        let changed = writer
            .execute(
                "INSERT INTO managed_test_runs
                     (operation_id, root_uri, session_id, head_commit_id, code_generation_id,
                      started_at_micros, requested_tests)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    start.operation_id.as_str(),
                    normalized_root(&start.root_uri),
                    start.session_id.as_deref(),
                    start.head_commit_id.as_ref().map(CommitId::as_str),
                    start
                        .code_generation_id
                        .as_ref()
                        .map(CodeGenerationId::as_str),
                    start.started_at.0,
                    requested_tests,
                ],
            )
            .await
            .map_err(|error| format!("failed to record managed test-run start: {error}"))?;
        if changed != 1 {
            return Err(format!(
                "managed test-run start changed {changed} rows instead of one"
            ));
        }
        Ok(())
    }

    /// Records the terminal outcome of a started run exactly once.
    pub async fn record_managed_test_run_outcome(
        &self,
        operation_id: &str,
        outcome: &ManagedTestRunOutcomeV1,
    ) -> Result<(), String> {
        let termination = serde_json::to_value(outcome.receipt.termination)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| "managed test-run termination has no canonical token".to_owned())?;
        let results_json = serde_json::to_string(&outcome.results)
            .map_err(|error| format!("failed to encode managed test-run results: {error}"))?;
        let receipt_json = serde_json::to_string(&outcome.receipt)
            .map_err(|error| format!("failed to encode managed test-run receipt: {error}"))?;
        let counts = [outcome.passed(), outcome.failed(), outcome.ignored]
            .map(|count| i64::try_from(count).map_err(|_| "managed test-run count exceeds i64"));
        let [passed, failed, ignored] = counts;
        let writer = self
            .runtime_database()
            .writer_connection("record managed test-run outcome")
            .await
            .map_err(|error| format!("failed to acquire managed test-run writer: {error}"))?;
        let changed = writer
            .execute(
                "UPDATE managed_test_runs
                 SET finished_at_micros = ?2, termination = ?3, exit_code = ?4,
                     passed = ?5, failed = ?6, ignored = ?7,
                     results_json = ?8, receipt_json = ?9
                 WHERE operation_id = ?1 AND finished_at_micros IS NULL",
                params![
                    operation_id,
                    outcome.receipt.ended_at.0,
                    termination,
                    outcome.exit_code.map(i64::from),
                    passed?,
                    failed?,
                    ignored?,
                    results_json,
                    receipt_json,
                ],
            )
            .await
            .map_err(|error| format!("failed to record managed test-run outcome: {error}"))?;
        if changed != 1 {
            return Err(format!(
                "managed test-run {operation_id} has no unfinished record to settle"
            ));
        }
        Ok(())
    }

    /// The newest managed run recorded for one admitted project root, or
    /// `None` when that root has never recorded one.
    pub async fn latest_managed_test_run(
        &self,
        root_uri: &str,
    ) -> Result<Option<ManagedTestRunRecordV1>, String> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to open managed test-run snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT operation_id, root_uri, session_id, head_commit_id, code_generation_id,
                        started_at_micros, requested_tests, exit_code, ignored,
                        results_json, receipt_json
                 FROM managed_test_runs
                 WHERE root_uri = ?1
                 ORDER BY started_at_micros DESC, operation_id DESC
                 LIMIT 1",
                params![normalized_root(root_uri)],
            )
            .await
            .map_err(|error| format!("failed to query managed test runs: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read managed test run: {error}"))?
        else {
            return Ok(None);
        };
        managed_test_run_record(&row).map(Some)
    }
}

fn managed_test_run_record(row: &Row) -> Result<ManagedTestRunRecordV1, String> {
    let decode = |field: &str, error: &dyn std::fmt::Display| {
        format!("managed test-run {field} did not decode: {error}")
    };
    let text = |index: i32, field: &str| {
        row.get::<Option<String>>(index)
            .map_err(|error| decode(field, &error))
    };
    let start = ManagedTestRunStartV1 {
        operation_id: row
            .get::<String>(0)
            .map_err(|error| decode("operation_id", &error))?,
        root_uri: row
            .get::<String>(1)
            .map_err(|error| decode("root_uri", &error))?,
        session_id: text(2, "session_id")?,
        head_commit_id: text(3, "head_commit_id")?
            .map(CommitId::new)
            .transpose()
            .map_err(|error| decode("head_commit_id", &error))?,
        code_generation_id: text(4, "code_generation_id")?
            .map(CodeGenerationId::new)
            .transpose()
            .map_err(|error| decode("code_generation_id", &error))?,
        started_at: UtcMicros(
            row.get::<i64>(5)
                .map_err(|error| decode("started_at_micros", &error))?,
        ),
        requested_tests: u64::try_from(
            row.get::<i64>(6)
                .map_err(|error| decode("requested_tests", &error))?,
        )
        .map_err(|error| decode("requested_tests", &error))?,
    };
    let outcome = match (text(9, "results_json")?, text(10, "receipt_json")?) {
        (Some(results), Some(receipt)) => Some(ManagedTestRunOutcomeV1 {
            receipt: serde_json::from_str(&receipt).map_err(|error| decode("receipt", &error))?,
            exit_code: row
                .get::<Option<i64>>(7)
                .map_err(|error| decode("exit_code", &error))?
                .map(i32::try_from)
                .transpose()
                .map_err(|error| decode("exit_code", &error))?,
            results: serde_json::from_str(&results).map_err(|error| decode("results", &error))?,
            ignored: row
                .get::<Option<i64>>(8)
                .map_err(|error| decode("ignored", &error))?
                .map(u64::try_from)
                .transpose()
                .map_err(|error| decode("ignored", &error))?
                .ok_or_else(|| "a finished managed test run has no ignored count".to_owned())?,
        }),
        (None, None) => None,
        _ => return Err("managed test-run outcome is half recorded".to_owned()),
    };
    Ok(ManagedTestRunRecordV1 { start, outcome })
}

impl ManagedTestRunRecordV1 {
    pub fn termination(&self) -> Option<OperationTermination> {
        self.outcome
            .as_ref()
            .map(|outcome| outcome.receipt.termination)
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_contracts::{Deadline, OperationBudgetUsage};

    use super::*;
    use crate::tests::harness::RegisteredGlobalDbHarness;

    fn start(operation_id: &str, started_at: i64) -> ManagedTestRunStartV1 {
        ManagedTestRunStartV1 {
            operation_id: operation_id.to_owned(),
            root_uri: "file:///work/project".to_owned(),
            session_id: Some("sess-runner".to_owned()),
            head_commit_id: None,
            code_generation_id: None,
            started_at: UtcMicros(started_at),
            requested_tests: 2,
        }
    }

    #[tokio::test]
    async fn a_started_run_settles_its_outcome_once_and_reads_back_newest_first() {
        let harness = RegisteredGlobalDbHarness::open("managed-test-runs-settle").await;
        let db = &harness.registered;
        db.record_managed_test_run_start(&start("run-old", 1_000))
            .await
            .expect("record old start");
        db.record_managed_test_run_start(&start("run-new", 2_000))
            .await
            .expect("record new start");
        assert_eq!(
            db.record_managed_test_run_start(&start("run-new", 3_000))
                .await
                .map_err(|error| error.contains("UNIQUE")),
            Err(true),
            "an operation starts once"
        );

        let unfinished = db
            .latest_managed_test_run("file:///work/project")
            .await
            .expect("read unfinished")
            .expect("recorded run");
        assert_eq!(unfinished.start, start("run-new", 2_000));
        assert_eq!(unfinished.outcome, None);

        let outcome = ManagedTestRunOutcomeV1 {
            receipt: OperationReceipt::completed(
                UtcMicros(2_000),
                UtcMicros(2_500),
                Deadline::new(UtcMicros(9_000)).expect("deadline"),
                OperationBudgetUsage::default(),
            )
            .expect("receipt"),
            exit_code: Some(101),
            results: vec![
                TestResultProjectionV1 {
                    test: "tests::passes".to_owned(),
                    passed: true,
                },
                TestResultProjectionV1 {
                    test: "tests::fails".to_owned(),
                    passed: false,
                },
            ],
            ignored: 0,
        };
        db.record_managed_test_run_outcome("run-new", &outcome)
            .await
            .expect("settle outcome");
        assert_eq!(
            db.record_managed_test_run_outcome("run-new", &outcome)
                .await,
            Err("managed test-run run-new has no unfinished record to settle".to_owned()),
        );
        let settled = db
            .latest_managed_test_run("file:///work/project/")
            .await
            .expect("read settled")
            .expect("recorded run");
        assert_eq!(settled.outcome, Some(outcome));
        assert_eq!(
            db.latest_managed_test_run("file:///work/other/")
                .await
                .expect("read other root"),
            None
        );
    }
}

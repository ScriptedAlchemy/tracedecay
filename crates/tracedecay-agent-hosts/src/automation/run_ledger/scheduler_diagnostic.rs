use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::{AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, run_ledger_path};
use crate::automation::backend::task_key as canonical_task_key;
use crate::automation::config_error;
use crate::errors::{Result, TraceDecayError};

pub(crate) async fn append_or_reuse_scheduler_diagnostic(
    dashboard_root: &Path,
    candidate: &AutomationRunLedgerRecord,
    effectful_anchor_run_id: Option<&str>,
) -> Result<AutomationRunLedgerRecord> {
    validate_candidate(candidate)?;
    let path = run_ledger_path(dashboard_root);
    let candidate = candidate.clone();
    let anchor = effectful_anchor_run_id.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        append_or_reuse_blocking(&path, &candidate, anchor.as_deref())
    })
    .await
    .map_err(|error| {
        config_error(format!(
            "failed to join scheduler diagnostic write: {error}"
        ))
    })?
}

fn validate_candidate(candidate: &AutomationRunLedgerRecord) -> Result<()> {
    super::validate_run_ledger_record_semantics(candidate)?;
    if candidate.trigger != AutomationTrigger::Scheduler
        || candidate.status != AutomationRunStatus::Skipped
        || candidate.task_key.as_deref().is_none()
        || candidate.error.as_deref().is_none()
    {
        return Err(config_error(
            "scheduler diagnostic must be a keyed scheduler skip with a reason",
        ));
    }
    Ok(())
}

fn append_or_reuse_blocking(
    path: &Path,
    candidate: &AutomationRunLedgerRecord,
    effectful_anchor_run_id: Option<&str>,
) -> Result<AutomationRunLedgerRecord> {
    if let Some(parent) = path.parent() {
        crate::storage::PrivateStoreIo::create_dir_all_durable(parent)
            .map_err(TraceDecayError::from)?;
    }
    let lock =
        super::exact_publication::acquire_run_ledger_lock(path).map_err(TraceDecayError::from)?;
    let result = (|| {
        let dashboard_root = path
            .parent()
            .ok_or_else(|| config_error("automation run ledger has no parent directory"))?;
        super::exact_publication::ensure_no_exact_append_intent(dashboard_root)
            .map_err(TraceDecayError::from)?;
        let mut file = super::exact_lookup::open_stabilized_run_ledger(path, true)?
            .ok_or_else(|| config_error("automation run ledger disappeared during durable open"))?;
        super::ensure_run_ledger_eof_guard(&mut file).map_err(TraceDecayError::from)?;
        if let Some(existing) =
            find_before_anchor(&mut file, path, candidate, effectful_anchor_run_id)?
        {
            return Ok(existing);
        }
        let line = serde_json::to_string(candidate).map_err(TraceDecayError::from)?;
        file.seek(SeekFrom::End(0)).map_err(TraceDecayError::from)?;
        file.write_all(line.as_bytes())
            .map_err(TraceDecayError::from)?;
        file.write_all(b"\n").map_err(TraceDecayError::from)?;
        super::sync_run_ledger_file_and_parent(path, &file)?;
        Ok(candidate.clone())
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(TraceDecayError::from);
    result.and_then(|record| unlock.map(|()| record))
}

fn find_before_anchor(
    file: &std::fs::File,
    path: &Path,
    candidate: &AutomationRunLedgerRecord,
    effectful_anchor_run_id: Option<&str>,
) -> Result<Option<AutomationRunLedgerRecord>> {
    let mut existing = None;
    let mut reached_anchor = effectful_anchor_run_id.is_none();
    // Diagnostic idempotence and the supplied effectful anchor are durable
    // full-history authorities. One reverse fixed-buffer pass under the
    // canonical ledger lock trades O(ledger) time for O(1) memory and exact
    // reuse even after either row falls outside recent operator pages.
    let mut rows = super::exact_lookup::ReverseJsonlScanner::new(file, path)?;
    while let Some(line) = rows.next_span()? {
        let Some(projection) = super::exact_lookup::scan_jsonl_row(file, path, line)? else {
            continue;
        };
        let is_effectful_anchor = effectful_anchor_run_id == Some(projection.run_id.as_str());
        let is_candidate = projection.run_id == candidate.run_id;
        if !is_effectful_anchor && !is_candidate {
            continue;
        }
        if is_effectful_anchor {
            let anchor_task_key = projection
                .task_key
                .as_deref()
                .unwrap_or_else(|| canonical_task_key(projection.task));
            let candidate_task_key = candidate
                .task_key
                .as_deref()
                .unwrap_or_else(|| canonical_task_key(candidate.task));
            if projection.task != candidate.task
                || anchor_task_key != candidate_task_key
                || projection.trigger != AutomationTrigger::Scheduler
                || !matches!(
                    projection.status,
                    AutomationRunStatus::Succeeded | AutomationRunStatus::Failed
                )
            {
                return Err(config_error(
                    "scheduler diagnostic effectful anchor identity is invalid",
                ));
            }
        }
        if is_candidate {
            if existing.is_some() {
                return Err(config_error(format!(
                    "automation run ledger '{}' contains duplicate scheduler diagnostic '{}'",
                    path.display(),
                    candidate.run_id
                )));
            }
            let projected_task_key = projection
                .task_key
                .as_deref()
                .unwrap_or_else(|| canonical_task_key(projection.task));
            let candidate_task_key = candidate
                .task_key
                .as_deref()
                .unwrap_or_else(|| canonical_task_key(candidate.task));
            if projection.task != candidate.task
                || projected_task_key != candidate_task_key
                || projection.trigger != candidate.trigger
                || projection.status != candidate.status
            {
                return Err(config_error(
                    "scheduler diagnostic identity conflicts with its persisted terminal",
                ));
            }
            let record = super::exact_lookup::decode_jsonl_row(file, path, &projection.span)?;
            if record.run_id != projection.run_id
                || record.status != projection.status
                || record.trigger != projection.trigger
                || record.task != projection.task
                || record.task_key != projection.task_key
            {
                return Err(config_error(
                    "scheduler diagnostic row projection changed during decode",
                ));
            }
            if record.error != candidate.error {
                return Err(config_error(
                    "scheduler diagnostic identity conflicts with its persisted terminal",
                ));
            }
            existing = Some(record);
        }
        if is_effectful_anchor {
            reached_anchor = true;
        }
    }
    if !reached_anchor {
        return Err(config_error(format!(
            "automation run ledger '{}' does not contain the scheduler diagnostic effectful anchor",
            path.display()
        )));
    }
    Ok(existing)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn record(
        run_id: &str,
        status: &str,
        trigger: &str,
        error: Option<&str>,
    ) -> AutomationRunLedgerRecord {
        let mut value = serde_json::json!({
            "schema_version": 2,
            "run_id": run_id,
            "trigger": trigger,
            "task": "user_job",
            "task_key": "user_job:nightly",
            "backend": "codex_app_server",
            "status": status,
            "accepted_count": 0,
            "rejected_count": 0,
            "started_at": "1",
            "completed_at": "1",
            "completed_at_micros": 1_000_000
        });
        if let Some(error) = error {
            value["error"] = serde_json::json!(error);
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn malformed_newer_row_blocks_diagnostic_reuse() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let anchor = record("effect-anchor", "succeeded", "scheduler", None);
        let diagnostic = record(
            "user_job_skip_stable",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&anchor).unwrap()).unwrap();
        writeln!(file, "{}", serde_json::to_string(&diagnostic).unwrap()).unwrap();
        writeln!(file, "not-json-but-unrelated").unwrap();
        drop(file);

        assert!(append_or_reuse_blocking(&path, &diagnostic, Some("effect-anchor")).is_err());
        let occurrences = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| line.contains("user_job_skip_stable"))
            .count();
        assert_eq!(occurrences, 1);
    }

    #[test]
    fn schema_invalid_unrelated_row_blocks_diagnostic_append() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let candidate = record(
            "user_job_skip_new",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        let malformed = serde_json::to_string(&record("unrelated", "succeeded", "dashboard", None))
            .unwrap()
            .replace("\"accepted_count\":0", "\"accepted_count\":false");
        std::fs::write(&path, format!("{malformed}\n")).unwrap();

        assert!(append_or_reuse_blocking(&path, &candidate, None).is_err());
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains(&candidate.run_id)
        );
    }

    #[test]
    fn semantically_invalid_unrelated_row_blocks_diagnostic_append() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let candidate = record(
            "user_job_skip_semantic",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        let invalid = serde_json::to_string(&record("unrelated", "succeeded", "dashboard", None))
            .unwrap()
            .replace("\"schema_version\":2", "\"schema_version\":1");
        std::fs::write(&path, format!("{invalid}\n")).unwrap();

        assert!(append_or_reuse_blocking(&path, &candidate, None).is_err());
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains(&candidate.run_id)
        );
    }

    #[test]
    fn semantically_invalid_candidate_is_rejected_before_diagnostic_write() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let mut candidate = record(
            "user_job_skip_invalid_candidate",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        candidate.schema_version = 99;

        let error = validate_candidate(&candidate).expect_err("invalid candidate must fail");

        assert!(error.to_string().contains("schema version 99"));
        assert!(!path.exists());
    }

    #[test]
    fn spoofed_effectful_anchor_blocks_diagnostic_reuse() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let candidate = record(
            "user_job_skip_anchor",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        let spoofed_anchor = record("effect-anchor", "succeeded", "dashboard", None);
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&spoofed_anchor).unwrap()),
        )
        .unwrap();

        assert!(append_or_reuse_blocking(&path, &candidate, Some("effect-anchor")).is_err());
    }

    #[test]
    fn malformed_row_before_anchor_blocks_diagnostic_reuse() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let anchor = record("effect-anchor", "succeeded", "scheduler", None);
        let candidate = record(
            "user_job_skip_before_malformed",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        std::fs::write(
            &path,
            format!(
                "not-json\n{}\n{}\n",
                serde_json::to_string(&anchor).unwrap(),
                serde_json::to_string(&candidate).unwrap()
            ),
        )
        .unwrap();

        assert!(append_or_reuse_blocking(&path, &candidate, Some(&anchor.run_id)).is_err());
        assert_eq!(
            std::fs::read_to_string(path)
                .unwrap()
                .matches(candidate.run_id.as_str())
                .count(),
            1
        );
    }

    #[test]
    fn diagnostic_before_anchor_is_reused_without_append() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let anchor = record("effect-anchor", "succeeded", "scheduler", None);
        let candidate = record(
            "user_job_skip_before_anchor",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&candidate).unwrap(),
                serde_json::to_string(&anchor).unwrap()
            ),
        )
        .unwrap();

        assert_eq!(
            append_or_reuse_blocking(&path, &candidate, Some(&anchor.run_id)).unwrap(),
            candidate
        );
        assert_eq!(
            std::fs::read_to_string(path)
                .unwrap()
                .matches("user_job_skip_before_anchor")
                .count(),
            1
        );
    }

    #[test]
    fn duplicate_diagnostic_across_anchor_is_rejected() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let anchor = record("effect-anchor", "succeeded", "scheduler", None);
        let candidate = record(
            "user_job_skip_duplicate_anchor",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                serde_json::to_string(&candidate).unwrap(),
                serde_json::to_string(&anchor).unwrap(),
                serde_json::to_string(&candidate).unwrap()
            ),
        )
        .unwrap();

        assert!(append_or_reuse_blocking(&path, &candidate, Some(&anchor.run_id)).is_err());
    }

    #[test]
    fn conflicting_diagnostic_before_anchor_is_rejected() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let anchor = record("effect-anchor", "succeeded", "scheduler", None);
        let candidate = record(
            "user_job_skip_conflict_anchor",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        let conflict = record(
            &candidate.run_id,
            "skipped",
            "scheduler",
            Some("concurrency_busy"),
        );
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&conflict).unwrap(),
                serde_json::to_string(&anchor).unwrap()
            ),
        )
        .unwrap();

        assert!(append_or_reuse_blocking(&path, &candidate, Some(&anchor.run_id)).is_err());
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("interval_not_due")
        );
    }

    #[test]
    fn concurrent_repeats_append_one_terminal() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let candidate = record(
            "user_job_skip_atomic",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        let left_path = path.clone();
        let left = candidate.clone();
        let right_path = path.clone();
        let right = candidate.clone();
        let left = std::thread::spawn(move || append_or_reuse_blocking(&left_path, &left, None));
        let right = std::thread::spawn(move || append_or_reuse_blocking(&right_path, &right, None));
        assert_eq!(left.join().unwrap().unwrap(), candidate);
        assert_eq!(right.join().unwrap().unwrap(), candidate);
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 1);
    }

    #[test]
    fn scheduler_diagnostic_creates_missing_ledger_and_commits_newline() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let candidate = record(
            "user_job_skip_create",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );

        assert!(!path.exists());
        assert_eq!(
            append_or_reuse_blocking(&path, &candidate, None).unwrap(),
            candidate
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    }

    #[test]
    fn multi_megabyte_anchor_allows_exact_reuse_and_append() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let candidate = record(
            "user_job_skip_large_anchor",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        let anchor = record("effect-anchor", "succeeded", "scheduler", None);
        let large_anchor = serde_json::to_string(&anchor).unwrap().replace(
            "\"completed_at\":\"1\"",
            &format!(
                "\"validation_report\":{{\"payload\":\"{}\"}},\"completed_at\":\"1\"",
                "x".repeat(8 * 1024 * 1024)
            ),
        );
        std::fs::write(
            &path,
            format!(
                "{}\n{large_anchor}\n",
                serde_json::to_string(&candidate).unwrap(),
            ),
        )
        .unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 8 * 1024 * 1024);

        assert_eq!(
            append_or_reuse_blocking(&path, &candidate, Some(&anchor.run_id)).unwrap(),
            candidate
        );

        let appended = record(
            "user_job_skip_after_large_anchor",
            "skipped",
            "scheduler",
            Some("concurrency_busy"),
        );
        assert_eq!(
            append_or_reuse_blocking(&path, &appended, Some(&anchor.run_id)).unwrap(),
            appended
        );

        let ledger = std::fs::read_to_string(path).unwrap();
        assert_eq!(ledger.matches("user_job_skip_large_anchor").count(), 1);
        assert_eq!(
            ledger.matches("user_job_skip_after_large_anchor").count(),
            1
        );
    }

    #[test]
    fn no_anchor_scans_large_history_for_exact_reuse() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = run_ledger_path(temp.path());
        let candidate = record(
            "user_job_skip_no_anchor",
            "skipped",
            "scheduler",
            Some("interval_not_due"),
        );
        let filler =
            serde_json::to_string(&record("foreign-no-anchor", "succeeded", "dashboard", None))
                .unwrap()
                .replace(
                    "\"completed_at\":\"1\"",
                    &format!(
                        "\"validation_report\":{{\"payload\":\"{}\"}},\"completed_at\":\"1\"",
                        "x".repeat(8 * 1024 * 1024)
                    ),
                );
        std::fs::write(
            &path,
            format!("{}\n{filler}\n", serde_json::to_string(&candidate).unwrap()),
        )
        .unwrap();

        assert_eq!(
            append_or_reuse_blocking(&path, &candidate, None).unwrap(),
            candidate
        );
        assert_eq!(
            std::fs::read_to_string(path)
                .unwrap()
                .matches("user_job_skip_no_anchor")
                .count(),
            1
        );
    }
}

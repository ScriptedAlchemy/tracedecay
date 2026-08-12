use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::{AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, run_ledger_path};
use crate::automation::config_error;
use crate::errors::{Result, TraceDecayError};

const DIAGNOSTIC_SCAN_BYTES: u64 = 8 * 1024 * 1024;

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
        std::fs::create_dir_all(parent).map_err(TraceDecayError::from)?;
    }
    let lock_path = crate::storage::append_lock_path(path);
    let lock =
        crate::storage::acquire_sidecar_lock_blocking(&lock_path).map_err(TraceDecayError::from)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)
            .map_err(TraceDecayError::from)?;
        if let Some(existing) =
            find_before_anchor(&mut file, path, candidate, effectful_anchor_run_id)?
        {
            return Ok(existing);
        }
        let line = serde_json::to_string(candidate).map_err(TraceDecayError::from)?;
        file.write_all(format!("{line}\n").as_bytes())
            .map_err(TraceDecayError::from)?;
        file.sync_all().map_err(TraceDecayError::from)?;
        Ok(candidate.clone())
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(TraceDecayError::from);
    result.and_then(|record| unlock.map(|()| record))
}

fn find_before_anchor(
    file: &mut std::fs::File,
    path: &Path,
    candidate: &AutomationRunLedgerRecord,
    effectful_anchor_run_id: Option<&str>,
) -> Result<Option<AutomationRunLedgerRecord>> {
    let file_len = file.metadata().map_err(TraceDecayError::from)?.len();
    let window = file_len.min(DIAGNOSTIC_SCAN_BYTES);
    let start = file_len.saturating_sub(window);
    file.seek(SeekFrom::Start(start))
        .map_err(TraceDecayError::from)?;
    let mut bytes = vec![
        0;
        usize::try_from(window).map_err(|_| config_error(
            "scheduler diagnostic scan window is not representable"
        ))?
    ];
    file.read_exact(&mut bytes).map_err(TraceDecayError::from)?;
    let complete = if start == 0 {
        bytes.as_slice()
    } else {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |newline| &bytes[newline + 1..])
    };
    let mut existing = None;
    let mut reached_anchor = effectful_anchor_run_id.is_none() && start == 0;
    for line in complete.split(|byte| *byte == b'\n').rev() {
        let Ok(record) = serde_json::from_slice::<AutomationRunLedgerRecord>(line) else {
            continue;
        };
        let is_effectful_anchor = effectful_anchor_run_id == Some(record.run_id.as_str());
        if record.run_id == candidate.run_id {
            if existing.is_some() {
                return Err(config_error(format!(
                    "automation run ledger '{}' contains duplicate scheduler diagnostic '{}'",
                    path.display(),
                    candidate.run_id
                )));
            }
            if record.task_key != candidate.task_key
                || record.trigger != candidate.trigger
                || record.status != candidate.status
                || record.error != candidate.error
            {
                return Err(config_error(
                    "scheduler diagnostic identity conflicts with its persisted terminal",
                ));
            }
            existing = Some(record);
        }
        if is_effectful_anchor {
            reached_anchor = true;
            break;
        }
    }
    if !reached_anchor {
        return Err(config_error(format!(
            "automation run ledger '{}' exceeded the bounded scheduler diagnostic proof window",
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
    fn unrelated_noise_and_malformed_rows_do_not_evict_a_diagnostic() {
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
        for index in 0..4_000 {
            let unrelated = record(
                &format!("dashboard-{index}"),
                "succeeded",
                "dashboard",
                None,
            );
            writeln!(file, "{}", serde_json::to_string(&unrelated).unwrap()).unwrap();
        }
        drop(file);

        assert_eq!(
            append_or_reuse_blocking(&path, &diagnostic, Some("effect-anchor")).unwrap(),
            diagnostic
        );
        let occurrences = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| line.contains("user_job_skip_stable"))
            .count();
        assert_eq!(occurrences, 1);
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
}

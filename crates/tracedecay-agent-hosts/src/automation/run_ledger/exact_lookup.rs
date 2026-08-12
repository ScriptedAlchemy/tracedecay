use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{AutomationRunLedgerRecord, run_ledger_path, validate_run_id_component};
use crate::automation::config_error;
use crate::errors::Result;

const EXACT_RUN_LEDGER_MAX_BYTES: u64 = 8 * 1024 * 1024;

pub async fn find_run_record_exact_bounded(
    dashboard_root: &Path,
    run_id: &str,
) -> Result<Option<AutomationRunLedgerRecord>> {
    validate_run_id_component(run_id)?;
    let path = run_ledger_path(dashboard_root);
    let requested_run_id = run_id.to_owned();
    tokio::task::spawn_blocking(move || {
        read_exact_run_record_bounded(&path, &requested_run_id, EXACT_RUN_LEDGER_MAX_BYTES)
    })
    .await
    .map_err(|error| {
        config_error(format!(
            "failed to join exact automation run ledger read: {error}"
        ))
    })?
}

fn read_exact_run_record_bounded(
    path: &Path,
    run_id: &str,
    max_bytes: u64,
) -> Result<Option<AutomationRunLedgerRecord>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "failed to read automation run ledger '{}': {error}",
                path.display()
            )));
        }
    };
    let file_len = file
        .metadata()
        .map_err(|error| {
            config_error(format!(
                "failed to inspect automation run ledger '{}': {error}",
                path.display()
            ))
        })?
        .len();
    if file_len > max_bytes {
        return Err(config_error(format!(
            "automation run ledger '{}' is {file_len} bytes, exceeding the exact-run lookup bound of {max_bytes} bytes",
            path.display()
        )));
    }
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        config_error(format!(
            "failed to seek automation run ledger '{}': {error}",
            path.display()
        ))
    })?;
    let mut bytes = Vec::with_capacity(file_len as usize);
    file.take(file_len)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            config_error(format!(
                "failed to read automation run ledger '{}': {error}",
                path.display()
            ))
        })?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        config_error(format!(
            "automation run ledger '{}' is not valid UTF-8: {error}",
            path.display()
        ))
    })?;
    let mut newest = None;
    let mut terminal_seen = false;
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record = serde_json::from_str::<AutomationRunLedgerRecord>(trimmed).map_err(|error| {
            config_error(format!(
                "automation run ledger '{}' contains a malformed row during exact-run lookup: {error}",
                path.display()
            ))
        })?;
        if record.run_id != run_id {
            continue;
        }
        if record.status.is_terminal() && terminal_seen {
            return Err(config_error(format!(
                "automation run ledger '{}' contains ambiguous records for run '{run_id}'",
                path.display()
            )));
        }
        terminal_seen |= record.status.is_terminal();
        if newest.is_none() {
            newest = Some(record);
        }
    }
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::run_ledger::AutomationRunStatus;

    fn ledger_line(run_id: &str, status: &str, completed_at: i64) -> String {
        format!(
            "{{\"schema_version\":2,\"run_id\":\"{run_id}\",\"trigger\":\"scheduler\",\
             \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"{status}\",\
             \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"{completed_at}\",\
             \"completed_at\":\"{completed_at}\",\"completed_at_micros\":{}}}"
            completed_at.saturating_mul(1_000_000),
        )
    }

    fn write_ledger(lines: &[String]) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("ledger");
        (temp, path)
    }

    #[test]
    fn returns_newest_exact_lifecycle_record_within_bound() {
        let lines = vec![
            ledger_line("target", "queued", 1),
            ledger_line("unrelated", "succeeded", 2),
            ledger_line("target", "running", 3),
        ];
        let (_temp, path) = write_ledger(&lines);

        let record = read_exact_run_record_bounded(&path, "target", 64 * 1024)
            .expect("bounded read")
            .expect("exact record");

        assert_eq!(record.run_id, "target");
        assert_eq!(record.status, AutomationRunStatus::Running);
        assert!(
            read_exact_run_record_bounded(&path, "missing", 64 * 1024)
                .expect("bounded read")
                .is_none()
        );
        assert!(read_exact_run_record_bounded(&path, "target", 1).is_err());
    }

    #[test]
    fn fails_closed_on_malformed_or_ambiguous_terminal_rows() {
        let malformed = vec![ledger_line("target", "succeeded", 1), "not json".to_owned()];
        let (_temp, malformed_path) = write_ledger(&malformed);
        assert!(read_exact_run_record_bounded(&malformed_path, "target", 64 * 1024).is_err());

        let duplicate_terminals = vec![
            ledger_line("target", "succeeded", 1),
            ledger_line("target", "failed", 2),
        ];
        let (_temp, ambiguous_path) = write_ledger(&duplicate_terminals);
        assert!(read_exact_run_record_bounded(&ambiguous_path, "target", 64 * 1024).is_err());
    }
}

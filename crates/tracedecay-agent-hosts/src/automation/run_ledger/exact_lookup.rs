use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{AutomationRunLedgerRecord, run_ledger_path, validate_run_id_component};
use crate::automation::config_error;
use crate::errors::Result;

const EXACT_RUN_SCAN_CHUNK_BYTES: usize = 256 * 1024;
const EXACT_RUN_SCAN_MAX_ROW_BYTES: usize = 1024 * 1024;

pub async fn find_run_record_exact_bounded(
    dashboard_root: &Path,
    run_id: &str,
) -> Result<Option<AutomationRunLedgerRecord>> {
    validate_run_id_component(run_id)?;
    let path = run_ledger_path(dashboard_root);
    let requested_run_id = run_id.to_owned();
    tokio::task::spawn_blocking(move || read_exact_run_record_bounded(&path, &requested_run_id))
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
    let mut cursor = file
        .metadata()
        .map_err(|error| {
            config_error(format!(
                "failed to inspect automation run ledger '{}': {error}",
                path.display()
            ))
        })?
        .len();
    let mut suffix = Vec::new();
    let mut newest = None;
    let mut terminal_seen = false;
    while cursor > 0 {
        let chunk_len = usize::try_from(cursor.min(EXACT_RUN_SCAN_CHUNK_BYTES as u64))
            .map_err(|_| config_error("automation exact-run chunk length is not representable"))?;
        cursor = cursor.saturating_sub(chunk_len as u64);
        file.seek(SeekFrom::Start(cursor)).map_err(|error| {
            config_error(format!(
                "failed to seek automation run ledger '{}': {error}",
                path.display()
            ))
        })?;
        let mut chunk = vec![0; chunk_len];
        file.read_exact(&mut chunk).map_err(|error| {
            config_error(format!(
                "failed to scan automation run ledger '{}': {error}",
                path.display()
            ))
        })?;
        chunk.extend_from_slice(&suffix);
        let mut end = chunk.len();
        for newline in chunk
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
        {
            if newline + 1 < end {
                consider_line(
                    path,
                    &chunk[newline + 1..end],
                    run_id,
                    &mut newest,
                    &mut terminal_seen,
                )?;
            }
            end = newline;
        }
        suffix = chunk[..end].to_vec();
        if suffix.len() > EXACT_RUN_SCAN_MAX_ROW_BYTES {
            return Err(config_error(format!(
                "automation run ledger '{}' contains a row exceeding the exact-run scan bound",
                path.display()
            )));
        }
    }
    if !suffix.is_empty() {
        consider_line(path, &suffix, run_id, &mut newest, &mut terminal_seen)?;
    }
    Ok(newest)
}

fn consider_line(
    path: &Path,
    line: &[u8],
    run_id: &str,
    newest: &mut Option<AutomationRunLedgerRecord>,
    terminal_seen: &mut bool,
) -> Result<()> {
    if line.len() > EXACT_RUN_SCAN_MAX_ROW_BYTES {
        return Err(config_error(format!(
            "automation run ledger '{}' contains a row exceeding the exact-run scan bound",
            path.display()
        )));
    }
    let trimmed = std::str::from_utf8(line)
        .map_err(|error| {
            config_error(format!(
                "automation run ledger '{}' is not valid UTF-8: {error}",
                path.display()
            ))
        })?
        .trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let record = serde_json::from_str::<AutomationRunLedgerRecord>(trimmed).map_err(|error| {
        config_error(format!(
            "automation run ledger '{}' contains a malformed row during exact-run lookup: {error}",
            path.display()
        ))
    })?;
    if record.run_id != run_id {
        return Ok(());
    }
    if record.status.is_terminal() && *terminal_seen {
        return Err(config_error(format!(
            "automation run ledger '{}' contains ambiguous records for run '{run_id}'",
            path.display()
        )));
    }
    *terminal_seen |= record.status.is_terminal();
    if newest.is_none() {
        *newest = Some(record);
    }
    Ok(())
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
             \"completed_at\":\"{completed_at}\",\"completed_at_micros\":{}}}",
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
    fn returns_newest_exact_lifecycle_record_across_large_ledger() {
        let mut lines = vec![ledger_line("target", "queued", 1)];
        let padding = "x".repeat(1024);
        for index in 0..9_000 {
            lines.push(format!(
                "{{\"schema_version\":2,\"run_id\":\"unrelated-{index}\",\"trigger\":\"scheduler\",\
                 \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"running\",\
                 \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"2\",\"completed_at\":\"2\",\
                 \"error\":\"{padding}\"}}"
            ));
        }
        lines.push(ledger_line("target", "running", 3));
        let (_temp, path) = write_ledger(&lines);
        assert!(std::fs::metadata(&path).expect("metadata").len() > 8 * 1024 * 1024);

        let record = read_exact_run_record_bounded(&path, "target")
            .expect("bounded read")
            .expect("exact record");

        assert_eq!(record.run_id, "target");
        assert_eq!(record.status, AutomationRunStatus::Running);
        assert!(
            read_exact_run_record_bounded(&path, "missing")
                .expect("bounded read")
                .is_none()
        );
    }

    #[test]
    fn reads_legacy_row_without_fabricating_completion_precision() {
        let line = "{\"schema_version\":1,\"run_id\":\"target\",\"trigger\":\"manual_cli\",\
                    \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"succeeded\",\
                    \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"1\",\"completed_at\":\"2\"}";
        let (_temp, path) = write_ledger(&[line.to_owned()]);

        let record = read_exact_run_record_bounded(&path, "target")
            .expect("bounded read")
            .expect("legacy record");

        assert_eq!(record.completed_at_micros, None);
    }

    #[test]
    fn fails_closed_on_malformed_oversized_or_ambiguous_rows() {
        let malformed = vec![ledger_line("target", "succeeded", 1), "not json".to_owned()];
        let (_temp, malformed_path) = write_ledger(&malformed);
        assert!(read_exact_run_record_bounded(&malformed_path, "target").is_err());

        let oversized = vec![format!("{{\"padding\":\"{}\"}}", "x".repeat(1024 * 1024))];
        let (_temp, oversized_path) = write_ledger(&oversized);
        assert!(read_exact_run_record_bounded(&oversized_path, "target").is_err());

        let duplicate_terminals = vec![
            ledger_line("target", "succeeded", 1),
            ledger_line("target", "failed", 2),
        ];
        let (_temp, ambiguous_path) = write_ledger(&duplicate_terminals);
        assert!(read_exact_run_record_bounded(&ambiguous_path, "target").is_err());
    }
}

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use super::{AutomationRunLedgerRecord, run_ledger_path};
use crate::automation::backend::task_key as canonical_task_key;
use crate::automation::config_error;
use crate::errors::Result;

const CURSOR_SCAN_CHUNK_BYTES: usize = 256 * 1024;
const CURSOR_SCAN_MAX_ROW_BYTES: usize = 1024 * 1024;

pub(super) async fn load_latest_task_validation_pointer(
    dashboard_root: &Path,
    requested_task_key: &str,
    pointer: &str,
) -> Result<Option<Value>> {
    let path = run_ledger_path(dashboard_root);
    let task_key = requested_task_key.to_owned();
    let pointer = pointer.to_owned();
    tokio::task::spawn_blocking(move || {
        read_latest_task_validation_pointer(&path, &task_key, &pointer)
    })
    .await
    .map_err(|error| config_error(format!("failed to join automation cursor read: {error}")))?
}

fn read_latest_task_validation_pointer(
    path: &Path,
    requested_task_key: &str,
    pointer: &str,
) -> Result<Option<Value>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "failed to open automation run ledger '{}': {error}",
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
    while cursor > 0 {
        let chunk_len = usize::try_from(cursor.min(CURSOR_SCAN_CHUNK_BYTES as u64))
            .map_err(|_| config_error("automation cursor chunk length is not representable"))?;
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
            if newline + 1 < end
                && let Some(value) = cursor_value_from_line(
                    path,
                    &chunk[newline + 1..end],
                    requested_task_key,
                    pointer,
                )?
            {
                return Ok(Some(value));
            }
            end = newline;
        }
        suffix = chunk[..end].to_vec();
        if suffix.len() > CURSOR_SCAN_MAX_ROW_BYTES {
            return Err(config_error(format!(
                "automation run ledger '{}' contains a row exceeding the cursor scan bound",
                path.display()
            )));
        }
    }
    if suffix.is_empty() {
        Ok(None)
    } else {
        cursor_value_from_line(path, &suffix, requested_task_key, pointer)
    }
}

fn cursor_value_from_line(
    path: &Path,
    line: &[u8],
    requested_task_key: &str,
    pointer: &str,
) -> Result<Option<Value>> {
    if line.len() > CURSOR_SCAN_MAX_ROW_BYTES {
        return Err(config_error(format!(
            "automation run ledger '{}' contains a row exceeding the cursor scan bound",
            path.display()
        )));
    }
    let trimmed = std::str::from_utf8(line)
        .map_err(|error| {
            config_error(format!(
                "automation run ledger '{}' contains invalid UTF-8 during cursor recovery: {error}",
                path.display()
            ))
        })?
        .trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let record = serde_json::from_str::<AutomationRunLedgerRecord>(trimmed).map_err(|error| {
        config_error(format!(
            "malformed automation run ledger row blocks cursor recovery in '{}': {error}",
            path.display()
        ))
    })?;
    let task_key = record
        .task_key
        .as_deref()
        .unwrap_or_else(|| canonical_task_key(record.task));
    Ok((task_key == requested_task_key)
        .then(|| {
            record
                .validation_report
                .as_ref()
                .and_then(|report| report.pointer(pointer))
                .cloned()
        })
        .flatten())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::io::Write;

    use super::*;

    const POINTER: &str = "/pagination/resume_after_fact_id";

    fn ledger_line(run_id: &str, report: Option<Value>) -> String {
        let mut record = serde_json::json!({
            "schema_version": 2, "run_id": run_id, "trigger": "manual_cli",
            "task": "memory_curator", "task_key": "memory_curator",
            "backend": "codex_app_server", "status": "succeeded",
            "accepted_count": 0, "rejected_count": 0,
            "started_at": "1", "completed_at": "1"
        });
        if let Some(report) = report {
            record["validation_report"] = report;
        }
        serde_json::to_string(&record).unwrap()
    }

    #[test]
    fn cursor_lookup_crosses_more_than_two_hundred_rows_without_pagination() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            ledger_line(
                "cursor",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.cursor"}
                }))
            )
        )
        .unwrap();
        for index in 0..250 {
            writeln!(file, "{}", ledger_line(&format!("failure-{index}"), None)).unwrap();
        }
        drop(file);
        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", POINTER).unwrap(),
            Some(serde_json::json!("fact.cursor"))
        );
    }

    #[test]
    fn cursor_lookup_crosses_a_ledger_larger_than_sixty_four_megabytes() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            ledger_line(
                "cursor",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.cursor"}
                }))
            )
        )
        .unwrap();
        let row = ledger_line("filler", None);
        while file.stream_position().unwrap() <= 65 * 1024 * 1024 {
            writeln!(file, "{row}").unwrap();
        }
        drop(file);
        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", POINTER).unwrap(),
            Some(serde_json::json!("fact.cursor"))
        );
    }

    #[test]
    fn malformed_newer_row_blocks_cursor_recovery() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(
            &path,
            format!(
                "{}\nnot-json\n",
                ledger_line(
                    "cursor",
                    Some(serde_json::json!({"pagination": {"resume_after_fact_id": null}}))
                )
            ),
        )
        .unwrap();
        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn oversized_single_row_fails_closed() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(&path, vec![b'x'; CURSOR_SCAN_MAX_ROW_BYTES + 1]).unwrap();
        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }
}

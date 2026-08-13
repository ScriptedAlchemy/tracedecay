use std::path::Path;

use serde_json::Value;

use super::exact_lookup::{
    ForwardJsonlScanner, canonical_completion_key, extract_json_pointer_bounded,
    json_pointer_exists, read_logical_run_lifecycle, scan_jsonl_row,
};
use super::run_ledger_path;
use crate::automation::backend::task_key as canonical_task_key;
use crate::automation::config_error;
use crate::errors::Result;

const CURSOR_SELECTED_VALUE_MAX_BYTES: usize = 4 * 1024;

pub(crate) async fn load_latest_task_validation_pointer(
    dashboard_root: &Path,
    requested_task_key: &str,
    pointer: &str,
) -> Result<Option<Value>> {
    if requested_task_key.len() > tracedecay_domain::canonical_text::CANONICAL_TEXT_MAX_BYTES {
        return Err(config_error(
            "requested automation task key exceeds its byte bound",
        ));
    }
    if pointer.len() > super::exact_lookup::JSON_POINTER_MAX_BYTES {
        return Err(config_error(
            "requested automation JSON pointer exceeds its byte bound",
        ));
    }
    let root = dashboard_root.to_path_buf();
    let path = run_ledger_path(dashboard_root);
    let task_key = requested_task_key.to_owned();
    let pointer = pointer.to_owned();
    tokio::task::spawn_blocking(move || {
        super::with_run_ledger_read_lock(&root, &path, || {
            read_latest_task_validation_pointer(&path, &task_key, &pointer)
        })
    })
    .await
    .map_err(|error| config_error(format!("failed to join automation cursor read: {error}")))?
}

fn read_latest_task_validation_pointer(
    path: &Path,
    requested_task_key: &str,
    pointer: &str,
) -> Result<Option<Value>> {
    let Some(file) = super::exact_lookup::open_stabilized_run_ledger(path, false)? else {
        return Ok(None);
    };
    let mut lines = ForwardJsonlScanner::new(&file, path)?;
    let mut selected = None;
    while let Some(line) = lines.next_span()? {
        let Some(row) = scan_jsonl_row(&file, path, line)? else {
            continue;
        };
        let task_key = row
            .task_key
            .as_deref()
            .unwrap_or_else(|| canonical_task_key(row.task));
        if task_key != requested_task_key {
            continue;
        }
        let Some(validation_report) = row.validation_report.clone() else {
            continue;
        };
        if !json_pointer_exists(&file, path, validation_report, pointer)? {
            continue;
        }
        let row_key = canonical_completion_key(&row)?;
        let replace = selected
            .as_ref()
            .map(canonical_completion_key)
            .transpose()?
            .is_none_or(|selected_key| row_key > selected_key);
        if replace {
            selected = Some(row);
        }
    }
    let Some(selected) = selected else {
        return Ok(None);
    };
    let selected_key = canonical_completion_key(&selected)?;
    let lifecycle = read_logical_run_lifecycle(&file, path, &selected.run_id)?
        .ok_or_else(|| config_error("automation cursor selected run disappeared"))?;
    let task_key = lifecycle
        .newest
        .task_key
        .as_deref()
        .unwrap_or_else(|| canonical_task_key(lifecycle.newest.task));
    if task_key != requested_task_key {
        return Err(config_error(
            "automation cursor logical newest lifecycle is outside the requested task",
        ));
    }
    if canonical_completion_key(&lifecycle.newest)? != selected_key {
        return Err(config_error(
            "automation cursor logical lifecycle changed its completion order",
        ));
    }
    let Some(validation_report) = lifecycle.newest.validation_report else {
        return Err(config_error(
            "automation cursor logical newest lifecycle has no validation report",
        ));
    };
    extract_json_pointer_bounded(
        &file,
        path,
        validation_report,
        pointer,
        CURSOR_SELECTED_VALUE_MAX_BYTES,
    )?
    .map(Some)
    .ok_or_else(|| config_error("automation cursor logical newest lifecycle has no pointer"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::io::{Seek, Write};

    use super::*;

    const POINTER: &str = "/pagination/resume_after_fact_id";

    fn ledger_line(run_id: &str, report: Option<Value>) -> String {
        let mut record = serde_json::json!({
            "schema_version": 2, "run_id": run_id, "trigger": "manual_cli",
            "task": "memory_curator", "task_key": "memory_curator",
            "backend": "codex_app_server", "status": "succeeded",
            "accepted_count": 0, "rejected_count": 0,
            "started_at": "1", "completed_at": "1", "completed_at_micros": 1_000_000
        });
        if let Some(report) = report {
            record["validation_report"] = report;
        }
        serde_json::to_string(&record).unwrap()
    }

    fn with_completion(line: String, completed_at: i64) -> String {
        line.replace(
            "\"completed_at\":\"1\"",
            &format!("\"completed_at\":\"{completed_at}\""),
        )
        .replace(
            "\"completed_at_micros\":1000000",
            &format!("\"completed_at_micros\":{}", completed_at * 1_000_000),
        )
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
    fn unrelated_multi_megabyte_row_does_not_hide_older_cursor() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let cursor = ledger_line(
            "cursor",
            Some(serde_json::json!({
                "pagination": {"resume_after_fact_id": "fact.cursor"}
            })),
        );
        let unrelated = format!(
            "{{\"schema_version\":2,\"run_id\":\"skill\",\"trigger\":\"scheduler\",\"task\":\"skill_writer\",\"task_key\":\"skill_writer\",\"backend\":\"codex_app_server\",\"status\":\"succeeded\",\"validation_report\":{{\"payload\":\"{}\"}},\"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"1\",\"completed_at\":\"1\"}}",
            "x".repeat(2 * 1024 * 1024)
        );
        std::fs::write(&path, format!("{cursor}\n{unrelated}\n")).unwrap();

        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", POINTER).unwrap(),
            Some(serde_json::json!("fact.cursor"))
        );
    }

    #[test]
    fn cursor_projection_is_independent_of_top_level_field_order() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(
            &path,
            "{\"validation_report\":{\"pagination\":{\"resume_after_fact_id\":\"fact.cursor\"}},\"completed_at\":\"1\",\"started_at\":\"1\",\"rejected_count\":0,\"accepted_count\":0,\"backend\":\"codex_app_server\",\"task_key\":\"memory_curator\",\"status\":\"succeeded\",\"task\":\"memory_curator\",\"trigger\":\"scheduler\",\"run_id\":\"cursor\",\"schema_version\":2}\n",
        )
        .unwrap();

        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", POINTER).unwrap(),
            Some(serde_json::json!("fact.cursor"))
        );
    }

    #[test]
    fn malformed_nonprojected_newer_row_blocks_cursor_recovery() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let older = ledger_line(
            "older",
            Some(serde_json::json!({
                "pagination": {"resume_after_fact_id": "fact.older"}
            })),
        );
        let malformed = ledger_line(
            "newer",
            Some(serde_json::json!({
                "pagination": {"resume_after_fact_id": "fact.invalid"}
            })),
        )
        .replace("\"accepted_count\":0,", "\"accepted_count\":false,");
        std::fs::write(&path, format!("{older}\n{malformed}\n")).unwrap();

        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn unrelated_schema_invalid_row_blocks_cursor_recovery() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let cursor = ledger_line(
            "cursor",
            Some(serde_json::json!({
                "pagination": {"resume_after_fact_id": "fact.cursor"}
            })),
        );
        let unrelated = ledger_line("unrelated", None)
            .replace("\"task\":\"memory_curator\"", "\"task\":\"skill_writer\"")
            .replace("\"accepted_count\":0", "\"accepted_count\":false");
        std::fs::write(&path, format!("{cursor}\n{unrelated}\n")).unwrap();

        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn same_task_semantically_invalid_row_without_pointer_blocks_cursor_recovery() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let cursor = ledger_line(
            "cursor",
            Some(serde_json::json!({
                "pagination": {"resume_after_fact_id": "fact.cursor"}
            })),
        );
        let invalid = ledger_line("invalid", None).replace(
            "\"completed_at\":\"1\"",
            "\"completed_at\":\"9223372036854775807\"",
        );
        std::fs::write(&path, format!("{cursor}\n{invalid}\n")).unwrap();

        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn newer_same_run_task_mutation_blocks_older_cursor() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let older = with_completion(
            ledger_line(
                "same-run",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.cursor"}
                })),
            )
            .replace("\"succeeded\"", "\"queued\""),
            1,
        );
        let newer = with_completion(
            ledger_line("same-run", None)
                .replace("\"succeeded\"", "\"running\"")
                .replace("\"task\":\"memory_curator\"", "\"task\":\"skill_writer\"")
                .replace(
                    "\"task_key\":\"memory_curator\"",
                    "\"task_key\":\"skill_writer\"",
                ),
            2,
        );
        std::fs::write(&path, format!("{older}\n{newer}\n")).unwrap();

        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn newer_same_run_without_pointer_shadows_older_cursor() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let older = with_completion(
            ledger_line(
                "same-run",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.stale"}
                })),
            )
            .replace("\"succeeded\"", "\"queued\""),
            1,
        );
        let newer = with_completion(
            ledger_line("same-run", None).replace("\"succeeded\"", "\"running\""),
            2,
        );
        std::fs::write(&path, format!("{older}\n{newer}\n")).unwrap();

        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn historical_retry_does_not_regress_logical_cursor() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let queued = with_completion(
            ledger_line(
                "same-run",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.queued"}
                })),
            )
            .replace("\"succeeded\"", "\"queued\""),
            1,
        );
        let running = with_completion(
            ledger_line(
                "same-run",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.running"}
                })),
            )
            .replace("\"succeeded\"", "\"running\""),
            2,
        );
        std::fs::write(&path, format!("{queued}\n{running}\n{queued}\n")).unwrap();

        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", POINTER).unwrap(),
            Some(serde_json::json!("fact.running"))
        );
    }

    #[test]
    fn cross_run_retry_does_not_displace_newer_logical_cursor() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let queued = with_completion(
            ledger_line(
                "run-a",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.a.queued"}
                })),
            )
            .replace("\"succeeded\"", "\"queued\""),
            1,
        );
        let running = with_completion(
            ledger_line(
                "run-a",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.a.running"}
                })),
            )
            .replace("\"succeeded\"", "\"running\""),
            2,
        );
        let succeeded = with_completion(
            ledger_line(
                "run-b",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.b"}
                })),
            ),
            3,
        );
        std::fs::write(
            &path,
            format!("{queued}\n{running}\n{succeeded}\n{queued}\n"),
        )
        .unwrap();

        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", POINTER).unwrap(),
            Some(serde_json::json!("fact.b"))
        );
    }

    #[test]
    fn terminal_lifecycle_regression_blocks_cursor() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let terminal = ledger_line(
            "same-run",
            Some(serde_json::json!({
                "pagination": {"resume_after_fact_id": "fact.terminal"}
            })),
        );
        let running = ledger_line(
            "same-run",
            Some(serde_json::json!({
                "pagination": {"resume_after_fact_id": "fact.running"}
            })),
        )
        .replace("\"succeeded\"", "\"running\"");
        std::fs::write(&path, format!("{terminal}\n{running}\n")).unwrap();

        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn completion_time_regression_blocks_cursor() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let queued = with_completion(
            ledger_line(
                "same-run",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.queued"}
                })),
            )
            .replace("\"succeeded\"", "\"queued\""),
            300,
        );
        let running = with_completion(
            ledger_line(
                "same-run",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.running"}
                })),
            )
            .replace("\"succeeded\"", "\"running\""),
            100,
        );
        std::fs::write(&path, format!("{queued}\n{running}\n")).unwrap();

        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn pointer_supports_arrays_and_rfc_6901_escapes() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(
            &path,
            format!(
                "{}\n",
                ledger_line(
                    "cursor",
                    Some(serde_json::json!({"a/b": [{"~key": "fact.cursor"}]}))
                )
            ),
        )
        .unwrap();

        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", "/a~1b/0/~0key").unwrap(),
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
    fn oversized_selected_cursor_value_fails_closed() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(
            &path,
            format!(
                "{}\n",
                ledger_line(
                    "cursor",
                    Some(serde_json::json!({
                        "pagination": {
                            "resume_after_fact_id": "x".repeat(CURSOR_SELECTED_VALUE_MAX_BYTES)
                        }
                    }))
                )
            ),
        )
        .unwrap();
        assert!(read_latest_task_validation_pointer(&path, "memory_curator", POINTER).is_err());
    }

    #[test]
    fn cursor_rejects_valid_json_without_commit_newline() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(
            &path,
            ledger_line(
                "cursor",
                Some(serde_json::json!({
                    "pagination": {"resume_after_fact_id": "fact.cursor"}
                })),
            ),
        )
        .unwrap();

        let error = read_latest_task_validation_pointer(&path, "memory_curator", POINTER)
            .expect_err("unterminated ledger row must not advance cursor");

        assert!(error.to_string().contains("incomplete durable tail"));
    }

    #[test]
    fn cursor_skips_unrelated_unbounded_value_keys() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        let mut report = serde_json::Map::new();
        report.insert(
            "κ\\u0065y".repeat(super::super::exact_lookup::JSON_POINTER_MAX_BYTES),
            Value::Null,
        );
        report.insert(
            "pagination".to_owned(),
            serde_json::json!({"resume_after_fact_id": "fact.cursor"}),
        );
        std::fs::write(
            &path,
            format!("{}\n", ledger_line("cursor", Some(Value::Object(report)))),
        )
        .unwrap();

        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", POINTER).unwrap(),
            Some(serde_json::json!("fact.cursor"))
        );

        let mut report = serde_json::Map::new();
        report.insert(
            "pagination".to_owned(),
            serde_json::json!({"resume_after_fact_id": "fact.cursor"}),
        );
        report.insert(
            "κ\\u0065y".repeat(super::super::exact_lookup::JSON_POINTER_MAX_BYTES),
            Value::Null,
        );
        std::fs::write(
            &path,
            format!("{}\n", ledger_line("cursor", Some(Value::Object(report)))),
        )
        .unwrap();
        assert_eq!(
            read_latest_task_validation_pointer(&path, "memory_curator", POINTER).unwrap(),
            Some(serde_json::json!("fact.cursor"))
        );
    }
}

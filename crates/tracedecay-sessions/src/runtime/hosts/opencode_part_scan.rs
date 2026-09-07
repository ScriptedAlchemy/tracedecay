use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::params;

use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{TranscriptIngestError, TranscriptIngestResult};

use super::opencode::{
    MAX_ID_BYTES, MAX_MESSAGES_PER_PAGE, OpenCodeMessageRef, OpenCodePageCursor,
    OpenCodeReferencePage, OpenCodeScanSource, install_progress_handler, open_scan_connection,
    sql_text,
};

const PROVIDER: &str = "opencode";

pub(super) fn scan_part_reference_page(
    source: &OpenCodeScanSource,
    cursor: OpenCodePageCursor,
    mut budget: HostScanBudget,
) -> TranscriptIngestResult<(OpenCodeReferencePage, HostScanBudget)> {
    let Some(connection) = open_scan_connection(source, &mut budget)? else {
        return Ok((
            OpenCodeReferencePage {
                references: Vec::new(),
                next: cursor,
                source_complete: true,
            },
            budget,
        ));
    };
    install_progress_handler(&connection, &source.source_path, &budget)?;
    let matcher = source.scope_matcher();
    let mut statement = connection
        .prepare(
            "SELECT p.rowid AS change_rowid,
                    m.rowid AS message_rowid,
                    CASE WHEN length(m.id) <= ?1 THEN m.id ELSE NULL END AS message_id,
                    CASE WHEN length(m.session_id) <= ?1 THEN m.session_id ELSE NULL END
                        AS session_id,
                    CASE WHEN length(s.directory) <= ?1 THEN s.directory ELSE NULL END
                        AS directory,
                    length(m.data) AS message_bytes,
                    COALESCE((
                        SELECT SUM(length(all_parts.data))
                        FROM part all_parts
                        WHERE all_parts.message_id = m.id
                    ), 0) AS part_bytes,
                    COALESCE((
                        SELECT MAX(length(all_parts.data))
                        FROM part all_parts
                        WHERE all_parts.message_id = m.id
                    ), 0) AS max_part_bytes,
                    (
                        SELECT COUNT(*)
                        FROM part all_parts
                        WHERE all_parts.message_id = m.id
                    ) AS part_count,
                    (
                        SELECT COUNT(*) - 1
                        FROM message ordered
                        WHERE ordered.session_id = m.session_id
                          AND ordered.rowid <= m.rowid
                    ) AS source_order
             FROM part p
             JOIN message m ON m.id = p.message_id
             JOIN session s ON s.id = m.session_id
             WHERE p.rowid > ?2
             ORDER BY p.rowid
             LIMIT ?3",
        )
        .map_err(|error| scan_error("prepare part change query", &source.source_path, error))?;
    let mut rows = statement
        .query(params![
            MAX_ID_BYTES,
            cursor.after_rowid,
            i64::try_from(MAX_MESSAGES_PER_PAGE).map_err(|_| invalid_frame())?
        ])
        .map_err(|error| scan_error("query part changes", &source.source_path, error))?;
    let mut references = BTreeMap::new();
    let mut rows_seen = 0_usize;
    let mut after_rowid = cursor.after_rowid;
    let mut query_exhausted = false;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => {
                query_exhausted = true;
                break;
            }
            Err(error) => {
                if !budget.checkpoint() {
                    break;
                }
                return Err(scan_error(
                    "read part change reference",
                    &source.source_path,
                    error,
                ));
            }
        };
        if !budget.try_charge_unit() {
            break;
        }
        rows_seen = rows_seen.saturating_add(1);
        after_rowid = row
            .get(0)
            .map_err(|error| scan_error("decode part change rowid", &source.source_path, error))?;
        let message_rowid = row.get(1).map_err(|error| {
            scan_error("decode changed message rowid", &source.source_path, error)
        })?;
        let id = sql_text(row, 2, &source.source_path, "decode changed message id")?;
        let session_id = sql_text(row, 3, &source.source_path, "decode changed session id")?;
        let directory = sql_text(
            row,
            4,
            &source.source_path,
            "decode changed session directory",
        )?;
        let message_bytes = row.get::<_, Option<i64>>(5).map_err(|error| {
            scan_error("decode changed message length", &source.source_path, error)
        })?;
        let part_bytes = row.get::<_, i64>(6).map_err(|error| {
            scan_error("decode changed part length", &source.source_path, error)
        })?;
        let max_part_bytes = row.get::<_, i64>(7).map_err(|error| {
            scan_error(
                "decode changed maximum part length",
                &source.source_path,
                error,
            )
        })?;
        let part_count = row
            .get::<_, i64>(8)
            .map_err(|error| scan_error("decode changed part count", &source.source_path, error))?;
        let order = row.get::<_, i64>(9).map_err(|error| {
            scan_error("decode changed message order", &source.source_path, error)
        })?;
        let (Some(id), Some(session_id), Some(directory), Some(message_bytes)) =
            (id, session_id, directory, message_bytes)
        else {
            budget.mark_non_durable();
            continue;
        };
        if !matcher.accepts(Some(Path::new(&directory))) {
            continue;
        }
        let (Ok(message_bytes), Ok(part_bytes), Ok(max_part_bytes), Ok(part_count), Ok(order)) = (
            u64::try_from(message_bytes),
            u64::try_from(part_bytes),
            u64::try_from(max_part_bytes),
            usize::try_from(part_count),
            u64::try_from(order),
        ) else {
            budget.mark_non_durable();
            continue;
        };
        references.insert(
            message_rowid,
            OpenCodeMessageRef {
                rowid: message_rowid,
                id,
                session_id,
                order,
                measured_bytes: message_bytes.saturating_add(part_bytes),
                max_field_bytes: message_bytes.max(max_part_bytes),
                part_count,
            },
        );
    }
    Ok((
        OpenCodeReferencePage {
            references: references.into_values().collect(),
            next: OpenCodePageCursor { after_rowid },
            source_complete: query_exhausted && rows_seen < MAX_MESSAGES_PER_PAGE,
        },
        budget,
    ))
}

fn scan_error(
    operation: &'static str,
    path: &Path,
    error: impl std::error::Error + Send + Sync + 'static,
) -> TranscriptIngestError {
    TranscriptIngestError::ScanIo {
        operation,
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    }
}

const fn invalid_frame() -> TranscriptIngestError {
    TranscriptIngestError::InvalidFrameState { provider: PROVIDER }
}

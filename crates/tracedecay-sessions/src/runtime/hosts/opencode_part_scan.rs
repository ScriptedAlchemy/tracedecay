use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, params};

use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::TranscriptIngestResult;

use super::opencode::{
    MAX_ID_BYTES, MAX_MESSAGES_PER_PAGE, OpenCodeMessageRef, OpenCodePageCursor,
    OpenCodeReferencePage, OpenCodeScanSource, install_progress_handler, invalid_frame, scan_error,
    sql_text,
};

pub(super) fn scan_part_reference_page(
    connection: &Connection,
    source: &OpenCodeScanSource,
    cursor: OpenCodePageCursor,
    mut budget: HostScanBudget,
) -> TranscriptIngestResult<(OpenCodeReferencePage, HostScanBudget)> {
    install_progress_handler(connection, &source.source_path, &budget)?;
    let matcher = source.scope_matcher();
    let mut statement = connection
        .prepare(
            "WITH page AS (
                 SELECT p.rowid AS change_rowid, m.rowid AS message_rowid,
                        m.id, m.session_id, s.directory, length(m.data) AS message_bytes
                 FROM part p
                 JOIN message m ON m.id = p.message_id
                 JOIN session s ON s.id = m.session_id
                 WHERE p.rowid > ?2
                 ORDER BY p.rowid
                 LIMIT ?3
             ),
             part_sums AS (
                 SELECT all_parts.message_id,
                        SUM(length(all_parts.data)) AS part_bytes,
                        MAX(length(all_parts.data)) AS max_part_bytes,
                        COUNT(*) AS part_count
                 FROM part all_parts
                 WHERE all_parts.message_id IN (SELECT id FROM page)
                 GROUP BY all_parts.message_id
             )
             SELECT page.change_rowid,
                    page.message_rowid,
                    CASE WHEN length(page.id) <= ?1 THEN page.id ELSE NULL END AS message_id,
                    CASE WHEN length(page.session_id) <= ?1 THEN page.session_id ELSE NULL END
                        AS session_id,
                    CASE WHEN length(page.directory) <= ?1 THEN page.directory ELSE NULL END
                        AS directory,
                    page.message_bytes,
                    COALESCE(part_sums.part_bytes, 0) AS part_bytes,
                    COALESCE(part_sums.max_part_bytes, 0) AS max_part_bytes,
                    COALESCE(part_sums.part_count, 0) AS part_count,
                    (
                        SELECT COUNT(*) - 1
                        FROM message ordered
                        WHERE ordered.session_id = page.session_id
                          AND ordered.rowid <= page.message_rowid
                    ) AS source_order
             FROM page
             LEFT JOIN part_sums ON part_sums.message_id = page.id
             ORDER BY page.change_rowid",
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

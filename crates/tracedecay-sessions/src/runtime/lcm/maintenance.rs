use std::collections::BTreeSet;

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::{LCM_SCAN_PAGE_ROWS, LcmError};

pub(super) async fn all_payload_metadata_refs(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<BTreeSet<String>, LcmError> {
    payload_metadata_refs(conn, "all", None).await
}

pub(super) async fn payload_metadata_refs_for_scope(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    payload_metadata_refs(conn, provider, session_id).await
}

/// Collects payload-metadata references through `rowid` keyset pages: the
/// whole `lcm_external_payloads` table exceeds what the `SQLite` runtime will
/// materialize for one query on a long-lived profile.
async fn payload_metadata_refs(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: Option<&str>,
) -> Result<BTreeSet<String>, LcmError> {
    let mut refs = BTreeSet::new();
    let mut after_rowid = 0_i64;
    loop {
        let mut rows = conn
            .query(
                "SELECT payload_ref, rowid
                 FROM lcm_external_payloads
                 WHERE (?1 = 'all' OR provider = ?1)
                   AND (?2 IS NULL OR session_id = ?2)
                   AND rowid > ?3
                 ORDER BY rowid
                 LIMIT ?4",
                params![provider, session_id, after_rowid, LCM_SCAN_PAGE_ROWS],
            )
            .await?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows.next().await? {
            refs.insert(row.get(0)?);
            after_rowid = row.get(1)?;
            page_rows += 1;
        }
        drop(rows);
        if page_rows < LCM_SCAN_PAGE_ROWS {
            return Ok(refs);
        }
    }
}

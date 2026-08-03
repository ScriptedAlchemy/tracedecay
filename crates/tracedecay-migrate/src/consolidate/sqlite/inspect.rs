use std::collections::HashSet;
use std::path::{Path, PathBuf};

use libsql::{Builder, Connection, Database as LibsqlDatabase};

use super::{
    LCM_RAW_MESSAGE_DIVERGENCE_PREDICATE, attach, db_error, db_message, query_i64,
    quote_identifier, table_exists,
};
use crate::errors::Result;

#[derive(Debug, Clone, Copy)]
pub(in crate::consolidate) struct DatabaseCollisionCounts {
    pub sessions: u64,
    pub messages: u64,
    pub lcm_messages: u64,
    pub divergent_lcm_messages: u64,
    pub divergent_lcm_session_ids: u64,
    pub divergent_lcm_content_hashes: u64,
    pub divergent_lcm_storage_kinds: u64,
    pub divergent_lcm_payload_refs: u64,
}

#[derive(Debug, Default)]
struct LcmMessageCollisionCounts {
    overlaps: u64,
    divergent: u64,
    session_ids: u64,
    content_hashes: u64,
    storage_kinds: u64,
    payload_refs: u64,
}

pub(in crate::consolidate) struct OfflineDatabaseGuards {
    _holds: Vec<(Connection, LibsqlDatabase)>,
    _authorities: Vec<crate::db::DatabaseAuthority>,
}

#[derive(Default)]
pub(in crate::consolidate) struct GraphLogicalIdentities {
    facts: HashSet<Vec<u8>>,
    feedback: HashSet<Vec<u8>>,
}

impl GraphLogicalIdentities {
    pub(in crate::consolidate) fn fact_count(&self) -> u64 {
        self.facts.len() as u64
    }

    pub(in crate::consolidate) fn feedback_count(&self) -> u64 {
        self.feedback.len() as u64
    }

    pub(in crate::consolidate) fn fact_overlap(&self, other: &Self) -> u64 {
        self.facts.intersection(&other.facts).count() as u64
    }

    pub(in crate::consolidate) fn facts_union_matches(
        &self,
        other: &Self,
        destination: &Self,
    ) -> bool {
        destination.facts.len() == self.facts.union(&other.facts).count()
            && destination
                .facts
                .iter()
                .all(|key| self.facts.contains(key) || other.facts.contains(key))
    }

    pub(in crate::consolidate) fn feedback_union_matches(
        &self,
        other: &Self,
        destination: &Self,
    ) -> bool {
        destination.feedback.len() == self.feedback.union(&other.feedback).count()
            && destination
                .feedback
                .iter()
                .all(|key| self.feedback.contains(key) || other.feedback.contains(key))
    }
}

pub(in crate::consolidate) async fn extend_graph_identities(
    conn: &Connection,
    identities: &mut GraphLogicalIdentities,
) -> Result<()> {
    if table_exists(conn, "main", "memory_facts").await? {
        read_fact_keys(conn, &mut identities.facts).await?;
    }
    if table_exists(conn, "main", "memory_feedback_events").await? {
        read_feedback_keys(conn, &mut identities.feedback).await?;
    }
    Ok(())
}

#[cfg(all(test, windows))]
pub(in crate::consolidate) async fn acquire_offline_guards(
    paths: &[PathBuf],
) -> Result<OfflineDatabaseGuards> {
    // Windows byte-range locks prevent the same process from copying a file
    // after BEGIN IMMEDIATE. Production consolidation already fails closed on
    // Windows because open-holder discovery is unsupported; unit fixtures use
    // isolated stores and exercise the remaining migration semantics here.
    let _ = paths;
    Ok(OfflineDatabaseGuards {
        _holds: Vec::new(),
        _authorities: Vec::new(),
    })
}

#[cfg(not(all(test, windows)))]
pub(in crate::consolidate) async fn acquire_offline_guards(
    paths: &[PathBuf],
) -> Result<OfflineDatabaseGuards> {
    let mut ordered = paths.to_vec();
    ordered.sort();
    ordered.dedup();
    let mut holds = Vec::new();
    let mut authorities = Vec::new();
    for path in ordered {
        if !crate::storage::has_sqlite_database_header(&path).map_err(|error| {
            db_error(
                "acquire_offline_guards",
                format!("failed to inspect '{}': {error}", path.display()),
            )
        })? {
            return Err(db_message(
                "acquire_offline_guards",
                format!("file is not a database: '{}'", path.display()),
            ));
        }
        let authority = crate::db::DatabaseAuthority::for_runtime(
            &path,
            "consolidate SQLite database offline",
        )?;
        let db = Builder::new_local(&path)
            .build()
            .await
            .map_err(|error| db_error("acquire_offline_guards", error))?;
        let conn = db
            .connect()
            .map_err(|error| db_error("acquire_offline_guards", error))?;
        conn.execute_batch("PRAGMA busy_timeout = 0;")
            .await
            .map_err(|error| db_error("acquire_offline_guards", error))?;
        conn.execute("BEGIN IMMEDIATE", ()).await.map_err(|error| {
            db_message(
                "acquire_offline_guards",
                format!(
                    "database '{}' is still writable by another process: {error}; stop MCP/CLI writers and retry",
                    path.display()
                ),
            )
        })?;
        holds.push((conn, db));
        authorities.push(authority);
    }
    Ok(OfflineDatabaseGuards {
        _holds: holds,
        _authorities: authorities,
    })
}

async fn read_fact_keys(conn: &Connection, identities: &mut HashSet<Vec<u8>>) -> Result<()> {
    let mut rows = conn
        .query("SELECT content FROM memory_facts", ())
        .await
        .map_err(|error| db_error("logical_identities", error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("logical_identities", error))?
    {
        let mut key = Vec::new();
        push_text(
            &mut key,
            &row.get::<String>(0)
                .map_err(|error| db_error("logical_identities", error))?,
        );
        identities.insert(key);
    }
    Ok(())
}

async fn read_feedback_keys(conn: &Connection, identities: &mut HashSet<Vec<u8>>) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT f.content, e.action, e.trust_delta, e.old_trust,
                    e.new_trust, e.created_at, e.source, e.note
             FROM memory_feedback_events e
             JOIN memory_facts f ON f.fact_id=e.fact_id",
            (),
        )
        .await
        .map_err(|error| db_error("logical_identities", error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("logical_identities", error))?
    {
        let mut key = Vec::new();
        push_text(&mut key, &row_text(&row, 0)?);
        push_text(&mut key, &row_text(&row, 1)?);
        for index in 2..5 {
            push_f64(&mut key, row.get::<f64>(index).map_err(logical_error)?);
        }
        key.extend_from_slice(&row.get::<i64>(5).map_err(logical_error)?.to_be_bytes());
        push_text(&mut key, &row_text(&row, 6)?);
        match row.get::<Option<String>>(7).map_err(logical_error)? {
            Some(note) => {
                key.push(1);
                push_text(&mut key, &note);
            }
            None => key.push(0),
        }
        identities.insert(key);
    }
    Ok(())
}

fn row_text(row: &libsql::Row, index: i32) -> Result<String> {
    row.get::<String>(index).map_err(logical_error)
}

fn logical_error(error: libsql::Error) -> crate::errors::TraceDecayError {
    db_error("logical_identities", error)
}

fn push_text(target: &mut Vec<u8>, value: &str) {
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value.as_bytes());
}

fn push_f64(target: &mut Vec<u8>, value: f64) {
    let normalized = if value == 0.0 { 0.0 } else { value };
    target.extend_from_slice(&normalized.to_bits().to_be_bytes());
}

pub async fn count_rows(path: &Path, table: &str) -> Result<u64> {
    let snapshots = crate::sqlite_read_snapshot::SnapshotSet::capture(&[path.to_path_buf()])
        .await
        .map_err(|error| db_error("read_snapshot", error))?;
    count_rows_in(&snapshots, path, table).await
}

pub(in crate::consolidate) async fn count_rows_in(
    snapshots: &crate::sqlite_read_snapshot::SnapshotSet,
    path: &Path,
    table: &str,
) -> Result<u64> {
    if !path.is_file() {
        return Ok(0);
    }
    let db = read_snapshot(snapshots, path)?;
    let count = if table_exists(db.connection(), "main", table).await? {
        query_i64(
            db.connection(),
            &format!("SELECT COUNT(*) FROM {}", quote_identifier(table)),
        )
        .await?
    } else {
        0
    };
    u64::try_from(count).map_err(|error| db_error("count_rows", error))
}

pub(in crate::consolidate) async fn quick_check_in(
    snapshots: &crate::sqlite_read_snapshot::SnapshotSet,
    path: &Path,
) -> Result<()> {
    let db = read_snapshot(snapshots, path)?;
    quick_check_connection(db.connection(), path).await
}

pub(in crate::consolidate) async fn quick_check_connection(
    conn: &Connection,
    path: &Path,
) -> Result<()> {
    let mut rows = conn
        .query("PRAGMA quick_check", ())
        .await
        .map_err(|error| db_error("quick_check", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error("quick_check", error))?
        .ok_or_else(|| db_message("quick_check", "quick_check returned no row"))?;
    let value = row
        .get::<String>(0)
        .map_err(|error| db_error("quick_check", error))?;
    if value != "ok" {
        return Err(db_message(
            "quick_check",
            format!(
                "SQLite quick_check failed for '{}': {value}",
                path.display()
            ),
        ));
    }
    Ok(())
}

pub(in crate::consolidate) async fn inspect_collisions(
    snapshots: &crate::sqlite_read_snapshot::SnapshotSet,
    source_sessions: &Path,
    target_sessions: &Path,
) -> Result<DatabaseCollisionCounts> {
    let sessions = overlap_count(
        snapshots,
        source_sessions,
        target_sessions,
        "sessions",
        "s.provider = t.provider AND s.session_id = t.session_id",
    )
    .await?;
    let messages = overlap_count(
        snapshots,
        source_sessions,
        target_sessions,
        "session_messages",
        "s.provider = t.provider AND s.message_id = t.message_id",
    )
    .await?;
    let lcm = lcm_message_collision_counts(snapshots, source_sessions, target_sessions).await?;
    Ok(DatabaseCollisionCounts {
        sessions,
        messages,
        lcm_messages: lcm.overlaps,
        divergent_lcm_messages: lcm.divergent,
        divergent_lcm_session_ids: lcm.session_ids,
        divergent_lcm_content_hashes: lcm.content_hashes,
        divergent_lcm_storage_kinds: lcm.storage_kinds,
        divergent_lcm_payload_refs: lcm.payload_refs,
    })
}

async fn overlap_count(
    snapshots: &crate::sqlite_read_snapshot::SnapshotSet,
    source: &Path,
    target: &Path,
    table: &str,
    join: &str,
) -> Result<u64> {
    if !source.is_file() || !target.is_file() {
        return Ok(0);
    }
    let source = read_snapshot(snapshots, source)?;
    let target = read_snapshot(snapshots, target)?;
    let conn = source.connection();
    attach(conn, target.path()).await?;
    let count =
        if table_exists(conn, "main", table).await? && table_exists(conn, "other", table).await? {
            query_i64(
                conn,
                &format!(
                    "SELECT COUNT(*) FROM main.{} s JOIN other.{} t ON {join}",
                    quote_identifier(table),
                    quote_identifier(table)
                ),
            )
            .await?
        } else {
            0
        };
    conn.execute("DETACH DATABASE other", ())
        .await
        .map_err(|error| db_error("inspect_collisions", error))?;
    u64::try_from(count).map_err(|error| db_error("inspect_collisions", error))
}

async fn lcm_message_collision_counts(
    snapshots: &crate::sqlite_read_snapshot::SnapshotSet,
    source: &Path,
    target: &Path,
) -> Result<LcmMessageCollisionCounts> {
    if !source.is_file() || !target.is_file() {
        return Ok(LcmMessageCollisionCounts::default());
    }
    let source = read_snapshot(snapshots, source)?;
    let target = read_snapshot(snapshots, target)?;
    let conn = source.connection();
    attach(conn, target.path()).await?;
    let counts = if table_exists(conn, "main", "lcm_raw_messages").await?
        && table_exists(conn, "other", "lcm_raw_messages").await?
    {
        let sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN {LCM_RAW_MESSAGE_DIVERGENCE_PREDICATE} THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN t.session_id IS NOT s.session_id THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN t.content_hash IS NOT s.content_hash THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN t.storage_kind IS NOT s.storage_kind THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN t.payload_ref IS NOT s.payload_ref THEN 1 ELSE 0 END), 0)
             FROM main.lcm_raw_messages s
             JOIN other.lcm_raw_messages t
               ON s.provider=t.provider AND s.message_id=t.message_id"
        );
        let mut rows = conn
            .query(&sql, ())
            .await
            .map_err(|error| db_error("inspect_collisions", error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| db_error("inspect_collisions", error))?
            .ok_or_else(|| db_message("inspect_collisions", "collision query returned no row"))?;
        let count = |index| -> Result<u64> {
            let value = row
                .get::<i64>(index)
                .map_err(|error| db_error("inspect_collisions", error))?;
            u64::try_from(value).map_err(|error| db_error("inspect_collisions", error))
        };
        LcmMessageCollisionCounts {
            overlaps: count(0)?,
            divergent: count(1)?,
            session_ids: count(2)?,
            content_hashes: count(3)?,
            storage_kinds: count(4)?,
            payload_refs: count(5)?,
        }
    } else {
        LcmMessageCollisionCounts::default()
    };
    conn.execute("DETACH DATABASE other", ())
        .await
        .map_err(|error| db_error("inspect_collisions", error))?;
    Ok(counts)
}

fn read_snapshot<'a>(
    snapshots: &'a crate::sqlite_read_snapshot::SnapshotSet,
    path: &Path,
) -> Result<&'a crate::sqlite_read_snapshot::SnapshotDatabase> {
    snapshots
        .get(path)
        .map_err(|error| db_error("read_snapshot", error))
}

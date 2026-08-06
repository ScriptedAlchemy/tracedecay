use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};

use super::{GitCorrelationError, GitHistoryProgressKey};

pub(in super::super) const MAX_STAGED_PAGE_ROWS: usize = 128;

pub(super) async fn install_schema(
    conn: &(impl Executor + ?Sized),
) -> Result<(), GitCorrelationError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS git_history_index_staged_spans (
            source_rowid INTEGER NOT NULL,
            segment_ordinal INTEGER NOT NULL CHECK(segment_ordinal >= 0),
            boundary INTEGER NOT NULL CHECK(boundary IN (0, 1)),
            branch TEXT,
            timestamp INTEGER NOT NULL,
            PRIMARY KEY(source_rowid, segment_ordinal, boundary),
            FOREIGN KEY(source_rowid, segment_ordinal)
                REFERENCES git_history_index_segments(source_rowid, ordinal)
                ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS git_history_index_staged_commits (
            source_rowid INTEGER NOT NULL,
            segment_ordinal INTEGER NOT NULL CHECK(segment_ordinal >= 0),
            oid TEXT NOT NULL,
            branch TEXT,
            committed_at INTEGER NOT NULL,
            PRIMARY KEY(source_rowid, segment_ordinal, oid),
            FOREIGN KEY(source_rowid, segment_ordinal)
                REFERENCES git_history_index_segments(source_rowid, ordinal)
                ON DELETE CASCADE
        );",
    )
    .await?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct GitHistoryStagedSpanRow {
    pub key: GitHistoryProgressKey,
    pub segment_ordinal: u64,
    pub boundary: u8,
    pub branch: Option<String>,
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct GitHistoryStagedCommitRow {
    pub key: GitHistoryProgressKey,
    pub segment_ordinal: u64,
    pub oid: String,
    pub branch: Option<String>,
    pub committed_at: i64,
}

pub(in super::super) async fn upsert_staged_span(
    conn: &(impl Executor + ?Sized),
    span: &GitHistoryStagedSpanRow,
) -> Result<bool, GitCorrelationError> {
    let boundary = checked_boundary(span.boundary)?;
    let changed = conn
        .execute(
            "INSERT INTO git_history_index_staged_spans (
                source_rowid, segment_ordinal, boundary, branch, timestamp
             )
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source_rowid, segment_ordinal, boundary) DO UPDATE SET
                branch = excluded.branch,
                timestamp = excluded.timestamp
             WHERE git_history_index_staged_spans.branch IS excluded.branch
               AND git_history_index_staged_spans.timestamp = excluded.timestamp",
            params![
                span.key.source_rowid,
                span.segment_ordinal,
                boundary,
                span.branch.as_deref(),
                span.timestamp,
            ],
        )
        .await?;
    Ok(changed == 1)
}

pub(in super::super) async fn upsert_staged_commit(
    conn: &(impl Executor + ?Sized),
    commit: &GitHistoryStagedCommitRow,
) -> Result<bool, GitCorrelationError> {
    let changed = conn
        .execute(
            "INSERT INTO git_history_index_staged_commits (
                source_rowid, segment_ordinal, oid, branch, committed_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source_rowid, segment_ordinal, oid) DO UPDATE SET
                branch = excluded.branch,
                committed_at = excluded.committed_at
             WHERE git_history_index_staged_commits.branch IS excluded.branch
               AND git_history_index_staged_commits.committed_at = excluded.committed_at",
            params![
                commit.key.source_rowid,
                commit.segment_ordinal,
                &commit.oid,
                commit.branch.as_deref(),
                commit.committed_at,
            ],
        )
        .await?;
    Ok(changed == 1)
}

pub(in super::super) async fn read_staged_span_page(
    conn: &(impl QueryExecutor + ?Sized),
    key: GitHistoryProgressKey,
    limit: usize,
) -> Result<Vec<GitHistoryStagedSpanRow>, GitCorrelationError> {
    let limit = checked_limit(limit)?;
    let mut rows = conn
        .query(
            "SELECT source_rowid, segment_ordinal, boundary, branch, timestamp
               FROM git_history_index_staged_spans
              WHERE source_rowid = ?1
              ORDER BY segment_ordinal ASC, boundary ASC
              LIMIT ?2",
            params![key.source_rowid, limit],
        )
        .await?;
    let mut spans = Vec::new();
    while let Some(row) = rows.next().await? {
        spans.push(staged_span_from_row(&row)?);
    }
    Ok(spans)
}

pub(in super::super) async fn read_staged_commit_page(
    conn: &(impl QueryExecutor + ?Sized),
    key: GitHistoryProgressKey,
    limit: usize,
) -> Result<Vec<GitHistoryStagedCommitRow>, GitCorrelationError> {
    let limit = checked_limit(limit)?;
    let mut rows = conn
        .query(
            "SELECT source_rowid, segment_ordinal, oid, branch, committed_at
               FROM git_history_index_staged_commits
              WHERE source_rowid = ?1
              ORDER BY segment_ordinal ASC, oid ASC
              LIMIT ?2",
            params![key.source_rowid, limit],
        )
        .await?;
    let mut commits = Vec::new();
    while let Some(row) = rows.next().await? {
        commits.push(staged_commit_from_row(&row)?);
    }
    Ok(commits)
}

pub(in super::super) async fn delete_staged_span(
    conn: &(impl Executor + ?Sized),
    span: &GitHistoryStagedSpanRow,
) -> Result<bool, GitCorrelationError> {
    let boundary = checked_boundary(span.boundary)?;
    Ok(conn
        .execute(
            "DELETE FROM git_history_index_staged_spans
              WHERE source_rowid = ?1
                AND segment_ordinal = ?2
                AND boundary = ?3
                AND branch IS ?4
                AND timestamp = ?5",
            params![
                span.key.source_rowid,
                span.segment_ordinal,
                boundary,
                span.branch.as_deref(),
                span.timestamp,
            ],
        )
        .await?
        == 1)
}

pub(in super::super) async fn delete_staged_commit(
    conn: &(impl Executor + ?Sized),
    commit: &GitHistoryStagedCommitRow,
) -> Result<bool, GitCorrelationError> {
    Ok(conn
        .execute(
            "DELETE FROM git_history_index_staged_commits
              WHERE source_rowid = ?1
                AND segment_ordinal = ?2
                AND oid = ?3
                AND branch IS ?4
                AND committed_at = ?5",
            params![
                commit.key.source_rowid,
                commit.segment_ordinal,
                &commit.oid,
                commit.branch.as_deref(),
                commit.committed_at,
            ],
        )
        .await?
        == 1)
}

fn checked_limit(limit: usize) -> Result<i64, GitCorrelationError> {
    if !(1..=MAX_STAGED_PAGE_ROWS).contains(&limit) {
        return Err(GitCorrelationError::InvalidArgument(format!(
            "git history staged page limit must be between 1 and {MAX_STAGED_PAGE_ROWS}"
        )));
    }
    i64::try_from(limit).map_err(|_| {
        GitCorrelationError::InvalidArgument(
            "git history staged page limit is too large".to_string(),
        )
    })
}

fn checked_boundary(boundary: u8) -> Result<i64, GitCorrelationError> {
    match boundary {
        0 | 1 => Ok(i64::from(boundary)),
        other => Err(GitCorrelationError::InvalidArgument(format!(
            "git history staged span boundary must be 0 or 1, got {other}"
        ))),
    }
}

fn staged_span_from_row(row: &Row) -> Result<GitHistoryStagedSpanRow, GitCorrelationError> {
    let stored_boundary: i64 = row.get(2)?;
    let boundary = u8::try_from(stored_boundary).map_err(|_| {
        GitCorrelationError::Db(format!(
            "invalid git history staged span boundary `{stored_boundary}`"
        ))
    })?;
    if boundary > 1 {
        return Err(GitCorrelationError::Db(format!(
            "invalid git history staged span boundary `{stored_boundary}`"
        )));
    }
    Ok(GitHistoryStagedSpanRow {
        key: GitHistoryProgressKey {
            source_rowid: row.get(0)?,
        },
        segment_ordinal: row.get(1)?,
        boundary,
        branch: row.get(3)?,
        timestamp: row.get(4)?,
    })
}

fn staged_commit_from_row(row: &Row) -> Result<GitHistoryStagedCommitRow, GitCorrelationError> {
    Ok(GitHistoryStagedCommitRow {
        key: GitHistoryProgressKey {
            source_rowid: row.get(0)?,
        },
        segment_ordinal: row.get(1)?,
        oid: row.get(2)?,
        branch: row.get(3)?,
        committed_at: row.get(4)?,
    })
}

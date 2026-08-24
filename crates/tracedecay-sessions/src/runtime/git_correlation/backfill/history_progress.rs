use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};

use super::GitCorrelationError;

mod staged;
pub(super) use staged::{
    GitHistoryStagedCommitRow, GitHistoryStagedSpanRow, MAX_STAGED_PAGE_ROWS, delete_staged_commit,
    delete_staged_span, read_staged_commit_page, read_staged_span_page, upsert_staged_commit,
    upsert_staged_span,
};

const MAX_CONSULTED_REF_SEAL_JSON_BYTES: usize = 256 * 1024;
pub(super) const MAX_PENDING_PAGE_ROWS: usize = 128;
const INITIAL_REFLOG_CONTENT_CHAIN: &str =
    "sha256:ada855f318c248e40b2bb191bbe42fad3ec6300cc470ecca8d2e2322a6d82ae3";

pub(super) const fn initial_reflog_content_chain() -> &'static str {
    INITIAL_REFLOG_CONTENT_CHAIN
}

pub(in super::super) async fn install_final_schema(
    conn: &(impl Executor + ?Sized),
) -> Result<(), GitCorrelationError> {
    let schema = format!(
        r#"CREATE TABLE IF NOT EXISTS git_history_index_progress (
            activity_timestamp INTEGER NOT NULL,
            source_rowid INTEGER NOT NULL PRIMARY KEY,
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_path TEXT NOT NULL,
            window_start INTEGER NOT NULL,
            window_end INTEGER NOT NULL,
            worktree BLOB NOT NULL,
            worktree_identity BLOB NOT NULL,
            git_dir BLOB NOT NULL,
            git_dir_identity BLOB NOT NULL,
            common_dir BLOB NOT NULL,
            common_dir_identity BLOB NOT NULL,
            generation INTEGER NOT NULL CHECK(generation >= 0),
            scan_mode TEXT NOT NULL
                CHECK(scan_mode IN (
                    'reflog_capture', 'reflog_verify', 'graph', 'publish_verify', 'publish'
                )),
            reflog_path BLOB NOT NULL,
            reflog_byte_offset INTEGER NOT NULL CHECK(reflog_byte_offset >= 0),
            reflog_byte_length INTEGER NOT NULL CHECK(reflog_byte_length >= 0),
            source_generation TEXT NOT NULL,
            reflog_digest TEXT NOT NULL,
            capture_target_offset INTEGER CHECK(capture_target_offset >= 0),
            verify_byte_offset INTEGER NOT NULL CHECK(verify_byte_offset >= 0),
            verify_digest TEXT NOT NULL,
            source_head_referent BLOB,
            source_head_oid TEXT NOT NULL,
            cursor_head_state TEXT NOT NULL
                CHECK(cursor_head_state IN ('local_branch', 'detached')),
            cursor_head_branch TEXT,
            cursor_oid TEXT NOT NULL,
            segment_end INTEGER NOT NULL,
            segment_tip_oid TEXT NOT NULL,
            segment_cursor INTEGER NOT NULL CHECK(segment_cursor >= 0),
            emitted_count INTEGER NOT NULL CHECK(emitted_count >= 0),
            consulted_ref_seal_json TEXT NOT NULL
                CHECK(length(consulted_ref_seal_json) <= {max_ref_seal_bytes}),
            CHECK(window_start <= window_end),
            CHECK(reflog_byte_offset <= reflog_byte_length),
            CHECK(capture_target_offset IS NULL OR capture_target_offset <= reflog_byte_length),
            CHECK(verify_byte_offset <= reflog_byte_length),
            CHECK(segment_end BETWEEN window_start AND window_end),
            CHECK(length(reflog_digest) > 0 AND length(verify_digest) > 0),
            CHECK(
                (
                    scan_mode = 'reflog_capture'
                    AND capture_target_offset IS NULL
                    AND verify_byte_offset = reflog_byte_length
                    AND verify_digest = '{initial}'
                    AND emitted_count = 0
                )
                OR
                (
                    scan_mode = 'reflog_verify'
                    AND capture_target_offset IS NOT NULL
                    AND reflog_byte_offset = capture_target_offset
                    AND verify_byte_offset >= capture_target_offset
                    AND emitted_count = 0
                )
                OR
                (
                    scan_mode IN ('graph', 'publish_verify', 'publish')
                    AND capture_target_offset IS NOT NULL
                    AND reflog_byte_offset = capture_target_offset
                    AND verify_byte_offset = capture_target_offset
                    AND verify_digest = reflog_digest
                )
            ),
            CHECK(
                (cursor_head_state = 'local_branch' AND cursor_head_branch IS NOT NULL)
                OR
                (cursor_head_state = 'detached' AND cursor_head_branch IS NULL)
            )
        );
        CREATE TABLE IF NOT EXISTS git_history_index_segments (
            source_rowid INTEGER NOT NULL,
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            branch TEXT,
            start_ts INTEGER NOT NULL,
            end_ts INTEGER NOT NULL,
            tip_oid TEXT NOT NULL,
            applied INTEGER NOT NULL DEFAULT 0 CHECK(applied IN (0, 1)),
            completed INTEGER NOT NULL DEFAULT 0 CHECK(completed IN (0, 1)),
            PRIMARY KEY(source_rowid, ordinal),
            FOREIGN KEY(source_rowid)
                REFERENCES git_history_index_progress(source_rowid)
                ON DELETE CASCADE,
            CHECK(start_ts <= end_ts),
            CHECK(completed = 0 OR applied = 1)
        );
        CREATE TABLE IF NOT EXISTS git_history_index_pending (
            source_rowid INTEGER NOT NULL,
            segment_ordinal INTEGER NOT NULL CHECK(segment_ordinal >= 0),
            oid TEXT NOT NULL,
            PRIMARY KEY(source_rowid, segment_ordinal, oid),
            FOREIGN KEY(source_rowid, segment_ordinal)
                REFERENCES git_history_index_segments(source_rowid, ordinal)
                ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS git_history_index_seen (
            source_rowid INTEGER NOT NULL,
            segment_ordinal INTEGER NOT NULL CHECK(segment_ordinal >= 0),
            oid TEXT NOT NULL,
            PRIMARY KEY(source_rowid, segment_ordinal, oid),
            FOREIGN KEY(source_rowid, segment_ordinal)
                REFERENCES git_history_index_segments(source_rowid, ordinal)
                ON DELETE CASCADE
        );"#,
        initial = INITIAL_REFLOG_CONTENT_CHAIN,
        max_ref_seal_bytes = MAX_CONSULTED_REF_SEAL_JSON_BYTES,
    );
    conn.execute_batch(&schema).await?;
    staged::install_schema(conn).await?;
    Ok(())
}

/// Exact session-activity row whose native history scan is in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct GitHistoryProgressKey {
    pub source_rowid: i64,
}

/// Durable native-history scan stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitHistoryScanMode {
    ReflogCapture,
    ReflogVerify,
    Graph,
    PublishVerify,
    Publish,
}

impl GitHistoryScanMode {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ReflogCapture => "reflog_capture",
            Self::ReflogVerify => "reflog_verify",
            Self::Graph => "graph",
            Self::PublishVerify => "publish_verify",
            Self::Publish => "publish",
        }
    }

    fn from_db(value: &str) -> Result<Self, GitCorrelationError> {
        match value {
            "reflog_capture" => Ok(Self::ReflogCapture),
            "reflog_verify" => Ok(Self::ReflogVerify),
            "graph" => Ok(Self::Graph),
            "publish_verify" => Ok(Self::PublishVerify),
            "publish" => Ok(Self::Publish),
            other => Err(invalid_stored_value("scan_mode", other)),
        }
    }
}

/// Branch interpretation at the mutable reverse-reflog cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitHistoryCursorHeadState {
    LocalBranch,
    Detached,
}

impl GitHistoryCursorHeadState {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::LocalBranch => "local_branch",
            Self::Detached => "detached",
        }
    }

    fn from_db(value: &str) -> Result<Self, GitCorrelationError> {
        match value {
            "local_branch" => Ok(Self::LocalBranch),
            "detached" => Ok(Self::Detached),
            other => Err(invalid_stored_value("cursor_head_state", other)),
        }
    }
}

/// Durable source seal and mutable cursor for one native history scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitHistoryProgressRow {
    pub key: GitHistoryProgressKey,
    pub activity_timestamp: i64,
    pub provider: String,
    pub session_id: String,
    pub project_path: String,
    pub window_start: i64,
    pub window_end: i64,
    pub worktree: Vec<u8>,
    pub worktree_identity: Vec<u8>,
    pub git_dir: Vec<u8>,
    pub git_dir_identity: Vec<u8>,
    pub common_dir: Vec<u8>,
    pub common_dir_identity: Vec<u8>,
    pub generation: u64,
    pub scan_mode: GitHistoryScanMode,
    pub reflog_path: Vec<u8>,
    pub reflog_byte_offset: u64,
    pub reflog_byte_length: u64,
    pub source_generation: String,
    pub reflog_digest: String,
    pub capture_target_offset: Option<u64>,
    pub verify_byte_offset: u64,
    pub verify_digest: String,
    pub source_head_referent: Option<Vec<u8>>,
    pub source_head_oid: String,
    pub cursor_head_state: GitHistoryCursorHeadState,
    pub cursor_head_branch: Option<String>,
    pub cursor_oid: String,
    pub segment_end: i64,
    pub segment_tip_oid: String,
    pub segment_cursor: u64,
    pub emitted_count: u64,
    pub consulted_refs: BTreeMap<Vec<u8>, Option<String>>,
}

/// One immutable reflog-derived segment and its independent apply/scan state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitHistorySegmentRow {
    pub key: GitHistoryProgressKey,
    pub ordinal: u64,
    pub branch: Option<String>,
    pub start_ts: i64,
    pub end_ts: i64,
    pub tip_oid: String,
    pub applied: bool,
    pub completed: bool,
}

/// One normalized commit-graph frontier entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitHistoryPendingRow {
    pub key: GitHistoryProgressKey,
    pub segment_ordinal: u64,
    pub oid: String,
}

/// One commit already visited while walking a segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitHistorySeenRow {
    pub key: GitHistoryProgressKey,
    pub segment_ordinal: u64,
    pub oid: String,
}

pub(super) async fn read_progress(
    conn: &(impl QueryExecutor + ?Sized),
    key: GitHistoryProgressKey,
) -> Result<Option<GitHistoryProgressRow>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT activity_timestamp, source_rowid, provider, session_id,
                    project_path, window_start, window_end, worktree, worktree_identity,
                    git_dir, git_dir_identity, common_dir, common_dir_identity, generation,
                    scan_mode, reflog_path, reflog_byte_offset, reflog_byte_length,
                    source_generation, reflog_digest, capture_target_offset,
                    verify_byte_offset, verify_digest, source_head_referent, source_head_oid,
                    cursor_head_state, cursor_head_branch, cursor_oid, segment_end,
                    segment_tip_oid, segment_cursor, emitted_count, consulted_ref_seal_json
               FROM git_history_index_progress
              WHERE source_rowid = ?1",
            key_params(key),
        )
        .await?;
    rows.next()
        .await?
        .map(|row| progress_from_row(&row))
        .transpose()
}

pub(super) async fn read_oldest_progress(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<Option<GitHistoryProgressRow>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT activity_timestamp, source_rowid, provider, session_id,
                    project_path, window_start, window_end, worktree, worktree_identity,
                    git_dir, git_dir_identity, common_dir, common_dir_identity, generation,
                    scan_mode, reflog_path, reflog_byte_offset, reflog_byte_length,
                    source_generation, reflog_digest, capture_target_offset,
                    verify_byte_offset, verify_digest, source_head_referent, source_head_oid,
                    cursor_head_state, cursor_head_branch, cursor_oid, segment_end,
                    segment_tip_oid, segment_cursor, emitted_count, consulted_ref_seal_json
               FROM git_history_index_progress
              ORDER BY activity_timestamp ASC, source_rowid ASC
              LIMIT 1",
            (),
        )
        .await?;
    rows.next()
        .await?
        .map(|row| progress_from_row(&row))
        .transpose()
}

/// Inserts a new exact progress row without replacing an existing source seal.
pub(super) async fn insert_progress(
    conn: &(impl Executor + ?Sized),
    progress: &GitHistoryProgressRow,
) -> Result<bool, GitCorrelationError> {
    validate_progress(progress)?;
    let consulted_ref_seal_json = encode_consulted_refs(&progress.consulted_refs)?;
    let changed = conn
        .execute(
            "INSERT INTO git_history_index_progress (
                    activity_timestamp, source_rowid, provider, session_id,
                    project_path, window_start, window_end, worktree, worktree_identity,
                    git_dir, git_dir_identity, common_dir, common_dir_identity, generation,
                    scan_mode, reflog_path, reflog_byte_offset, reflog_byte_length,
                    source_generation, reflog_digest, capture_target_offset,
                    verify_byte_offset, verify_digest, source_head_referent, source_head_oid,
                    cursor_head_state, cursor_head_branch, cursor_oid, segment_end,
                    segment_tip_oid, segment_cursor, emitted_count, consulted_ref_seal_json
                 )
                 VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                    ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                    ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33
                 )
                 ON CONFLICT(source_rowid) DO NOTHING",
            progress_params(progress, &consulted_ref_seal_json),
        )
        .await?;
    Ok(changed == 1)
}

/// Advances only mutable cursor fields when both generation and source seal match.
pub(super) async fn compare_and_swap_progress(
    conn: &(impl Executor + ?Sized),
    expected_generation: u64,
    next: &GitHistoryProgressRow,
) -> Result<bool, GitCorrelationError> {
    validate_progress(next)?;
    let consulted_ref_seal_json = encode_consulted_refs(&next.consulted_refs)?;
    let required_generation = expected_generation.checked_add(1).ok_or_else(|| {
        GitCorrelationError::InvalidArgument("git history generation overflow".to_string())
    })?;
    if next.generation != required_generation {
        return Err(GitCorrelationError::InvalidArgument(format!(
            "next git history generation must be {required_generation}, got {}",
            next.generation
        )));
    }
    let changed = conn
        .execute(
            "UPDATE git_history_index_progress
                SET generation = ?1,
                    scan_mode = ?2,
                    reflog_byte_offset = ?3,
                    reflog_digest = ?4,
                    capture_target_offset = ?5,
                    verify_byte_offset = ?6,
                    verify_digest = ?7,
                    cursor_head_state = ?8,
                    cursor_head_branch = ?9,
                    cursor_oid = ?10,
                    segment_end = ?11,
                    segment_tip_oid = ?12,
                    segment_cursor = ?13,
                    emitted_count = ?14,
                    consulted_ref_seal_json = ?15
              WHERE activity_timestamp = ?16
                AND source_rowid = ?17
                AND generation = ?18
                AND provider = ?19
                AND session_id = ?20
                AND project_path = ?21
                AND window_start = ?22
                AND window_end = ?23
                AND worktree = ?24
                AND reflog_path = ?25
                AND reflog_byte_length = ?26
                AND source_generation = ?27
                AND source_head_referent IS ?28
                AND source_head_oid = ?29
                AND worktree_identity = ?30
                AND git_dir = ?31
                AND git_dir_identity = ?32
                AND common_dir = ?33
                AND common_dir_identity = ?34
                AND (
                    (scan_mode = 'reflog_capture'
                        AND ?2 IN ('reflog_capture', 'reflog_verify')
                        AND ?3 <= reflog_byte_offset
                        AND ?13 >= segment_cursor
                        AND ?14 = 0)
                    OR
                    (scan_mode = 'reflog_verify'
                        AND ?2 IN ('reflog_verify', 'graph')
                        AND ?6 <= verify_byte_offset
                        AND capture_target_offset IS ?5
                        AND reflog_digest = ?4
                        AND consulted_ref_seal_json = ?15
                        AND cursor_head_state = ?8
                        AND cursor_head_branch IS ?9
                        AND cursor_oid = ?10
                        AND segment_end = ?11
                        AND segment_tip_oid = ?12
                        AND segment_cursor = ?13
                        AND emitted_count = ?14)
                    OR
                    (scan_mode = 'graph'
                        AND ?2 IN ('graph', 'publish_verify')
                        AND capture_target_offset IS ?5
                        AND reflog_digest = ?4
                        AND consulted_ref_seal_json = ?15
                        AND cursor_head_state = ?8
                        AND cursor_head_branch IS ?9
                        AND cursor_oid = ?10
                        AND segment_end = ?11
                        AND segment_tip_oid = ?12
                        AND ?13 >= segment_cursor
                        AND ?14 >= emitted_count)
                    OR
                    (scan_mode = 'publish_verify'
                        AND ?2 IN ('publish_verify', 'publish')
                        AND capture_target_offset IS ?5
                        AND reflog_digest = ?4
                        AND consulted_ref_seal_json = ?15
                        AND cursor_head_state = ?8
                        AND cursor_head_branch IS ?9
                        AND cursor_oid = ?10
                        AND segment_end = ?11
                        AND segment_tip_oid = ?12
                        AND segment_cursor = ?13
                        AND emitted_count = ?14)
                    OR
                    (scan_mode = 'publish'
                        AND ?2 = 'publish'
                        AND capture_target_offset IS ?5
                        AND reflog_digest = ?4
                        AND consulted_ref_seal_json = ?15
                        AND cursor_head_state = ?8
                        AND cursor_head_branch IS ?9
                        AND cursor_oid = ?10
                        AND segment_end = ?11
                        AND segment_tip_oid = ?12
                        AND segment_cursor = ?13
                        AND emitted_count = ?14)
                )",
            params![
                next.generation,
                next.scan_mode.as_str(),
                next.reflog_byte_offset,
                &next.reflog_digest,
                next.capture_target_offset,
                next.verify_byte_offset,
                &next.verify_digest,
                next.cursor_head_state.as_str(),
                next.cursor_head_branch.as_deref(),
                &next.cursor_oid,
                next.segment_end,
                &next.segment_tip_oid,
                next.segment_cursor,
                next.emitted_count,
                &consulted_ref_seal_json,
                next.activity_timestamp,
                next.key.source_rowid,
                expected_generation,
                &next.provider,
                &next.session_id,
                &next.project_path,
                next.window_start,
                next.window_end,
                &next.worktree,
                &next.reflog_path,
                next.reflog_byte_length,
                &next.source_generation,
                next.source_head_referent.as_deref(),
                &next.source_head_oid,
                &next.worktree_identity,
                &next.git_dir,
                &next.git_dir_identity,
                &next.common_dir,
                &next.common_dir_identity,
            ],
        )
        .await?;
    Ok(changed == 1)
}

/// Deletes exactly one progress key; declared foreign keys cascade its graph state.
pub(super) async fn reset_progress(
    conn: &(impl Executor + ?Sized),
    key: GitHistoryProgressKey,
) -> Result<bool, GitCorrelationError> {
    Ok(conn
        .execute(
            "DELETE FROM git_history_index_progress
              WHERE source_rowid = ?1",
            key_params(key),
        )
        .await?
        == 1)
}

pub(super) async fn read_segment(
    conn: &(impl QueryExecutor + ?Sized),
    key: GitHistoryProgressKey,
    ordinal: u64,
) -> Result<Option<GitHistorySegmentRow>, GitCorrelationError> {
    let mut rows = conn
        .query(
            "SELECT source_rowid, ordinal, branch,
                    start_ts, end_ts, tip_oid, applied, completed
               FROM git_history_index_segments
              WHERE source_rowid = ?1
                AND ordinal = ?2",
            params![key.source_rowid, ordinal],
        )
        .await?;
    rows.next()
        .await?
        .map(|row| segment_from_row(&row))
        .transpose()
}

/// Inserts a segment or updates only its mutable flags when its sealed shape matches.
pub(super) async fn upsert_segment(
    conn: &(impl Executor + ?Sized),
    segment: &GitHistorySegmentRow,
) -> Result<bool, GitCorrelationError> {
    let changed = conn
        .execute(
            "INSERT INTO git_history_index_segments (
                    source_rowid, ordinal, branch,
                    start_ts, end_ts, tip_oid, applied, completed
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(source_rowid, ordinal) DO UPDATE SET
                    applied = excluded.applied,
                    completed = excluded.completed
                 WHERE git_history_index_segments.branch IS excluded.branch
                   AND git_history_index_segments.start_ts = excluded.start_ts
                   AND git_history_index_segments.end_ts = excluded.end_ts
                   AND git_history_index_segments.tip_oid = excluded.tip_oid",
            params![
                segment.key.source_rowid,
                segment.ordinal,
                segment.branch.as_deref(),
                segment.start_ts,
                segment.end_ts,
                &segment.tip_oid,
                bool_value(segment.applied),
                bool_value(segment.completed),
            ],
        )
        .await?;
    Ok(changed == 1)
}

pub(super) async fn read_pending_page(
    conn: &(impl QueryExecutor + ?Sized),
    key: GitHistoryProgressKey,
    segment_ordinal: u64,
    limit: usize,
) -> Result<Vec<GitHistoryPendingRow>, GitCorrelationError> {
    if !(1..=MAX_PENDING_PAGE_ROWS).contains(&limit) {
        return Err(GitCorrelationError::InvalidArgument(format!(
            "git history pending page limit must be between 1 and {MAX_PENDING_PAGE_ROWS}"
        )));
    }
    let limit = i64::try_from(limit).map_err(|_| {
        GitCorrelationError::InvalidArgument(
            "git history pending page limit is too large".to_string(),
        )
    })?;
    let mut rows = conn
        .query(
            "SELECT source_rowid, segment_ordinal, oid
               FROM git_history_index_pending
              WHERE source_rowid = ?1
                AND segment_ordinal = ?2
              ORDER BY oid ASC
              LIMIT ?3",
            params![key.source_rowid, segment_ordinal, limit],
        )
        .await?;
    let mut pending = Vec::new();
    while let Some(row) = rows.next().await? {
        pending.push(pending_from_row(&row)?);
    }
    Ok(pending)
}

pub(super) async fn upsert_pending(
    conn: &(impl Executor + ?Sized),
    pending: &GitHistoryPendingRow,
) -> Result<bool, GitCorrelationError> {
    let changed = conn
        .execute(
            "INSERT INTO git_history_index_pending (
                source_rowid, segment_ordinal, oid
             )
             SELECT ?1, ?2, ?3
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM git_history_index_seen
                     WHERE source_rowid = ?1
                       AND segment_ordinal = ?2
                       AND oid = ?3
              )
             ON CONFLICT(source_rowid, segment_ordinal, oid)
             DO NOTHING",
            params![
                pending.key.source_rowid,
                pending.segment_ordinal,
                &pending.oid,
            ],
        )
        .await?;
    Ok(changed == 1)
}

pub(super) async fn delete_pending(
    conn: &(impl Executor + ?Sized),
    key: GitHistoryProgressKey,
    segment_ordinal: u64,
    oid: &str,
) -> Result<bool, GitCorrelationError> {
    Ok(conn
        .execute(
            "DELETE FROM git_history_index_pending
              WHERE source_rowid = ?1
                AND segment_ordinal = ?2
                AND oid = ?3",
            params![key.source_rowid, segment_ordinal, oid],
        )
        .await?
        == 1)
}

pub(super) async fn insert_seen(
    conn: &(impl Executor + ?Sized),
    seen: &GitHistorySeenRow,
) -> Result<bool, GitCorrelationError> {
    Ok(conn
        .execute(
            "INSERT INTO git_history_index_seen (
                    source_rowid, segment_ordinal, oid
                 )
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(source_rowid, segment_ordinal, oid)
                 DO NOTHING",
            params![seen.key.source_rowid, seen.segment_ordinal, &seen.oid,],
        )
        .await?
        == 1)
}

fn progress_from_row(row: &Row) -> Result<GitHistoryProgressRow, GitCorrelationError> {
    Ok(GitHistoryProgressRow {
        key: GitHistoryProgressKey {
            source_rowid: row.get(1)?,
        },
        activity_timestamp: row.get(0)?,
        provider: row.get(2)?,
        session_id: row.get(3)?,
        project_path: row.get(4)?,
        window_start: row.get(5)?,
        window_end: row.get(6)?,
        worktree: row.get(7)?,
        worktree_identity: row.get(8)?,
        git_dir: row.get(9)?,
        git_dir_identity: row.get(10)?,
        common_dir: row.get(11)?,
        common_dir_identity: row.get(12)?,
        generation: row.get(13)?,
        scan_mode: GitHistoryScanMode::from_db(&row.get::<String>(14)?)?,
        reflog_path: row.get(15)?,
        reflog_byte_offset: row.get(16)?,
        reflog_byte_length: row.get(17)?,
        source_generation: row.get(18)?,
        reflog_digest: row.get(19)?,
        capture_target_offset: row.get(20)?,
        verify_byte_offset: row.get(21)?,
        verify_digest: row.get(22)?,
        source_head_referent: row.get(23)?,
        source_head_oid: row.get(24)?,
        cursor_head_state: GitHistoryCursorHeadState::from_db(&row.get::<String>(25)?)?,
        cursor_head_branch: row.get(26)?,
        cursor_oid: row.get(27)?,
        segment_end: row.get(28)?,
        segment_tip_oid: row.get(29)?,
        segment_cursor: row.get(30)?,
        emitted_count: row.get(31)?,
        consulted_refs: decode_consulted_refs(&row.get::<String>(32)?)?,
    })
}

fn segment_from_row(row: &Row) -> Result<GitHistorySegmentRow, GitCorrelationError> {
    Ok(GitHistorySegmentRow {
        key: GitHistoryProgressKey {
            source_rowid: row.get(0)?,
        },
        ordinal: row.get(1)?,
        branch: row.get(2)?,
        start_ts: row.get(3)?,
        end_ts: row.get(4)?,
        tip_oid: row.get(5)?,
        applied: stored_bool(row.get(6)?, "applied")?,
        completed: stored_bool(row.get(7)?, "completed")?,
    })
}

fn pending_from_row(row: &Row) -> Result<GitHistoryPendingRow, GitCorrelationError> {
    Ok(GitHistoryPendingRow {
        key: GitHistoryProgressKey {
            source_rowid: row.get(0)?,
        },
        segment_ordinal: row.get(1)?,
        oid: row.get(2)?,
    })
}

fn progress_params(
    progress: &GitHistoryProgressRow,
    consulted_ref_seal_json: &str,
) -> impl tracedecay_runtime_core::db::engine::IntoParams {
    params![
        progress.activity_timestamp,
        progress.key.source_rowid,
        &progress.provider,
        &progress.session_id,
        &progress.project_path,
        progress.window_start,
        progress.window_end,
        &progress.worktree,
        &progress.worktree_identity,
        &progress.git_dir,
        &progress.git_dir_identity,
        &progress.common_dir,
        &progress.common_dir_identity,
        progress.generation,
        progress.scan_mode.as_str(),
        &progress.reflog_path,
        progress.reflog_byte_offset,
        progress.reflog_byte_length,
        &progress.source_generation,
        &progress.reflog_digest,
        progress.capture_target_offset,
        progress.verify_byte_offset,
        &progress.verify_digest,
        progress.source_head_referent.as_deref(),
        &progress.source_head_oid,
        progress.cursor_head_state.as_str(),
        progress.cursor_head_branch.as_deref(),
        &progress.cursor_oid,
        progress.segment_end,
        &progress.segment_tip_oid,
        progress.segment_cursor,
        progress.emitted_count,
        consulted_ref_seal_json,
    ]
}

fn key_params(key: GitHistoryProgressKey) -> impl tracedecay_runtime_core::db::engine::IntoParams {
    params![key.source_rowid]
}

fn validate_progress(progress: &GitHistoryProgressRow) -> Result<(), GitCorrelationError> {
    if progress.window_start > progress.window_end
        || !(progress.window_start..=progress.window_end).contains(&progress.segment_end)
        || progress.reflog_byte_offset > progress.reflog_byte_length
        || progress.verify_byte_offset > progress.reflog_byte_length
        || progress.worktree.is_empty()
        || progress.worktree_identity.is_empty()
        || progress.git_dir.is_empty()
        || progress.git_dir_identity.is_empty()
        || progress.common_dir.is_empty()
        || progress.common_dir_identity.is_empty()
        || progress.reflog_path.is_empty()
        || progress.source_generation.is_empty()
        || progress.reflog_digest.is_empty()
        || progress.verify_digest.is_empty()
    {
        return Err(GitCorrelationError::InvalidArgument(
            "git history progress has invalid window or reflog bounds".to_string(),
        ));
    }
    let legal_mode = match (progress.scan_mode, progress.capture_target_offset) {
        (GitHistoryScanMode::ReflogCapture, None) => {
            progress.verify_byte_offset == progress.reflog_byte_length
                && progress.verify_digest == initial_reflog_content_chain()
                && progress.emitted_count == 0
        }
        (GitHistoryScanMode::ReflogVerify, Some(target)) => {
            progress.reflog_byte_offset == target
                && (target..=progress.reflog_byte_length).contains(&progress.verify_byte_offset)
                && progress.emitted_count == 0
        }
        (
            GitHistoryScanMode::Graph
            | GitHistoryScanMode::PublishVerify
            | GitHistoryScanMode::Publish,
            Some(target),
        ) => {
            progress.reflog_byte_offset == target
                && progress.verify_byte_offset == target
                && progress.verify_digest == progress.reflog_digest
        }
        _ => false,
    };
    if !legal_mode {
        return Err(GitCorrelationError::InvalidArgument(
            "git history progress has an impossible capture/verify state".to_string(),
        ));
    }
    match (
        progress.cursor_head_state,
        progress.cursor_head_branch.as_deref(),
    ) {
        (GitHistoryCursorHeadState::LocalBranch, Some(branch)) if !branch.is_empty() => Ok(()),
        (GitHistoryCursorHeadState::Detached, None) => Ok(()),
        _ => Err(GitCorrelationError::InvalidArgument(
            "git history cursor branch does not match its head state".to_string(),
        )),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConsultedRefSealEntry {
    name_hex: String,
    oid: Option<String>,
}

fn encode_consulted_refs(
    consulted_refs: &BTreeMap<Vec<u8>, Option<String>>,
) -> Result<String, GitCorrelationError> {
    let json = canonical_consulted_refs_json(consulted_refs).map_err(|error| {
        GitCorrelationError::InvalidArgument(format!(
            "could not encode git history consulted-ref seal: {error}"
        ))
    })?;
    if json.len() > MAX_CONSULTED_REF_SEAL_JSON_BYTES {
        return Err(GitCorrelationError::InvalidArgument(format!(
            "git history consulted-ref seal exceeds {MAX_CONSULTED_REF_SEAL_JSON_BYTES} bytes"
        )));
    }
    Ok(json)
}

fn decode_consulted_refs(
    json: &str,
) -> Result<BTreeMap<Vec<u8>, Option<String>>, GitCorrelationError> {
    if json.len() > MAX_CONSULTED_REF_SEAL_JSON_BYTES {
        return Err(GitCorrelationError::Db(format!(
            "git history consulted-ref seal exceeds {MAX_CONSULTED_REF_SEAL_JSON_BYTES} bytes"
        )));
    }
    let entries: Vec<ConsultedRefSealEntry> = serde_json::from_str(json).map_err(|error| {
        GitCorrelationError::Db(format!("invalid git history consulted-ref seal: {error}"))
    })?;
    let mut consulted_refs = BTreeMap::new();
    for entry in entries {
        let name = hex::decode(&entry.name_hex).map_err(|error| {
            GitCorrelationError::Db(format!(
                "invalid git history consulted-ref name hex: {error}"
            ))
        })?;
        if hex::encode(&name) != entry.name_hex {
            return Err(GitCorrelationError::Db(
                "git history consulted-ref name hex is not canonical".to_string(),
            ));
        }
        if consulted_refs.insert(name, entry.oid).is_some() {
            return Err(GitCorrelationError::Db(
                "git history consulted-ref seal contains a duplicate name".to_string(),
            ));
        }
    }
    let canonical = canonical_consulted_refs_json(&consulted_refs).map_err(|error| {
        GitCorrelationError::Db(format!(
            "could not canonicalize git history consulted-ref seal: {error}"
        ))
    })?;
    if canonical != json {
        return Err(GitCorrelationError::Db(
            "git history consulted-ref seal is not canonical JSON".to_string(),
        ));
    }
    Ok(consulted_refs)
}

fn canonical_consulted_refs_json(
    consulted_refs: &BTreeMap<Vec<u8>, Option<String>>,
) -> serde_json::Result<String> {
    serde_json::to_string(
        &consulted_refs
            .iter()
            .map(|(name, oid)| ConsultedRefSealEntry {
                name_hex: hex::encode(name),
                oid: oid.clone(),
            })
            .collect::<Vec<_>>(),
    )
}

const fn bool_value(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn stored_bool(value: i64, column: &str) -> Result<bool, GitCorrelationError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(invalid_stored_value(column, &other.to_string())),
    }
}

fn invalid_stored_value(column: &str, value: &str) -> GitCorrelationError {
    GitCorrelationError::Db(format!(
        "invalid git history index {column} value `{value}`"
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
